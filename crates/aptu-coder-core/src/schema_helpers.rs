// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "schemars")]

use schemars::Schema;
use serde_json::json;

/// Returns a plain integer schema without the non-standard "format": "uint"
/// that schemars emits by default for usize/u32 fields.
// SAFETY: json! macro always produces a Value::Object for object literals.
#[allow(clippy::expect_used)]
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
// SAFETY: json! macro always produces a Value::Object for object literals.
#[allow(clippy::expect_used)]
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

/// Returns a nullable integer schema for `analyze_symbol`'s `max_depth`
/// field, capped at `MAX_TOOL_DEPTH`.
// SAFETY: json! macro always produces a Value::Object for object literals.
#[allow(clippy::expect_used)]
pub fn max_depth_schema(_gen: &mut schemars::SchemaGenerator) -> Schema {
    let map = json!({
        "type": ["integer", "null"],
        "minimum": 0,
        "maximum": MAX_TOOL_DEPTH
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
///
/// JSON Schema `pattern` is an ECMAScript regex, where the `(?i)` inline
/// flag is a `SyntaxError`, so case-insensitivity is expressed with
/// per-character classes instead. Multi-char extensions precede shared
/// single-char prefixes in the alternation.
pub const SUPPORTED_FILE_EXT_PATTERN: &str = concat!(
    r"\.(?:",
    r"[Rr][Ss]|[Pp][Yy]|[Gg][Oo]|[Tt][Ss][Xx]|[Tt][Ss]|[Jj][Ss]|[Mm][Jj][Ss]|[Cc][Jj][Ss]|",
    r"[Jj][Aa][Vv][Aa]|[Kk][Tt][Ss]|[Kk][Tt]|[Cc][Ss]|[Cc][Pp][Pp]|[Cc][Xx][Xx]|[Cc][Cc]|[Cc]|",
    r"[Hh][Pp][Pp]|[Hh][Xx][Xx]|[Hh]|[Ff]77|[Ff]90|[Ff]95|[Ff]03|[Ff]08|[Ff][Oo][Rr]|[Ff][Tt][Nn]|[Ff]|",
    r"[Hh][Tt][Mm][Ll]|[Hh][Tt][Mm]|[Mm][Dd][Xx]|[Mm][Dd]|[Aa][Ss][Tt][Rr][Oo]|[Cc][Ss][Ss]|",
    r"[Yy][Aa][Mm][Ll]|[Yy][Mm][Ll]|[Jj][Ss][Oo][Nn]|[Tt][Oo][Mm][Ll]",
    r")$"
);

/// Hard cap on `analyze_symbol`'s `max_depth` parameter (graph traversal
/// depth). Usage data (this
/// repo + goose sessions) never exceeded 2; external comparables cap at 5-6,
/// but that permits ~25x worst-case blowup for exponential graph output, so 3
/// is one hop of headroom over observed max.
pub const MAX_TOOL_DEPTH: u32 = 3;

/// Returns a string schema with a `pattern` constraint covering all supported
/// source file extensions. Used as `schema_with` on `path` fields.
// SAFETY: json! macro always produces a Value::Object for object literals.
#[allow(clippy::expect_used)]
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
