// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;

/// A single edge in the call graph with impl-trait metadata.
/// `neighbor_name` holds the caller name in `callers` maps and the callee name in `callees` maps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallEdge {
    pub path: PathBuf,
    pub line: usize,
    pub neighbor_name: String,
    pub is_impl_trait: bool,
}

/// Information about an `impl Trait for Type` block found in Rust source.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImplTraitInfo {
    pub trait_name: String,
    pub impl_type: String,
    pub path: PathBuf,
    pub line: usize,
}

/// Kind of definition or use of a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DefUseKind {
    /// Symbol write (declaration, assignment LHS).
    Write,
    /// Symbol read (reference in expression context).
    Read,
    /// Augmented assignment (+=, |=, ++, etc.); both written and read.
    WriteRead,
}

/// A single definition or use site of a symbol within a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct DefUseSite {
    /// Kind of site: write, read, or write_read.
    pub kind: DefUseKind,
    /// Symbol name.
    pub symbol: String,
    /// File path (relative to the analysis root).
    pub file: String,
    /// Line number (1-indexed) in the file.
    pub line: usize,
    /// Column offset (0-indexed, byte offset from line start).
    pub column: usize,
    /// 3-line code context: lines N-1, N, N+1 from source.
    pub snippet: String,
    /// Name of the enclosing function or method, or None if at file scope.
    pub enclosing_scope: Option<String>,
}

/// Pagination parameters shared across all tools.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct PaginationParams {
    /// Pagination cursor from a previous response's `next_cursor` field. Pass unchanged to retrieve the next page. Omit on the first call.
    pub cursor: Option<String>,
    /// Files per page for pagination (default: 100). Reduce below 100 to limit response size; increase above 100 to reduce round trips.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_page_size_schema")
    )]
    pub page_size: Option<usize>,
}

/// Output control parameters shared across all tools.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct OutputControlParams {
    /// Return full output even when it exceeds the 50K char limit. Prefer summary=true or narrowing scope over force=true; force=true can produce very large responses.
    pub force: Option<bool>,
    /// true = compact summary (totals plus directory tree, no per-file function lists); false = full output; unset = auto-summarize when output exceeds 50K chars.
    /// Mutually exclusive with cursor; passing both returns INVALID_PARAMS.
    pub summary: Option<bool>,
    /// true = full output with section headers and imports (Markdown-style); false or unset = compact one-line-per-item format (default).
    pub verbose: Option<bool>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct AnalyzeDirectoryParams {
    /// Directory path to analyze
    pub path: String,

    /// Maximum directory traversal depth for overview mode only. 0 or unset = unlimited depth. Use 1-3 for large monorepos to manage output size. Ignored in other modes.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_integer_schema")
    )]
    pub max_depth: Option<u32>,

    /// Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering. Example: "main" or "HEAD~1".
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering."
        )
    )]
    pub git_ref: Option<String>,

    #[serde(flatten)]
    pub pagination: PaginationParams,

    #[serde(flatten)]
    pub output_control: OutputControlParams,
}

/// Output section selector for `analyze_file` fields projection.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AnalyzeFileField {
    /// Include function definitions with signatures, types, and line ranges.
    Functions,
    /// Include class and method definitions with inheritance and fields.
    Classes,
    /// Include import statements.
    Imports,
    /// Include all sections (equivalent to omitting fields parameter).
    All,
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct AnalyzeFileParams {
    /// File path to analyze
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::supported_file_path_schema")
    )]
    pub path: String,

    /// AST traversal depth limit for tree-sitter queries. Leave unset in normal use; increase only for deeply nested generated code. None=library default, 0=unlimited, n=limit to n levels.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_ast_limit_schema")
    )]
    pub ast_recursion_limit: Option<usize>,

    /// Limit output to specific sections. Valid values: "functions", "classes", "imports", "all".
    /// The FILE header (path, line count, section counts) is always emitted regardless.
    /// Omitting this field returns all sections (current behavior).
    /// Ignored when summary=true (summary takes precedence).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(extend("examples" = [["functions", "classes"], ["functions"], ["imports"]])))]
    pub fields: Option<Vec<AnalyzeFileField>>,

    #[serde(flatten)]
    pub pagination: PaginationParams,

    #[serde(flatten)]
    pub output_control: OutputControlParams,
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct AnalyzeModuleParams {
    /// File path to analyze
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::supported_file_path_schema")
    )]
    pub path: String,
}

