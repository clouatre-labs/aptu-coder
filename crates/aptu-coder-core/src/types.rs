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
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub line: usize,
    /// Column offset (0-indexed, byte offset from line start).
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
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
    /// Must be a valid opaque token or omitted entirely; passing an empty string is invalid and is treated as omitted.
    /// Mutually exclusive with summary=true; passing both returns INVALID_PARAMS.
    pub cursor: Option<String>,
    /// Legacy parameter removed per alpha policy (#1556). Presence is rejected so unknown
    /// pagination inputs fail deserialization instead of being silently ignored.
    #[serde(default, skip_serializing, deserialize_with = "reject_page_size")]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub page_size: Option<()>,
}

fn reject_page_size<'de, D>(deserializer: D) -> Result<Option<()>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde::de::IgnoredAny> = Option::deserialize(deserializer)?;
    match value {
        Some(_) => Err(serde::de::Error::custom(
            "unknown field `page_size`: page sizing is server-owned and removed from the parameter surface; use `cursor` for pagination",
        )),
        None => Ok(None),
    }
}

/// Output control parameters shared across all tools.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct OutputControlParams {
    /// true = compact summary (totals plus directory tree, no per-file function lists); false = full output; unset = auto-summarize when output exceeds 50K chars.
    /// Mutually exclusive with cursor; passing both returns INVALID_PARAMS.
    pub summary: Option<bool>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct AnalyzeDirectoryParams {
    /// Directory path to analyze
    pub path: String,

    /// Maximum directory traversal depth for overview mode only. Pass 0 for unlimited depth; use 1-3 for large monorepos to manage output size. Ignored in other modes.
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
    /// Include symbol references.
    References,
    /// Include caller-callee call pairs.
    Calls,
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

    /// Limit output to specific sections. Valid values: "functions", "classes", "imports", "references", "calls", "all". The FILE header (path, line count, section counts) is always emitted regardless. Omit for all sections. Ignored when summary=true.
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

/// Analysis mode for `analyze_symbol`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SymbolAnalysisMode {
    /// Build a call graph for the symbol (default).
    #[default]
    CallGraph,
    /// Find all files in the directory that import the module path given in symbol.
    ImportLookup,
    /// Extract write/read sites for the symbol.
    DefUse,
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

    /// Maximum traversal depth. For call graph mode, level 1 = direct callers and callees, level 2 = one more hop, etc.; also caps directory walking. Unset means graph depth 1 and unlimited directory walk. Warn user on levels above 2. Hard cap of 3.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::max_depth_schema")
    )]
    pub max_depth: Option<u32>,

    #[serde(flatten)]
    pub pagination: PaginationParams,

    #[serde(flatten)]
    pub output_control: OutputControlParams,

    /// Filter callers to impl Trait for Type blocks only. Rust only; returns INVALID_PARAMS for other languages.
    #[serde(default)]
    pub impl_only: Option<bool>,

    /// Analysis mode. call_graph (default): build a call graph for the symbol. import_lookup: find all files in the directory that import the module path given in symbol (e.g., std::collections); requires symbol to be non-empty and rejects match_mode/max_depth/impl_only. def_use: extract write/read sites for the symbol; requires symbol to be non-empty.
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Analysis mode. call_graph (default): build a call graph for the symbol. import_lookup: find all files in the directory that import the module path given in symbol (e.g., std::collections); requires symbol to be non-empty and rejects match_mode/max_depth/impl_only. def_use: extract write/read sites for the symbol; requires symbol to be non-empty."
        )
    )]
    pub mode: Option<SymbolAnalysisMode>,

    /// Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering. Example: "main" or "HEAD~1".
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering."
        )
    )]
    pub git_ref: Option<String>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct FileInfo {
    /// Path relative to the analyzed directory (absolute when outside base).
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "Path relative to the analyzed directory (absolute when outside base)."
        )
    )]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// Symbol references. Omitted from output when empty (projection-gated).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ReferenceInfo>,
    /// Call frequency map (function name -> count).
    #[serde(skip)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub call_frequency: HashMap<String, usize>,
    /// Caller-callee pairs extracted from call expressions.
    /// Omitted from output when empty (projection-gated).
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
    /// - `None` returns a clone with `references` and `calls` zero-filled: the default
    ///   response carries only functions, classes, and imports.
    /// - A slice containing `AnalyzeFileField::All` returns a full clone.
    /// - Otherwise each of `functions`, `classes`, `imports`, `references`, and `calls` is
    ///   populated only when the corresponding `AnalyzeFileField` variant is present in
    ///   `fields`.
    /// - `impl_traits`, and `def_use_sites` are always preserved unchanged.
    /// - `call_frequency` is preserved only when `Functions` is in the projected fields.
    #[must_use]
    pub fn project(&self, fields: Option<&[AnalyzeFileField]>) -> Self {
        let Some(fields) = fields else {
            let mut out = self.clone();
            out.references = Vec::new();
            out.calls = Vec::new();
            return out;
        };

        // Single pass: derive a presence bitmask so each variant is checked exactly once.
        let mut want_all = false;
        let mut want_functions = false;
        let mut want_classes = false;
        let mut want_imports = false;
        let mut want_references = false;
        let mut want_calls = false;
        for f in fields {
            match f {
                AnalyzeFileField::All => {
                    want_all = true;
                    break;
                }
                AnalyzeFileField::Functions => want_functions = true,
                AnalyzeFileField::Classes => want_classes = true,
                AnalyzeFileField::Imports => want_imports = true,
                AnalyzeFileField::References => want_references = true,
                AnalyzeFileField::Calls => want_calls = true,
            }
        }
        if want_all {
            return self.clone();
        }

        Self {
            functions: if want_functions {
                self.functions.clone()
            } else {
                Vec::new()
            },
            classes: if want_classes {
                self.classes.clone()
            } else {
                Vec::new()
            },
            imports: if want_imports {
                self.imports.clone()
            } else {
                Vec::new()
            },
            references: if want_references {
                self.references.clone()
            } else {
                Vec::new()
            },
            call_frequency: if want_functions {
                self.call_frequency.clone()
            } else {
                HashMap::new()
            },
            calls: if want_calls {
                self.calls.clone()
            } else {
                Vec::new()
            },
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            description = "True when the file extension is not supported; semantic fields are empty"
        )
    )]
    /// True when the file extension is not supported; semantic fields are empty.
    pub unsupported: Option<bool>,
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
            unsupported: None,
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

        // Test AnalyzeDirectoryParams: cursor, force, summary must be top-level
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
            !dir_props.contains_key("page_size"),
            "page_size must be ABSENT from AnalyzeDirectoryParams schema"
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
            !file_props.contains_key("page_size"),
            "page_size must be ABSENT from AnalyzeFileParams schema"
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
            !symbol_props.contains_key("page_size"),
            "page_size must be ABSENT from AnalyzeSymbolParams schema"
        );
        assert!(
            symbol_props.contains_key("summary"),
            "summary must be top-level in AnalyzeSymbolParams schema"
        );

        // Verify verbose is NOT present in either param schema (removed per issue 1129)
        assert!(
            !file_props.contains_key("verbose"),
            "verbose must be removed from AnalyzeFileParams schema"
        );
        assert!(
            !symbol_props.contains_key("verbose"),
            "verbose must be removed from AnalyzeSymbolParams schema"
        );

        // Verify force is NOT present in either param schema (removed per issue 1129)
        assert!(
            !file_props.contains_key("force"),
            "force must be removed from AnalyzeFileParams schema"
        );
        assert!(
            !symbol_props.contains_key("force"),
            "force must be removed from AnalyzeSymbolParams schema"
        );

        // Verify ast_recursion_limit is NOT present in either param schema (removed per issue 1129)
        assert!(
            !file_props.contains_key("ast_recursion_limit"),
            "ast_recursion_limit must be removed from AnalyzeFileParams schema"
        );
        assert!(
            !symbol_props.contains_key("ast_recursion_limit"),
            "ast_recursion_limit must be removed from AnalyzeSymbolParams schema"
        );
    }

    fn make_sa(name: &str, count: usize) -> SemanticAnalysis {
        let mut cf = HashMap::new();
        cf.insert(name.to_string(), count);
        SemanticAnalysis {
            functions: vec![FunctionInfo {
                name: name.to_string(),
                line: 1,
                end_line: 5,
                parameters: vec![],
                return_type: None,
            }],
            classes: vec![],
            imports: vec![],
            references: vec![],
            call_frequency: cf,
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        }
    }

    fn make_sa_with_refs_and_calls() -> SemanticAnalysis {
        let mut sa = make_sa("fn_ref", 1);
        sa.references = vec![ReferenceInfo {
            symbol: "helper".to_string(),
            reference_type: ReferenceType::Usage,
            location: "src/lib.rs".to_string(),
            line: 7,
        }];
        sa.calls = vec![CallInfo {
            caller: "outer".to_string(),
            callee: "inner".to_string(),
            line: 9,
            column: 4,
            arg_count: None,
        }];
        sa
    }

    #[test]
    fn test_project_fields_none_preserves_call_frequency() {
        let sa = make_sa("foo", 3);
        let projected = sa.project(None);
        assert_eq!(projected.functions.len(), 1);
        assert_eq!(projected.call_frequency.len(), 1);
    }

    #[test]
    fn test_project_fields_references_gates_references() {
        // Arrange: an analysis with one reference; project only functions.
        let sa = make_sa_with_refs_and_calls();
        let projected = sa.project(Some(&[AnalyzeFileField::Functions]));
        assert!(projected.references.is_empty());
        assert!(projected.calls.is_empty());

        // Act: project with references requested.
        let projected = sa.project(Some(&[AnalyzeFileField::References]));

        // Assert: references preserved, calls still zero-filled.
        assert_eq!(projected.references.len(), 1);
        assert!(projected.calls.is_empty());
    }

    #[test]
    fn test_project_fields_calls_gates_calls() {
        // Arrange: an analysis with one call; project only calls.
        let sa = make_sa_with_refs_and_calls();
        let projected = sa.project(Some(&[AnalyzeFileField::Calls]));

        // Assert: calls preserved, references zero-filled.
        assert_eq!(projected.calls.len(), 1);
        assert!(projected.references.is_empty());
    }

    #[test]
    fn test_project_fields_none_zeroes_references_and_calls() {
        // Arrange/Act: default projection (no fields requested).
        let sa = make_sa_with_refs_and_calls();
        let projected = sa.project(None);

        // Assert: references/calls zero-filled; other sections preserved.
        assert!(projected.references.is_empty());
        assert!(projected.calls.is_empty());
        assert_eq!(projected.functions.len(), 1);
    }

    #[test]
    fn test_project_fields_all_preserves_call_frequency() {
        let sa = make_sa("bar", 5);
        let projected = sa.project(Some(&[AnalyzeFileField::All]));
        assert_eq!(projected.call_frequency.len(), 1);
    }

    #[test]
    fn test_project_fields_functions_preserves_call_frequency() {
        let sa = make_sa("baz", 2);
        let projected = sa.project(Some(&[AnalyzeFileField::Functions]));
        assert_eq!(projected.call_frequency.len(), 1);
    }

    #[test]
    fn test_project_fields_classes_empties_call_frequency() {
        let mut cf = HashMap::new();
        cf.insert("qux".to_string(), 1usize);
        let sa = SemanticAnalysis {
            functions: vec![],
            classes: vec![],
            imports: vec![],
            references: vec![],
            call_frequency: cf,
            calls: vec![],
            impl_traits: vec![],
            def_use_sites: vec![],
        };
        let projected = sa.project(Some(&[AnalyzeFileField::Classes]));
        assert!(projected.call_frequency.is_empty());
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
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
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
    /// Exact text block to find and replace; must be non-empty. Mutually exclusive with
    /// `edits`: provide either `old_text`/`new_text` (single edit) or `edits` (batch).
    #[serde(default)]
    pub old_text: Option<String>,
    /// Replacement text; empty string deletes every matched block. Mutually exclusive with `edits`.
    #[serde(default)]
    pub new_text: Option<String>,
    /// When `true`, replaces every non-overlapping occurrence of `old_text` in a single pass
    /// (sed `s/old/new/g` semantics). Returns `INVALID_PARAMS` if `old_text` is empty.
    /// When `false`, `old_text` must appear exactly once; fails with `ambiguous` if multiple matches.
    #[serde(default)]
    pub replace_all: Option<bool>,
    /// Blake3 hex hash of the raw file bytes the caller last saw. If the file has changed since the caller last read it, the edit is rejected with `INVALID_PARAMS` directing the caller to re-read. Omit to skip the staleness check (backward compatible).
    #[serde(default)]
    pub expected_content_hash: Option<String>,
    /// Batch edits applied atomically to one file; mutually exclusive with `old_text`/`new_text`.
    #[serde(default)]
    pub edits: Option<Vec<BatchEdit>>,
}

