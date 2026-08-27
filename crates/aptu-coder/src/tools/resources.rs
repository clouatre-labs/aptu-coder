// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! MCP Resource surface for the structural knowledge graph.
//!
//! Implements `list_resources`, `list_resource_templates`, and `read_resource`
//! for the `aptu-coder://graph/{repo_hash}/{query_type}/{arg}?cursor=...&max_nodes=...&depth=...&format=...` URI scheme.
//! Three resource templates are advertised: blast-radius, subgraph, blast-radius-bidirectional.

use aptu_coder_core::graph::{GraphDiskStore, StructuralGraph};
use aptu_coder_core::pagination::{
    DEFAULT_PAGE_SIZE, PaginationMode, decode_cursor, paginate_slice,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rmcp::RoleServer;
use rmcp::model::{
    CacheScope, ErrorCode, ErrorData, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    ResourceContents, ResourceTemplate,
};
use rmcp::service::RequestContext;
use std::collections::HashMap;

const PAGE_SIZE: usize = 50;

/// Maximum blast-radius BFS depth permitted in caller-supplied resource URIs.
///
/// Set to 5 based on empirical measurement and external practice. On aptu-coder's
/// own codebase (73 files, 1898 graph nodes, 8 representative symbols), depth 5
/// captured 95%+ of the reachable blast radius for every symbol tested, with only
/// 1 marginal node gained at depths 6-8 before full saturation. External practice
/// clusters at 3-5 (graphql depth-limit examples: 3 and 5, ArangoDB Spring Data
/// default: 1). Traversal cost is sub-40us at all depths measured. Default remains 3.
const MAX_GRAPH_DEPTH: usize = 5;

/// Default node cap for bidirectional blast-radius traversal when not supplied in the URI.
const DEFAULT_MAX_NODES: usize = 50;

/// Output format for graph queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    Json,
    Text,
}

/// Graph query variants parsed from resource URIs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GraphQuery {
    BlastRadius {
        repo_hash: String,
        symbol: String,
        depth: usize,
        cursor_offset: usize,
    },
    Subgraph {
        repo_hash: String,
        symbol: String,
        cursor_offset: usize,
    },
    BidirectionalBlastRadius {
        repo_hash: String,
        symbols: Vec<String>,
        max_nodes: usize,
        depth: usize,
        cursor_offset: usize,
    },
}

impl GraphQuery {
    fn repo_hash(&self) -> &str {
        match self {
            Self::BlastRadius { repo_hash, .. } => repo_hash,
            Self::Subgraph { repo_hash, .. } => repo_hash,
            Self::BidirectionalBlastRadius { repo_hash, .. } => repo_hash,
        }
    }

    fn cursor_offset(&self) -> usize {
        match self {
            Self::BlastRadius { cursor_offset, .. } => *cursor_offset,
            Self::Subgraph { cursor_offset, .. } => *cursor_offset,
            Self::BidirectionalBlastRadius { cursor_offset, .. } => *cursor_offset,
        }
    }
}