/// Symbol name matching strategy for `analyze_symbol`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SymbolMatchMode {
    #[default]
    Exact,
    Insensitive,
    Prefix,
    Contains,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct AnalyzeSymbolParams {
    /// Directory path to search for the symbol
    pub path: String,

    /// Symbol name to build call graph for (function or method). Example: `parse_config` finds all callers and callees of that function.
    pub symbol: String,

    /// Symbol matching mode (default: exact). exact: case-sensitive exact match. insensitive: case-insensitive exact match. prefix: case-insensitive prefix match. contains: case-insensitive substring match.
    pub match_mode: Option<SymbolMatchMode>,

    /// Call graph traversal depth for this tool (default 1). Level 1 = direct callers and callees; level 2 = one more hop, etc. Output size grows exponentially with graph branching. Warn user on levels above 2.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_integer_schema")
    )]
    pub follow_depth: Option<u32>,

    /// Maximum directory traversal depth. Unset means unlimited. Use 2-3 for large monorepos.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_integer_schema")
    )]
    pub max_depth: Option<u32>,

    /// AST traversal depth limit for tree-sitter queries. Leave unset in normal use; increase only for deeply nested generated code. None=library default, 0=unlimited, n=limit to n levels.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_ast_limit_schema")
    )]
    pub ast_recursion_limit: Option<usize>,

    #[serde(flatten)]
    pub pagination: PaginationParams,

    #[serde(flatten)]
    pub output_control: OutputControlParams,

    /// Filter callers to impl Trait for Type blocks only. Rust only; returns INVALID_PARAMS for other languages.
    #[serde(default)]
    pub impl_only: Option<bool>,

    /// Find all files in the directory that import the module path given in symbol (e.g., std::collections). Requires symbol to be non-empty. Mutually exclusive with def_use and normal call-graph lookup. When set, follow_depth, impl_only, and match_mode are ignored.
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Find all files in the directory that import the module path given in symbol (e.g., std::collections). Requires symbol to be non-empty. Mutually exclusive with def_use and normal call-graph lookup. When set, follow_depth, impl_only, and match_mode are ignored."
        )
    )]
    pub import_lookup: Option<bool>,

    /// Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering. Example: "main" or "HEAD~1".
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering."
        )
    )]
    pub git_ref: Option<String>,

    /// Extract write/read sites for the symbol. First call returns empty def_use_sites and a cursor; paginate with that cursor to retrieve results. Mutually exclusive with import_lookup. Default: false.
    #[serde(default)]
    pub def_use: Option<bool>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct AnalysisResult {
    pub path: String,
    pub mode: AnalysisMode,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub import_count: usize,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_integer_schema")
    )]
    pub main_line: Option<usize>,
    pub files: Vec<FileInfo>,
    pub functions: Vec<FunctionInfo>,
    pub classes: Vec<ClassInfo>,
    pub references: Vec<ReferenceInfo>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct FileInfo {
    pub path: String,
    pub language: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line_count: usize,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub function_count: usize,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub class_count: usize,
    /// Whether this file is a test file.
    pub is_test: bool,
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct FunctionInfo {
    pub name: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line: usize,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub end_line: usize,
    /// Parameter list as string representations (e.g., `["x: i32", "y: String"]`).
    pub parameters: Vec<String>,
    pub return_type: Option<String>,
}

impl FunctionInfo {
    /// Maximum length for parameter display before truncation.
    const MAX_PARAMS_DISPLAY_LEN: usize = 80;
    /// Truncation point when parameters exceed `MAX_PARAMS_DISPLAY_LEN`.
    const TRUNCATION_POINT: usize = 77;

    /// Format function signature as a single-line string with truncation.
    /// Returns: `name(param1, param2, ...) -> return_type :start-end`
    /// Parameters are truncated to ~80 chars with `...` if needed.
    #[must_use]
    pub fn compact_signature(&self) -> String {
        let mut sig = String::with_capacity(self.name.len() + 40);
        sig.push_str(&self.name);
        sig.push('(');

        if !self.parameters.is_empty() {
            let params_str = self.parameters.join(", ");
            if params_str.len() > Self::MAX_PARAMS_DISPLAY_LEN {
                // Truncate at a safe char boundary to avoid panicking on multibyte UTF-8.
                let truncate_at = params_str.floor_char_boundary(Self::TRUNCATION_POINT);
                sig.push_str(&params_str[..truncate_at]);
                sig.push_str("...");
            } else {
                sig.push_str(&params_str);
            }
        }

        sig.push(')');

        if let Some(ret_type) = &self.return_type {
            sig.push_str(" -> ");
            sig.push_str(ret_type);
        }

        write!(sig, " :{}-{}", self.line, self.end_line).ok();
        sig
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct ClassInfo {
    pub name: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line: usize,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub end_line: usize,
    pub methods: Vec<FunctionInfo>,
    pub fields: Vec<String>,
    /// Inherited types (parent classes, interfaces, trait bounds).
    #[cfg_attr(
        feature = "schemars",
        schemars(description = "Inherited types (parent classes, interfaces, trait bounds)")
    )]
    pub inherits: Vec<String>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct CallInfo {
    pub caller: String,
    pub callee: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line: usize,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub column: usize,
    /// Number of arguments passed at the call site.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_integer_schema")
    )]
    pub arg_count: Option<usize>,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct ReferenceInfo {
    pub symbol: String,
    pub reference_type: ReferenceType,
    pub location: String,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line: usize,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReferenceType {
    Definition,
    Usage,
    Import,
    Export,
}

/// Analysis mode for generating output.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AnalysisMode {
    /// High-level directory structure and file counts.
    Overview,
    /// Detailed semantic analysis of functions, classes, and references within a file.
    FileDetails,
    /// Call graph and dataflow analysis focused on a specific symbol.
    SymbolFocus,
    /// Fast function and import index for a single file (analyze_module fast path).
    ModuleOnly,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct CallChain {
    pub chain: Vec<CallInfo>,
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub depth: u32,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct FocusedAnalysisData {
    pub symbol: String,
    pub definition: Option<FunctionInfo>,
    pub call_chains: Vec<CallChain>,
    pub references: Vec<ReferenceInfo>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct ImportInfo {
    /// Full module path excluding the imported symbol (e.g., `std::collections` for `use std::collections::HashMap`).
    pub module: String,
    /// Imported symbols (e.g., `[HashMap]` for `use std::collections::HashMap`).
    pub items: Vec<String>,
    /// Line number where import appears.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[non_exhaustive]
pub struct SemanticAnalysis {
    pub functions: Vec<FunctionInfo>,
    pub classes: Vec<ClassInfo>,
    /// Flat list of imports; each entry carries its full module path and imported symbols.
    pub imports: Vec<ImportInfo>,
    pub references: Vec<ReferenceInfo>,
    /// Call frequency map (function name -> count).
    #[serde(skip)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub call_frequency: HashMap<String, usize>,
    /// Caller-callee pairs extracted from call expressions.
    pub calls: Vec<CallInfo>,
    /// `impl Trait for Type` blocks found in this file (Rust only).
    #[serde(skip)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub impl_traits: Vec<ImplTraitInfo>,
    /// Definition and use sites for a focused symbol (in-memory only).
    #[serde(skip)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub def_use_sites: Vec<DefUseSite>,
}

impl SemanticAnalysis {
    /// Create a new `SemanticAnalysis` with all fields specified.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        functions: Vec<crate::types::FunctionInfo>,
        classes: Vec<crate::types::ClassInfo>,
        imports: Vec<crate::types::ImportInfo>,
        references: Vec<crate::types::ReferenceInfo>,
        call_frequency: HashMap<String, usize>,
        calls: Vec<crate::types::CallInfo>,
        impl_traits: Vec<ImplTraitInfo>,
    ) -> Self {
        Self {
            functions,
            classes,
            imports,
            references,
            call_frequency,
            calls,
            impl_traits,
            def_use_sites: Vec::new(),
        }
    }

    /// Return a filtered copy of this `SemanticAnalysis` based on the requested field set.
    ///
    /// - `None` (or a slice containing `AnalyzeFileField::All`) returns a full clone.
    /// - Otherwise each of `functions`, `classes`, and `imports` is populated only when the
    ///   corresponding `AnalyzeFileField` variant is present in `fields`.
    /// - All other fields (`references`, `call_frequency`, `calls`, `impl_traits`,
    ///   `def_use_sites`) are always preserved unchanged.
    #[must_use]
    pub fn project(&self, fields: Option<&[AnalyzeFileField]>) -> Self {
        let Some(fields) = fields else {
            return self.clone();
        };
        if fields.iter().any(|f| matches!(f, AnalyzeFileField::All)) {
            return self.clone();
        }
        let functions = if fields
            .iter()
            .any(|f| matches!(f, AnalyzeFileField::Functions))
        {
            self.functions.clone()
        } else {
            Vec::new()
        };
        let classes = if fields
            .iter()
            .any(|f| matches!(f, AnalyzeFileField::Classes))
        {
            self.classes.clone()
        } else {
            Vec::new()
        };
        let imports = if fields
            .iter()
            .any(|f| matches!(f, AnalyzeFileField::Imports))
        {
            self.imports.clone()
        } else {
            Vec::new()
        };
        Self {
            functions,
            classes,
            imports,
            references: self.references.clone(),
            call_frequency: self.call_frequency.clone(),
            calls: self.calls.clone(),
            impl_traits: self.impl_traits.clone(),
            def_use_sites: self.def_use_sites.clone(),
        }
    }
}
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct ModuleFunctionInfo {
    /// Function name
    pub name: String,
    /// Line number where function is defined
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line: usize,
}

/// Minimal import info for `analyze_module`: module and items only.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct ModuleImportInfo {
    /// Full module path (e.g., `std::collections` for `use std::collections::HashMap`)
    pub module: String,
    /// Imported symbols (e.g., `[HashMap]`)
    pub items: Vec<String>,
}

/// Minimal fixed schema for `analyze_module`: lightweight code understanding.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct ModuleInfo {
    /// File name (basename only, e.g., 'lib.rs')
    pub name: String,
    /// Total line count in file
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line_count: usize,
    /// Programming language (e.g., 'rust', 'python', 'go')
    pub language: String,
    /// Function definitions (name and line only)
    pub functions: Vec<ModuleFunctionInfo>,
    /// Import statements (module and items only)
    pub imports: Vec<ModuleImportInfo>,
}

impl ModuleInfo {
    /// Create a new `ModuleInfo` with all fields specified.
    #[must_use]
    pub fn new(
        name: String,
        line_count: usize,
        language: String,
        functions: Vec<ModuleFunctionInfo>,
        imports: Vec<ModuleImportInfo>,
    ) -> Self {
        Self {
            name,
            line_count,
            language,
            functions,
            imports,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_signature_short_params() {
        let func = FunctionInfo {
            name: "add".to_string(),
            line: 10,
            end_line: 12,
            parameters: vec!["a: i32".to_string(), "b: i32".to_string()],
            return_type: Some("i32".to_string()),
        };

        let sig = func.compact_signature();
        assert_eq!(sig, "add(a: i32, b: i32) -> i32 :10-12");
    }

    #[test]
    fn test_compact_signature_long_params_truncation() {
        let func = FunctionInfo {
            name: "process".to_string(),
            line: 20,
            end_line: 50,
            parameters: vec![
                "config: ComplexConfigType".to_string(),
                "data: VeryLongDataStructureNameThatExceedsEightyCharacters".to_string(),
                "callback: Fn(Result) -> ()".to_string(),
            ],
            return_type: Some("Result<Output>".to_string()),
        };

        let sig = func.compact_signature();
        assert!(sig.contains("process("));
        assert!(sig.contains("..."));
        assert!(sig.contains("-> Result<Output>"));
        assert!(sig.contains(":20-50"));
    }

    #[test]
    fn test_compact_signature_empty_params() {
        let func = FunctionInfo {
            name: "main".to_string(),
            line: 1,
            end_line: 5,
            parameters: vec![],
            return_type: None,
        };

        let sig = func.compact_signature();
        assert_eq!(sig, "main() :1-5");
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn schema_flatten_inline() {
        use schemars::schema_for;

        // Test AnalyzeDirectoryParams: cursor, page_size, force, summary must be top-level
        let dir_schema = schema_for!(AnalyzeDirectoryParams);
        let dir_props = dir_schema
            .as_object()
            .and_then(|o| o.get("properties"))
            .and_then(|v| v.as_object())
            .expect("AnalyzeDirectoryParams must have properties");

        assert!(
            dir_props.contains_key("cursor"),
            "cursor must be top-level in AnalyzeDirectoryParams schema"
        );
        assert!(
            dir_props.contains_key("page_size"),
            "page_size must be top-level in AnalyzeDirectoryParams schema"
        );
        assert!(
            dir_props.contains_key("force"),
            "force must be top-level in AnalyzeDirectoryParams schema"
        );
        assert!(
            dir_props.contains_key("summary"),
            "summary must be top-level in AnalyzeDirectoryParams schema"
        );

        // Test AnalyzeFileParams
        let file_schema = schema_for!(AnalyzeFileParams);
        let file_props = file_schema
            .as_object()
            .and_then(|o| o.get("properties"))
            .and_then(|v| v.as_object())
            .expect("AnalyzeFileParams must have properties");

        assert!(
            file_props.contains_key("cursor"),
            "cursor must be top-level in AnalyzeFileParams schema"
        );
        assert!(
            file_props.contains_key("page_size"),
            "page_size must be top-level in AnalyzeFileParams schema"
        );
        assert!(
            file_props.contains_key("force"),
            "force must be top-level in AnalyzeFileParams schema"
        );
        assert!(
            file_props.contains_key("summary"),
            "summary must be top-level in AnalyzeFileParams schema"
        );

        // Test AnalyzeSymbolParams
        let symbol_schema = schema_for!(AnalyzeSymbolParams);
        let symbol_props = symbol_schema
            .as_object()
            .and_then(|o| o.get("properties"))
            .and_then(|v| v.as_object())
            .expect("AnalyzeSymbolParams must have properties");

        assert!(
            symbol_props.contains_key("cursor"),
            "cursor must be top-level in AnalyzeSymbolParams schema"
        );
        assert!(
            symbol_props.contains_key("page_size"),
            "page_size must be top-level in AnalyzeSymbolParams schema"
        );
        assert!(
            symbol_props.contains_key("force"),
            "force must be top-level in AnalyzeSymbolParams schema"
        );
        assert!(
            symbol_props.contains_key("summary"),
            "summary must be top-level in AnalyzeSymbolParams schema"
        );

        // Verify ast_recursion_limit enforces minimum: 0 in both parameter schemas.
        let file_ast = file_props
            .get("ast_recursion_limit")
            .expect("ast_recursion_limit must be present in AnalyzeFileParams schema");
        assert_eq!(
            file_ast.get("minimum").and_then(|v| v.as_u64()),
            Some(0),
            "ast_recursion_limit in AnalyzeFileParams must have minimum: 0"
        );
        let symbol_ast = symbol_props
            .get("ast_recursion_limit")
            .expect("ast_recursion_limit must be present in AnalyzeSymbolParams schema");
        assert_eq!(
            symbol_ast.get("minimum").and_then(|v| v.as_u64()),
            Some(0),
            "ast_recursion_limit in AnalyzeSymbolParams must have minimum: 0"
        );
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct EditOverwriteParams {
    /// Path to the file to create or overwrite.
    pub path: String,
    /// Optional base directory for path resolution (default: server CWD).
    pub working_dir: Option<String>,
    /// UTF-8 content to write.
    pub content: String,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct EditOverwriteOutput {
    /// Path of the file that was written.
    pub path: String,
    /// Number of bytes written (UTF-8 byte length of content).
    pub bytes_written: usize,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct EditReplaceParams {
    /// Path to the file to edit.
    pub path: String,
    /// Optional base directory for path resolution (default: server CWD).
    pub working_dir: Option<String>,
    /// Exact text block to find and replace. Must appear exactly once in the file.
    pub old_text: String,
    /// Replacement text.
    pub new_text: String,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct EditReplaceOutput {
    /// Path of the file that was edited.
    pub path: String,
    /// File size in bytes before the edit.
    pub bytes_before: usize,
    /// File size in bytes after the edit.
    pub bytes_after: usize,
}

/// Filter rule for command output post-processing.
/// Matches command prefixes and applies transformations (strip/keep/cap lines).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct FilterRule {
    /// Regex pattern to match against the command string (e.g., "^git\\s+pull").
    pub match_command: String,
    /// Optional description of what this filter does.
    pub description: Option<String>,
    /// If true, strip ANSI escape sequences before processing lines.
    #[serde(default)]
    pub strip_ansi: bool,
    /// Regex patterns: lines matching any of these are removed from output.
    #[serde(default)]
    pub strip_lines_matching: Vec<String>,
    /// Regex patterns: if non-empty, retain only lines matching any of these.
    #[serde(default)]
    pub keep_lines_matching: Vec<String>,
    /// Maximum number of lines to keep in output.
    pub max_lines: Option<usize>,
    /// Replacement text if filtered output is empty (success-only).
    pub on_empty: Option<String>,
}
