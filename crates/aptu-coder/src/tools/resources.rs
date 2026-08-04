// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! MCP Resource surface for the structural knowledge graph.
//!
//! Implements `list_resources`, `list_resource_templates`, and `read_resource`
//! for the `aptu-coder://graph/{repo_hash}/{query_type}/{arg}?cursor=...` URI scheme.
//! Three resource templates are advertised: blast-radius, import-closure, subgraph.

use aptu_coder_core::graph::{GraphDiskStore, StructuralGraph};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rmcp::RoleServer;
use rmcp::model::{
    ErrorCode, ErrorData, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents,
    ResourceTemplate,
};
use rmcp::service::RequestContext;

const PAGE_SIZE: usize = 50;

/// Graph query variants parsed from resource URIs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GraphQuery {
    BlastRadius {
        symbol: String,
        depth: usize,
        cursor_offset: usize,
    },
    ImportClosure {
        module: String,
        cursor_offset: usize,
    },
    Subgraph {
        symbol: String,
        cursor_offset: usize,
    },
}

/// Base64url-encode a JSON `{"g":offset}` cursor token.
/// Uses base64url (no padding) to avoid '+' and '/' in query strings.
fn encode_graph_cursor(offset: usize) -> String {
    let json = serde_json::json!({"g": offset});
    let json_str = serde_json::to_string(&json).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json_str.as_bytes())
}

/// Decode a graph cursor token. Returns `None` if the token is not a valid
/// graph cursor (e.g. it is a PaginationMode cursor or garbage).
fn decode_graph_cursor(s: &str) -> Option<usize> {
    let decoded = URL_SAFE_NO_PAD.decode(s.as_bytes()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("g")?.as_u64().map(|n| n as usize)
}

/// Parse a `aptu-coder://graph/{repo_hash}/{query_type}/{arg}?cursor=...&depth=N` URI.
///
/// Validates scheme, path structure, and query type. The `repo_hash` segment is
/// extracted and used as the `GraphDiskStore` key in `read_resource_impl`; it is
/// not re-validated here against disk (the store returns `None` on a stale key).
fn parse_graph_uri(uri: &str) -> Result<GraphQuery, ErrorData> {
    let rest = uri.strip_prefix("aptu-coder://").ok_or_else(|| {
        ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            format!("invalid URI scheme: expected aptu-coder://, got {uri}"),
            None,
        )
    })?;

    // Split path from query string.
    let (path_part, qs) = if let Some(pos) = rest.find('?') {
        (&rest[..pos], Some(&rest[pos + 1..]))
    } else {
        (rest, None)
    };

    // Parse query-string params: cursor=<token>&depth=<N>
    let mut cursor_offset: usize = 0;
    let mut depth: usize = 3;
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

    let query_type = segments[2];
    let arg = segments[3..].join("/");

    match query_type {
        "blast-radius" => Ok(GraphQuery::BlastRadius {
            symbol: arg,
            depth,
            cursor_offset,
        }),
        "import-closure" => Ok(GraphQuery::ImportClosure {
            module: arg,
            cursor_offset,
        }),
        "subgraph" => Ok(GraphQuery::Subgraph {
            symbol: arg,
            cursor_offset,
        }),
        _ => Err(ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            format!(
                "unknown query type '{query_type}': expected blast-radius, import-closure, or subgraph"
            ),
            None,
        )),
    }
}

/// Resolve a query against a graph into a flat list of node JSON values.
fn query_to_nodes(graph: &StructuralGraph, query: &GraphQuery) -> Vec<serde_json::Value> {
    match query {
        GraphQuery::BlastRadius { symbol, depth, .. } => {
            let indices = graph.bfs_blast_radius(symbol, *depth);
            indices
                .iter()
                .map(|idx| {
                    let node = graph.0[*idx].clone();
                    serde_json::to_value(node).unwrap_or(serde_json::Value::Null)
                })
                .collect()
        }
        GraphQuery::ImportClosure { module, .. } => {
            // Find all nodes that import the given module
            let indices = graph.bfs_blast_radius(module, 1);
            indices
                .iter()
                .map(|idx| {
                    let node = graph.0[*idx].clone();
                    serde_json::to_value(node).unwrap_or(serde_json::Value::Null)
                })
                .collect()
        }
        GraphQuery::Subgraph { symbol, .. } => {
            let indices = graph.bfs_blast_radius(symbol, 2);
            indices
                .iter()
                .map(|idx| {
                    let node = graph.0[*idx].clone();
                    serde_json::to_value(node).unwrap_or(serde_json::Value::Null)
                })
                .collect()
        }
    }
}