/// Base64url-encode a graph cursor offset.
///
/// Produces `{"g":N}` JSON encoded as base64url (no padding), which is safe
/// in URI query strings and cannot be mistaken for a `PaginationMode` cursor
/// (which uses `{"mode":...,"offset":...}`).
fn encode_graph_cursor(offset: usize) -> String {
    URL_SAFE_NO_PAD.encode(format!(r#"{{"g":{offset}}}"#).as_bytes())
}

/// Decode a graph cursor token. Returns `None` if the token is not a valid
/// graph cursor (wrong base64, wrong JSON, or missing "g" key).
fn decode_graph_cursor(s: &str) -> Option<usize> {
    let decoded = URL_SAFE_NO_PAD.decode(s.as_bytes()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("g")?.as_u64().map(|n| n as usize)
}

/// Parse a `aptu-coder://graph/{repo_hash}/{query_type}/{arg}?cursor=...&depth=N&max_nodes=M&format=...` URI.
///
/// Validates scheme, path structure, and query type. The `repo_hash` is carried
/// inside the returned `GraphQuery` and used as the `GraphDiskStore` lookup key
/// in `read_resource_impl`; a stale hash causes `get` to return `None` which
/// triggers the cold-cache error.
fn parse_graph_uri(uri: &str) -> Result<(GraphQuery, OutputFormat), ErrorData> {
    let rest = uri.strip_prefix("aptu-coder://").ok_or_else(|| {
        ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            format!("invalid URI scheme: expected aptu-coder://, got {uri}"),
            None,
        )
    })?;

    // Split path from query string.
    let (path_part, qs) = match rest.find('?') {
        Some(pos) => (&rest[..pos], Some(&rest[pos + 1..])),
        None => (rest, None),
    };

    // Parse query-string params: cursor=<token>&depth=<N>&max_nodes=<M>&format=<format>
    let mut cursor_offset: usize = 0;
    let mut depth: usize = 3;
    let mut max_nodes: usize = DEFAULT_MAX_NODES;
    let mut format: OutputFormat = OutputFormat::Json;
    if let Some(qs) = qs {
        for kv in qs.split('&') {
            if let Some(token) = kv.strip_prefix("cursor=") {
                cursor_offset = decode_graph_cursor(token).ok_or_else(|| {
                    ErrorData::new(
                        ErrorCode::INVALID_PARAMS,
                        format!("invalid graph cursor token: {token}"),
                        None,
                    )
                })?;
            } else if let Some(d) = kv.strip_prefix("depth=") {
                depth = d.parse::<usize>().map_err(|_| {
                    ErrorData::new(
                        ErrorCode::INVALID_PARAMS,
                        format!("invalid depth parameter: {d}"),
                        None,
                    )
                })?;
            } else if let Some(m) = kv.strip_prefix("max_nodes=") {
                max_nodes = m.parse::<usize>().map_err(|_| {
                    ErrorData::new(
                        ErrorCode::INVALID_PARAMS,
                        format!("invalid max_nodes parameter: {m}"),
                        None,
                    )
                })?;
            } else if let Some(fmt) = kv.strip_prefix("format=") {
                format = match fmt {
                    "text" => OutputFormat::Text,
                    "json" => OutputFormat::Json,
                    _ => {
                        return Err(ErrorData::new(
                            ErrorCode::INVALID_PARAMS,
                            format!("invalid format parameter: expected text or json, got {fmt}"),
                            None,
                        ));
                    }
                };
            }
        }
    }

    // Path segments: graph/{repo_hash}/{query_type}/{arg}
    let segments: Vec<&str> = path_part.split('/').collect();
    if segments.len() < 4 || segments[0] != "graph" {
        return Err(ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            format!(
                "invalid URI path: expected graph/{{repo_hash}}/{{query_type}}/{{arg}}, got {uri}"
            ),
            None,
        ));
    }

    let repo_hash = segments[1].to_string();
    let query_type = segments[2];
    let arg = segments[3..].join("/");

    match query_type {
        "blast-radius" => {
            if depth > MAX_GRAPH_DEPTH {
                return Err(ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("depth parameter exceeds maximum ({MAX_GRAPH_DEPTH}): got {depth}"),
                    None,
                ));
            }
            Ok((
                GraphQuery::BlastRadius {
                    repo_hash,
                    symbol: arg,
                    depth,
                    cursor_offset,
                },
                format,
            ))
        }
        "subgraph" => Ok((
            GraphQuery::Subgraph {
                repo_hash,
                symbol: arg,
                cursor_offset,
            },
            format,
        )),
        "blast-radius-bidirectional" => {
            let symbols: Vec<String> = arg
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            if symbols.is_empty() {
                return Err(ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    "blast-radius-bidirectional requires at least one non-empty symbol name"
                        .to_string(),
                    None,
                ));
            }
            if depth < 1 {
                return Err(ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    "depth parameter must be at least 1".to_string(),
                    None,
                ));
            }
            if depth > MAX_GRAPH_DEPTH {
                return Err(ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("depth parameter exceeds maximum ({MAX_GRAPH_DEPTH}): got {depth}"),
                    None,
                ));
            }
            if max_nodes < 1 {
                return Err(ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    "max_nodes parameter must be at least 1".to_string(),
                    None,
                ));
            }
            Ok((
                GraphQuery::BidirectionalBlastRadius {
                    repo_hash,
                    symbols,
                    max_nodes,
                    depth,
                    cursor_offset,
                },
                format,
            ))
        }
        _ => Err(ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            format!(
                "unknown query type '{query_type}': expected blast-radius, subgraph, or blast-radius-bidirectional"
            ),
            None,
        )),
    }
}

