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

/// Builds an ECMAScript-valid regex matching all supported source file
/// extensions (case-insensitive).
///
/// Used as the `inputSchema` `pattern` constraint on `path` fields in
/// `AnalyzeFileParams` and `AnalyzeModuleParams`. Generated from
/// `lang.rs` `EXTENSION_MAP` (via `supported_extensions()`), so adding a
/// language requires one change, not two.
///
/// JSON Schema `pattern` is an ECMAScript regex, where the `(?i)` inline
/// flag is a `SyntaxError`, so case-insensitivity is expressed with
/// per-character classes instead. Longer extensions precede shared shorter
/// prefixes in the alternation so the regex engine reaches the correct
/// branch before `$` fails on a shorter match.
pub fn supported_file_ext_pattern() -> &'static str {
    static PATTERN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        let mut exts = crate::lang::supported_extensions();
        exts.sort_by_key(|ext| std::cmp::Reverse(ext.len()));
        let alternation = exts
            .iter()
            .map(|ext| {
                ext.chars()
                    .map(|c| {
                        if c.is_ascii_alphabetic() {
                            format!("[{}{}]", c.to_ascii_uppercase(), c.to_ascii_lowercase())
                        } else {
                            c.to_string()
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("|");
        format!(r"\.(?:{alternation})$")
    })
}

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
        "pattern": crate::schema_helpers::supported_file_ext_pattern()
    })
    .as_object()
    .expect("json! object literal is always a Value::Object")
    .clone();
    Schema::from(map)
}