/// Return an empty resources list (concrete graph slices are unbounded;
/// clients use templates).
pub(crate) fn list_resources_impl(
    _params: Option<PaginatedRequestParams>,
    _context: &RequestContext<RoleServer>,
) -> Result<ListResourcesResult, ErrorData> {
    Ok(ListResourcesResult::with_all_items(Vec::new()))
}

/// Return three ResourceTemplate entries for the graph URI scheme.
pub(crate) fn list_resource_templates_impl(
    _params: Option<PaginatedRequestParams>,
    _context: &RequestContext<RoleServer>,
) -> Result<ListResourceTemplatesResult, ErrorData> {
    let templates = vec![
        ResourceTemplate::new(
            "aptu-coder://graph/{repo_hash}/blast-radius/{symbol}?depth={depth}",
            "graph-blast-radius",
        )
        .with_description("BFS blast-radius traversal from a symbol")
        .with_mime_type("application/json"),
        ResourceTemplate::new(
            "aptu-coder://graph/{repo_hash}/import-closure/{module}",
            "graph-import-closure",
        )
        .with_description("Import closure for a module path")
        .with_mime_type("application/json"),
        ResourceTemplate::new(
            "aptu-coder://graph/{repo_hash}/subgraph/{symbol}",
            "graph-subgraph",
        )
        .with_description("Subgraph centered on a symbol")
        .with_mime_type("application/json"),
    ];
    Ok(ListResourceTemplatesResult::with_all_items(templates))
}