/// Resolve a query against a graph into a list of nodes and edges.
///
/// Returns a tuple of (nodes, edges) where:
/// - nodes: Vec<serde_json::Value> of node JSON values, in order returned by the query
/// - edges: Vec<(usize, usize, serde_json::Value)> where each tuple is (source_index, target_index, serialized_edge)
///
/// Nodes and edges whose serialization fails are silently dropped rather than failing the
/// entire request; a malformed node or edge in the graph should not prevent the client
/// from receiving the rest of the result set. Edges with endpoints missing from the node
/// list (due to serialization failure) are also dropped.
fn query_to_graph(
    graph: &StructuralGraph,
    query: &GraphQuery,
) -> (
    Vec<serde_json::Value>,
    Vec<(usize, usize, serde_json::Value)>,
) {
    let (node_indices, edge_tuples) = match query {
        GraphQuery::BlastRadius { symbol, depth, .. } => {
            graph.blast_radius_subgraph(symbol, *depth)
        }
        GraphQuery::Subgraph { symbol, .. } => graph.blast_radius_subgraph(symbol, 2),
        GraphQuery::BidirectionalBlastRadius {
            symbols,
            max_nodes,
            depth,
            ..
        } => {
            let symbol_refs: Vec<&str> = symbols.iter().map(|s| s.as_str()).collect();
            let seeds = graph.find_symbols(&symbol_refs);
            graph.blast_radius_bidirectional(&seeds, *max_nodes, *depth)
        }
    };

    // Build a map from NodeIndex to its position in the returned node list. The position
    // must track `nodes.len()` at push time, not the source index into `node_indices`:
    // a dropped (unserializable) node would otherwise leave every later position off by
    // the number of prior drops.
    let mut index_to_position = HashMap::new();
    let mut nodes = Vec::new();
    for idx in &node_indices {
        if let Ok(node_json) = serde_json::to_value(&graph.graph[*idx]) {
            index_to_position.insert(*idx, nodes.len());
            nodes.push(node_json);
        }
    }

    // Filter and re-express edges as (source_position, target_position, serialized_edge)
    let edges: Vec<(usize, usize, serde_json::Value)> = edge_tuples
        .into_iter()
        .filter_map(|(source_idx, target_idx, edge)| {
            let source_pos = index_to_position.get(&source_idx).copied()?;
            let target_pos = index_to_position.get(&target_idx).copied()?;
            let edge_json = serde_json::to_value(&edge).ok()?;
            Some((source_pos, target_pos, edge_json))
        })
        .collect();

    (nodes, edges)
}

/// Decode a list-handler cursor (core base64-STANDARD encoding) to a page offset.
///
/// Distinct from [`decode_graph_cursor`], which uses base64url URL_SAFE_NO_PAD
/// encoding embedded in graph-node URI query strings.
fn cursor_to_offset(params: Option<PaginatedRequestParams>) -> Result<usize, ErrorData> {
    match params.and_then(|p| p.cursor) {
        Some(s) => decode_cursor(&s)
            .map(|c| c.offset)
            .map_err(|e| ErrorData::new(ErrorCode::INVALID_PARAMS, e.to_string(), None)),
        None => Ok(0),
    }
}

