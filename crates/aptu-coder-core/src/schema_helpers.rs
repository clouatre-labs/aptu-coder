// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "schemars")]

use schemars::Schema;
use serde_json::json;

/// Returns a plain integer schema without the non-standard "format": "uint"
/// that schemars emits by default for usize/u32 fields.
pub fn integer_schema(_gen: &mut schemars::SchemaGenerator) -> Schema {
    let map = json!({
        "type": "integer",
        "minimum": 0
    })
    .as_object()
    .expect("json! object literal is always a Value::Object")
    .clone();
    Schema::from(map)
}

/// Returns a nullable integer schema for Option<usize> / Option<u32> fields.
pub fn option_integer_schema(_gen: &mut schemars::SchemaGenerator) -> Schema {
    let map = json!({
        "type": ["integer", "null"],
        "minimum": 0
    })
    .as_object()
    .expect("json! object literal is always a Value::Object")
    .clone();
    Schema::from(map)
}

/// Returns a nullable integer schema for `Option<usize>` `ast_recursion_limit` fields.
/// `None` = library default, `0` = unlimited traversal depth, `n` = limit to n levels.
pub fn option_ast_limit_schema(_gen: &mut schemars::SchemaGenerator) -> Schema {
    let map = json!({
        "type": ["integer", "null"],
        "minimum": 0
    })
    .as_object()
    .expect("json! object literal is always a Value::Object")
    .clone();
    Schema::from(map)
}

/// Regex matching all supported source file extensions (case-insensitive).
///
/// Used as the `inputSchema` `pattern` constraint on `path` fields in
/// `AnalyzeFileParams` and `AnalyzeModuleParams`. Covers every extension in
/// `lang.rs` `EXTENSION_MAP`. Centralised here so adding a language requires
/// one change, not two.
pub const SUPPORTED_FILE_EXT_PATTERN: &str = r"(?i)\.(rs|py|go|ts|tsx|js|mjs|cjs|java|kt|kts|cs|cpp|cc|cxx|c|h|hpp|hxx|f|f77|f90|f95|f03|f08|for|ftn)$";

/// Returns a string schema with a `pattern` constraint covering all supported
/// source file extensions. Used as `schema_with` on `path` fields.
pub fn supported_file_path_schema(_gen: &mut schemars::SchemaGenerator) -> Schema {
    let map = serde_json::json!({
        "type": "string",
        "pattern": SUPPORTED_FILE_EXT_PATTERN
    })
    .as_object()
    .expect("json! object literal is always a Value::Object")
    .clone();
    Schema::from(map)
}

/// Returns a nullable integer schema for `Option<usize>` `page_size` fields.
/// Enforces minimum: 1 to prevent callers from sending `page_size=0`, which
/// would cause `paginate_slice` to make no progress and loop on the same cursor.
pub fn option_page_size_schema(_gen: &mut schemars::SchemaGenerator) -> Schema {
    let map = json!({
        "type": ["integer", "null"],
        "minimum": 1
    })
    .as_object()
    .expect("json! object literal is always a Value::Object")
    .clone();
    Schema::from(map)
}