/// Read a graph resource identified by URI.
///
/// Parses the URI, loads the graph from disk store, dispatches to the query
/// helper, paginates, and returns a `ReadResourceResponse::Complete`.
pub(crate) fn read_resource_impl(
    request: ReadResourceRequestParams,
    graph_store: &GraphDiskStore,
) -> Result<ReadResourceResponse, ErrorData> {
    // Parse URI to get the query and repo_hash
    let rest = request.uri.strip_prefix("aptu-coder://").ok_or_else(|| {
        ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            format!(
                "invalid URI scheme: expected aptu-coder://, got {}",
                request.uri
            ),
            None,
        )
    })?;
    let (path_part, _cursor_offset) = if let Some(pos) = rest.find('?') {
        (&rest[..pos], 0)
    } else {
        (rest, 0)
    };
    let segments: Vec<&str> = path_part.split('/').collect();
    if segments.len() < 4 || segments[0] != "graph" {
        return Err(ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            format!("invalid URI path: {}", request.uri),
            None,
        ));
    }
    let repo_hash = segments[1];

    // Parse the full query (including cursor from query string)
    let query = parse_graph_uri(&request.uri)?;

    // Load graph from store
    let graph = graph_store.get(repo_hash).ok_or_else(|| {
        ErrorData::new(
            ErrorCode::RESOURCE_NOT_FOUND,
            "graph not built yet -- call analyze_symbol on this directory first to build the graph cache".to_string(),
            None,
        )
    })?;

    // Resolve query to nodes
    let all_nodes = query_to_nodes(&graph, &query);
    let total = all_nodes.len();

    // Paginate manually
    let offset = match &query {
        GraphQuery::BlastRadius { cursor_offset, .. } => *cursor_offset,
        GraphQuery::ImportClosure { cursor_offset, .. } => *cursor_offset,
        GraphQuery::Subgraph { cursor_offset, .. } => *cursor_offset,
    };
    let page: Vec<serde_json::Value> = all_nodes
        .iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .cloned()
        .collect();

    let next_offset = if offset + PAGE_SIZE < total {
        Some(offset + PAGE_SIZE)
    } else {
        None
    };
    let next_cursor = next_offset.map(encode_graph_cursor);

    let payload = serde_json::json!({
        "nodes": page,
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

    let result = ReadResourceResult::new(vec![contents]);
    Ok(ReadResourceResponse::Complete(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aptu_coder_core::analyze::FileAnalysisOutput;
    use aptu_coder_core::types::{FunctionInfo, SemanticAnalysis};

    /// Build a graph with `caller` and `callee` symbols connected by a Calls edge.
    /// `bfs_blast_radius` on `caller` at depth>=1 will return `callee`.
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
        use aptu_coder_core::types::CallInfo;
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
        let entry = FileAnalysisOutput::new("test.rs:1:1:1".to_string(), analysis, 15, None);
        StructuralGraph::build_from_analysis(&[entry])
    }

    fn make_graph_with_node(name: &str) -> StructuralGraph {
        let mut func = FunctionInfo::default();
        func.name = name.to_string();
        func.line = 1;
        func.end_line = 5;
        let analysis = SemanticAnalysis::new(
            vec![func],
            vec![],
            vec![],
            vec![],
            Default::default(),
            vec![],
            vec![],
        );
        let entry = FileAnalysisOutput::new("test.rs:1:1:1".to_string(), analysis, 10, None);
        StructuralGraph::build_from_analysis(&[entry])
    }

    #[test]
    fn test_parse_graph_uri_blast_radius_happy_path() {
        let uri = "aptu-coder://graph/abc123/blast-radius/my_func";
        let query = parse_graph_uri(uri).unwrap();
        assert_eq!(
            query,
            GraphQuery::BlastRadius {
                symbol: "my_func".to_string(),
                depth: 3,
                cursor_offset: 0
            }
        );
    }

    #[test]
    fn test_parse_graph_uri_invalid_scheme() {
        let uri = "file:///path/to/file";
        let result = parse_graph_uri(uri);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("invalid URI scheme"));
    }

    #[test]
    fn test_parse_graph_uri_unknown_query_type() {
        let uri = "aptu-coder://graph/abc123/unknown/foo";
        let result = parse_graph_uri(uri);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("unknown query type"));
    }

    #[test]
    fn test_encode_decode_graph_cursor_roundtrip() {
        let token = encode_graph_cursor(42);
        assert_eq!(decode_graph_cursor(&token), Some(42));

        // Zero offset
        let token = encode_graph_cursor(0);
        assert_eq!(decode_graph_cursor(&token), Some(0));

        // Large offset
        let token = encode_graph_cursor(9999);
        assert_eq!(decode_graph_cursor(&token), Some(9999));
    }

    #[test]
    fn test_parse_graph_uri_with_cursor() {
        let token = encode_graph_cursor(50);
        let uri = format!("aptu-coder://graph/abc123/blast-radius/my_func?cursor={token}");
        let query = parse_graph_uri(&uri).unwrap();
        assert_eq!(
            query,
            GraphQuery::BlastRadius {
                symbol: "my_func".to_string(),
                depth: 3,
                cursor_offset: 50
            }
        );
    }

    #[test]
    fn test_decode_graph_cursor_rejects_garbage() {
        assert_eq!(decode_graph_cursor("not-valid-base64"), None);
        assert_eq!(decode_graph_cursor(""), None);
    }

    #[test]
    fn test_query_to_nodes_found_symbol() {
        // bfs_blast_radius returns NEIGHBORS of the start node, not the node itself.
        // Use a graph with a Calls edge so the BFS returns the callee.
        let graph = make_graph_with_call("caller_func", "callee_func");
        let query = GraphQuery::BlastRadius {
            symbol: "caller_func".to_string(),
            depth: 3,
            cursor_offset: 0,
        };
        let nodes = query_to_nodes(&graph, &query);
        assert!(!nodes.is_empty());
    }

    #[test]
    fn test_read_resource_impl_cold_cache_miss() {
        // A temp dir for the store (empty, no data)
        let tmp = std::env::temp_dir().join("aptu-coder-test-resources");
        let _ = std::fs::create_dir_all(&tmp);
        let store = GraphDiskStore::new(tmp.clone());
        let _ = std::fs::remove_dir_all(&tmp);

        let request =
            ReadResourceRequestParams::new("aptu-coder://graph/abc123/blast-radius/my_func");
        let result = read_resource_impl(request, &store);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("graph not built yet"),
            "expected 'graph not built yet' error, got: {}",
            err.message
        );
    }
}