/// Return an empty resources list (concrete graph slices are unbounded;
/// clients use templates).
pub(crate) fn list_resources_impl(
    _params: Option<PaginatedRequestParams>,
    _context: &RequestContext<RoleServer>,
) -> Result<ListResourcesResult, ErrorData> {
    Ok(ListResourcesResult::with_all_items(Vec::new())
        .with_ttl_ms(3_600_000)
        .with_cache_scope(CacheScope::Public))
}

/// Return three ResourceTemplate entries for the graph URI scheme.
pub(crate) fn list_resource_templates_impl(
    params: Option<PaginatedRequestParams>,
    _context: &RequestContext<RoleServer>,
) -> Result<ListResourceTemplatesResult, ErrorData> {
    let templates = vec![
        ResourceTemplate::new(
            "aptu-coder://graph/{repo_hash}/blast-radius/{symbol}?depth={depth}",
            "graph-blast-radius",
        )
        .with_description("BFS blast-radius traversal from a symbol (depth 1-5, default 3)")
        .with_mime_type("application/json"),
        ResourceTemplate::new(
            "aptu-coder://graph/{repo_hash}/subgraph/{symbol}",
            "graph-subgraph",
        )
        .with_description("Subgraph centered on a symbol")
        .with_mime_type("application/json"),
        ResourceTemplate::new(
            "aptu-coder://graph/{repo_hash}/blast-radius-bidirectional/{symbols}?max_nodes={max_nodes}&depth={depth}&format={format}",
            "graph-blast-radius-bidirectional",
        )
        .with_description("Bidirectional BFS from one or more comma-separated seed symbols (max_nodes: 1-∞ default 50, depth: 1-5 default 3, format: text or json default json)")
        .with_mime_type("application/json"),
    ];
    let offset = cursor_to_offset(params)?;
    let paginated = paginate_slice(
        &templates,
        offset,
        DEFAULT_PAGE_SIZE,
        PaginationMode::Default,
    )
    .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;
    let mut result = ListResourceTemplatesResult::with_all_items(paginated.items);
    result.next_cursor = paginated.next_cursor;
    Ok(result)
}