/// One edit item within an `edits[]` batch for `edit_replace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct BatchEdit {
    /// Exact text to find within the file.
    pub old_text: String,
    /// Replacement text.
    pub new_text: String,
    /// When true, replace every non-overlapping occurrence of `old_text`.
    #[serde(default)]
    pub replace_all: Option<bool>,
}

/// Per-edit result within a successful batch `edit_replace` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct BatchEditResult {
    /// Zero-based index of the edit in the `edits[]` request array.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub index: usize,
    /// Number of occurrences replaced by this edit.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub occurrences_replaced: usize,
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub struct EditReplaceOutput {
    /// Path of the file that was edited.
    pub path: String,
    /// File size in bytes before the edit.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub bytes_before: usize,
    /// File size in bytes after the edit.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub bytes_after: usize,
    /// Number of occurrences replaced. Always 1 when `replace_all` is false (default single-match
    /// path). When `replace_all` is true, reflects the actual substitution count (0 triggers a
    /// `not_found` error before this field is populated, so a successful response always has
    /// `occurrences_replaced >= 1`). When the batch (`edits[]`) form is used, this is the
    /// total number of occurrences replaced across all edits in the batch.
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::integer_schema")
    )]
    pub occurrences_replaced: usize,
    /// Blake3 hex hash of the file bytes after the edit. Present only when the batch
    /// (`edits[]`) form was used.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Per-edit results. Present only when the batch (`edits[]`) form was used.
    #[serde(default)]
    pub edits: Option<Vec<BatchEditResult>>,
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
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "crate::schema_helpers::option_integer_schema")
    )]
    pub max_lines: Option<usize>,
    /// Replacement text if filtered output is empty (success-only).
    pub on_empty: Option<String>,
}