/// Read a graph resource identified by URI.
///
/// Parses the URI, loads the graph from disk store, dispatches to the query
/// helper, and either returns text output (if format=text) or paginates
/// and returns JSON, then `ReadResourceResponse::Complete`.
pub(crate) fn read_resource_impl(
    request: ReadResourceRequestParams,
    graph_store: &GraphDiskStore,
) -> Result<ReadResourceResponse, ErrorData> {
    let (query, format) = parse_graph_uri(&request.uri)?;

    let graph = graph_store.get(query.repo_hash()).ok_or_else(|| {
        ErrorData::new(
            ErrorCode::RESOURCE_NOT_FOUND,
            "graph not built yet -- call analyze_symbol on this directory first to build the graph cache".to_string(),
            None,
        )
    })?;

    // Handle text format output separately, bypassing pagination
    if format == OutputFormat::Text {
        // This match intentionally mirrors query_to_graph's match rather than sharing a helper function,
        // because sharing one would require naming petgraph::graph::NodeIndex explicitly and adding petgraph
        // as a new direct dependency of this crate just for a 3-arm match.
        let node_indices = match &query {
            GraphQuery::BlastRadius { symbol, depth, .. } => {
                graph.blast_radius_subgraph(symbol, *depth).0
            }
            GraphQuery::Subgraph { symbol, .. } => graph.blast_radius_subgraph(symbol, 2).0,
            GraphQuery::BidirectionalBlastRadius {
                symbols,
                max_nodes,
                depth,
                ..
            } => {
                let symbol_refs: Vec<&str> = symbols.iter().map(|s| s.as_str()).collect();
                let seeds = graph.find_symbols(&symbol_refs);
                graph
                    .blast_radius_bidirectional(&seeds, *max_nodes, *depth)
                    .0
            }
        };

        let text = graph.render_subgraph_text(&node_indices);
        let contents = ResourceContents::text(text, &request.uri).with_mime_type("text/plain");
        return Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![contents],
        )));
    }

    let (all_nodes, all_edges) = query_to_graph(&graph, &query);
    let total = all_nodes.len();
    let offset = query.cursor_offset();

    let page: Vec<serde_json::Value> = all_nodes
        .into_iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .collect::<Vec<_>>();
    let page_end = offset + page.len();

    // Filter edges to only those whose source and target both fall in [offset, page_end)
    let page_edges: Vec<serde_json::Value> = all_edges
        .into_iter()
        .filter(|(src, tgt, _)| {
            *src >= offset && *src < page_end && *tgt >= offset && *tgt < page_end
        })
        .map(|(src, tgt, edge_json)| {
            serde_json::json!({
                "source": src - offset,
                "target": tgt - offset,
                "kind": edge_json,
            })
        })
        .collect();

    let next_cursor = (page_end < total).then(|| encode_graph_cursor(page_end));

    let payload = serde_json::json!({
        "nodes": page,
        "edges": page_edges,
        "next_cursor": next_cursor,
        "total": total,
    });

    let text = serde_json::to_string(&payload).map_err(|e| {
        ErrorData::new(
            ErrorCode::INTERNAL_ERROR,
            format!("failed to serialize graph payload: {e}"),
            None,
        )
    })?;

    let contents = ResourceContents::text(text, &request.uri).with_mime_type("application/json");
    Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
        vec![contents],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptu_coder_core::analyze::FileAnalysisOutput;
    use aptu_coder_core::types::{CallInfo, FunctionInfo, SemanticAnalysis};

    /// Build a graph with `caller` calling `callee` via a Calls edge.
    /// `bfs_blast_radius` on `caller` at depth >= 1 returns `callee`.
    fn make_graph_with_call(caller: &str, callee: &str) -> StructuralGraph {
        let mut f1 = FunctionInfo::default();
        f1.name = caller.to_string();
        f1.line = 1;
        f1.end_line = 5;
        let mut f2 = FunctionInfo::default();
        f2.name = callee.to_string();
        f2.line = 10;
        f2.end_line = 15;
        // CallInfo is #[non_exhaustive] with no Default or new(); deserialize from JSON.
        let call: CallInfo = serde_json::from_str(&format!(
            r#"{{"caller":"{caller}","callee":"{callee}","line":1,"column":0}}"#
        ))
        .expect("valid call JSON");
        let analysis = SemanticAnalysis::new(
            vec![f1, f2],
            vec![],
            vec![],
            vec![],
            Default::default(),
            vec![call],
            vec![],
        );
        let entry = FileAnalysisOutput::new(
            "test.rs".to_string(),
            "test.rs:1:1:1".to_string(),
            analysis,
            15,
            None,
        );
        StructuralGraph::build_from_analysis(&[entry])
    }

    #[test]
    fn test_parse_graph_uri_blast_radius_happy_path() {
        let uri = "aptu-coder://graph/abc123/blast-radius/my_func";
        let (query, format) = parse_graph_uri(uri).unwrap();
        assert_eq!(
            query,
            GraphQuery::BlastRadius {
                repo_hash: "abc123".to_string(),
                symbol: "my_func".to_string(),
                depth: 3,
                cursor_offset: 0,
            }
        );
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_parse_graph_uri_invalid_scheme() {
        let result = parse_graph_uri("file:///path/to/file");
        assert!(result.unwrap_err().message.contains("invalid URI scheme"));
    }

    #[test]
    fn test_parse_graph_uri_unknown_query_type() {
        let result = parse_graph_uri("aptu-coder://graph/abc123/unknown/foo");
        assert!(result.unwrap_err().message.contains("unknown query type"));
    }

    #[test]
    fn test_encode_decode_graph_cursor_roundtrip() {
        for offset in [0usize, 42, 9999] {
            let token = encode_graph_cursor(offset);
            assert_eq!(decode_graph_cursor(&token), Some(offset));
        }
    }

    #[test]
    fn test_parse_graph_uri_with_cursor() {
        let token = encode_graph_cursor(50);
        let uri = format!("aptu-coder://graph/abc123/blast-radius/my_func?cursor={token}");
        let (query, _format) = parse_graph_uri(&uri).unwrap();
        assert_eq!(
            query,
            GraphQuery::BlastRadius {
                repo_hash: "abc123".to_string(),
                symbol: "my_func".to_string(),
                depth: 3,
                cursor_offset: 50,
            }
        );
    }

    #[test]
    fn test_decode_graph_cursor_rejects_garbage() {
        assert_eq!(decode_graph_cursor("not-valid-base64"), None);
        assert_eq!(decode_graph_cursor(""), None);
    }

    #[test]
    fn test_query_to_graph_found_symbol() {
        // blast_radius_subgraph includes the start node as the first element and edges.
        // A Calls edge ensures the callee is returned and the Calls edge appears.
        let graph = make_graph_with_call("caller_func", "callee_func");
        let query = GraphQuery::BlastRadius {
            repo_hash: "x".to_string(),
            symbol: "caller_func".to_string(),
            depth: 3,
            cursor_offset: 0,
        };
        let (nodes, edges) = query_to_graph(&graph, &query);
        assert!(!nodes.is_empty(), "nodes should be non-empty");
        assert!(!edges.is_empty(), "edges should be non-empty");
        // Check that at least one edge has kind equal to the string "Calls"
        let has_calls_edge = edges
            .iter()
            .any(|(_, _, kind)| kind.as_str() == Some("Calls"));
        assert!(has_calls_edge, "should have at least one Calls edge");
    }

    #[test]
    fn test_query_to_graph_multiple_edges() {
        // Test that multiple Calls edges are all captured in the result.
        // Create a graph where one function calls two others.
        let mut f1 = FunctionInfo::default();
        f1.name = "main".to_string();
        f1.line = 1;
        f1.end_line = 5;

        let mut f2 = FunctionInfo::default();
        f2.name = "helper1".to_string();
        f2.line = 10;
        f2.end_line = 15;

        let mut f3 = FunctionInfo::default();
        f3.name = "helper2".to_string();
        f3.line = 20;
        f3.end_line = 25;

        let call1: CallInfo =
            serde_json::from_str(r#"{"caller":"main","callee":"helper1","line":2,"column":0}"#)
                .expect("valid call JSON");
        let call2: CallInfo =
            serde_json::from_str(r#"{"caller":"main","callee":"helper2","line":3,"column":0}"#)
                .expect("valid call JSON");

        let analysis = SemanticAnalysis::new(
            vec![f1, f2, f3],
            vec![],
            vec![],
            vec![],
            Default::default(),
            vec![call1, call2],
            vec![],
        );
        let entry = FileAnalysisOutput::new(
            "test.rs".to_string(),
            "test.rs:1:1:1".to_string(),
            analysis,
            30,
            None,
        );
        let graph = StructuralGraph::build_from_analysis(&[entry]);

        let query = GraphQuery::BlastRadius {
            repo_hash: "x".to_string(),
            symbol: "main".to_string(),
            depth: 3,
            cursor_offset: 0,
        };
        let (nodes, edges) = query_to_graph(&graph, &query);
        assert_eq!(nodes.len(), 3, "should have 3 nodes (main + 2 helpers)");
        assert_eq!(edges.len(), 2, "should have 2 Calls edges");
    }

    #[test]
    fn test_parse_graph_uri_depth_at_max_accepted() {
        let uri = "aptu-coder://graph/abc123/blast-radius/my_func?depth=5";
        let (query, _format) = parse_graph_uri(uri).unwrap();
        assert_eq!(
            query,
            GraphQuery::BlastRadius {
                repo_hash: "abc123".to_string(),
                symbol: "my_func".to_string(),
                depth: 5,
                cursor_offset: 0,
            }
        );
    }

    #[test]
    fn test_parse_graph_uri_depth_exceeds_max_rejected() {
        let uri = "aptu-coder://graph/abc123/blast-radius/my_func?depth=6";
        let err = parse_graph_uri(uri).unwrap_err();
        assert!(
            err.message.contains("exceeds maximum"),
            "unexpected error message: {}",
            err.message
        );
    }

    #[test]
    fn test_read_resource_impl_large_graph_pagination() {
        let tmp = std::env::temp_dir().join("aptu-coder-test-pagination");
        let _ = std::fs::create_dir_all(&tmp);
        let store = GraphDiskStore::new(tmp.clone());

        // Build a star graph: func_0 calls func_1..func_60 (60 callees).
        let mut functions = Vec::with_capacity(61);
        let mut calls = Vec::with_capacity(60);

        let mut f0 = FunctionInfo::default();
        f0.name = "func_0".to_string();
        f0.line = 1;
        f0.end_line = 5;
        functions.push(f0);

        for i in 1..=60 {
            let callee_name = format!("func_{i}");
            let mut f = FunctionInfo::default();
            f.name = callee_name.clone();
            f.line = i * 10;
            f.end_line = i * 10 + 5;
            functions.push(f);

            let call: CallInfo = serde_json::from_str(&format!(
                r#"{{"caller":"func_0","callee":"{callee_name}","line":1,"column":0}}"#
            ))
            .expect("valid call JSON");
            calls.push(call);
        }

        let analysis = SemanticAnalysis::new(
            functions,
            vec![],
            vec![],
            vec![],
            Default::default(),
            calls,
            vec![],
        );
        let entry = FileAnalysisOutput::new(
            "test.rs".to_string(),
            "test.rs:1:1:1".to_string(),
            analysis,
            650,
            None,
        );
        let graph = StructuralGraph::build_from_analysis(&[entry]);

        let repo_hash = "repo_test_pagination";
        store.put(repo_hash, &graph);

        // First page read (cursor_offset 0)
        let request1 = ReadResourceRequestParams::new(format!(
            "aptu-coder://graph/{repo_hash}/blast-radius/func_0"
        ));
        let response1 = read_resource_impl(request1, &store).expect("first page read succeeds");
        let result1 = match response1 {
            ReadResourceResponse::Complete(res) => res,
            _ => panic!("expected ReadResourceResponse::Complete"),
        };
        let text1 = match &result1.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => text,
            _ => panic!("expected text resource contents"),
        };
        let val1: serde_json::Value = serde_json::from_str(text1).expect("valid JSON payload");
        assert_eq!(
            val1["nodes"].as_array().unwrap().len(),
            50,
            "first page should have 50 nodes"
        );
        assert_eq!(
            val1["total"].as_u64().unwrap(),
            61,
            "total should be 61 (start + 60 callees)"
        );
        assert!(val1["next_cursor"].as_str().is_some());
        // First page should have edges (func_0 at position 0 has Calls edges to its 49 in-page callee neighbors)
        assert!(
            !val1["edges"].as_array().unwrap().is_empty(),
            "first page should have edges"
        );

        // Second page read using cursor token for offset 50
        let cursor_token = encode_graph_cursor(50);
        let request2 = ReadResourceRequestParams::new(format!(
            "aptu-coder://graph/{repo_hash}/blast-radius/func_0?cursor={cursor_token}"
        ));
        let response2 = read_resource_impl(request2, &store).expect("second page read succeeds");
        let result2 = match response2 {
            ReadResourceResponse::Complete(res) => res,
            _ => panic!("expected ReadResourceResponse::Complete"),
        };
        let text2 = match &result2.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => text,
            _ => panic!("expected text resource contents"),
        };
        let val2: serde_json::Value = serde_json::from_str(text2).expect("valid JSON payload");
        assert_eq!(
            val2["nodes"].as_array().unwrap().len(),
            11,
            "second page should have 11 nodes"
        );
        assert_eq!(
            val2["total"].as_u64().unwrap(),
            61,
            "total should be 61 (start + 60 callees)"
        );
        assert!(val2["next_cursor"].is_null());
        // Second page should have empty edges array (every edge touches func_0, which is not present on page two)
        assert!(
            val2["edges"].as_array().unwrap().is_empty(),
            "second page should have no edges"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_read_resource_impl_cold_cache_miss() {
        let tmp = std::env::temp_dir().join("aptu-coder-test-resources");
        let _ = std::fs::create_dir_all(&tmp);
        let store = GraphDiskStore::new(tmp.clone());
        let _ = std::fs::remove_dir_all(&tmp);

        let request =
            ReadResourceRequestParams::new("aptu-coder://graph/abc123/blast-radius/my_func");
        let err = read_resource_impl(request, &store).unwrap_err();
        assert!(
            err.message.contains("graph not built yet"),
            "unexpected error: {}",
            err.message
        );
    }

    #[test]
    fn test_parse_graph_uri_bidirectional_happy_path() {
        let uri =
            "aptu-coder://graph/abc123/blast-radius-bidirectional/foo,bar?max_nodes=10&depth=2";
        let (query, format) = parse_graph_uri(uri).unwrap();
        assert_eq!(
            query,
            GraphQuery::BidirectionalBlastRadius {
                repo_hash: "abc123".to_string(),
                symbols: vec!["foo".to_string(), "bar".to_string()],
                max_nodes: 10,
                depth: 2,
                cursor_offset: 0,
            }
        );
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_parse_graph_uri_bidirectional_empty_symbols_rejected() {
        let uri = "aptu-coder://graph/abc123/blast-radius-bidirectional/?depth=3";
        let err = parse_graph_uri(uri).unwrap_err();
        assert!(err.message.contains("at least one"));
    }

    #[test]
    fn test_parse_graph_uri_format_text() {
        let uri = "aptu-coder://graph/abc123/blast-radius/my_func?format=text";
        let (_query, format) = parse_graph_uri(uri).unwrap();
        assert_eq!(format, OutputFormat::Text);
    }

    #[test]
    fn test_parse_graph_uri_format_invalid_rejected() {
        let uri = "aptu-coder://graph/abc123/blast-radius/my_func?format=xml";
        let err = parse_graph_uri(uri).unwrap_err();
        assert!(err.message.contains("invalid format parameter"));
    }

    #[test]
    fn test_query_to_graph_bidirectional() {
        // Build a 3-function chain: a calls b, b calls c
        let mut fa = FunctionInfo::default();
        fa.name = "a".to_string();
        fa.line = 1;
        fa.end_line = 5;

        let mut fb = FunctionInfo::default();
        fb.name = "b".to_string();
        fb.line = 10;
        fb.end_line = 15;

        let mut fc = FunctionInfo::default();
        fc.name = "c".to_string();
        fc.line = 20;
        fc.end_line = 25;

        let call_ab: CallInfo =
            serde_json::from_str(r#"{"caller":"a","callee":"b","line":2,"column":0}"#)
                .expect("valid call JSON");
        let call_bc: CallInfo =
            serde_json::from_str(r#"{"caller":"b","callee":"c","line":11,"column":0}"#)
                .expect("valid call JSON");

        let analysis = SemanticAnalysis::new(
            vec![fa, fb, fc],
            vec![],
            vec![],
            vec![],
            Default::default(),
            vec![call_ab, call_bc],
            vec![],
        );
        let entry = FileAnalysisOutput::new(
            "test.rs".to_string(),
            "test.rs:1:1:1".to_string(),
            analysis,
            30,
            None,
        );
        let graph = StructuralGraph::build_from_analysis(&[entry]);

        let query = GraphQuery::BidirectionalBlastRadius {
            repo_hash: "x".to_string(),
            symbols: vec!["b".to_string()],
            max_nodes: 50,
            depth: 3,
            cursor_offset: 0,
        };
        let (nodes, edges) = query_to_graph(&graph, &query);
        // Starting from b, bidirectional BFS should find a (caller), b (start), and c (callee)
        assert!(
            !nodes.is_empty(),
            "should have at least the start node, got {} nodes",
            nodes.len()
        );
        // Bidirectional search should return edges connecting the symbols
        assert!(!edges.is_empty(), "bidirectional search should have edges");
    }
}
