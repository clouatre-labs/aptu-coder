// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::types::{AnalyzeDirectoryParams, AnalyzeSymbolParams, FileInfo};

fn prop_value<T: schemars::JsonSchema>(prop: &str, key: &str) -> Option<serde_json::Value> {
    let value = serde_json::to_value(schemars::schema_for!(T)).ok()?;
    value.get("properties")?.get(prop)?.get(key).cloned()
}

#[test]
fn descriptions_match_previous_attribute_text() {
    let expected = "Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering.";
    for params in [
        prop_value::<AnalyzeDirectoryParams>("git_ref", "description"),
        prop_value::<AnalyzeSymbolParams>("git_ref", "description"),
    ] {
        assert_eq!(params, Some(serde_json::json!(expected)));
    }
    assert_eq!(
        prop_value::<AnalyzeDirectoryParams>("git_ref", "examples"),
        Some(serde_json::json!(["main"]))
    );
    assert_eq!(
        prop_value::<AnalyzeSymbolParams>("git_ref", "examples"),
        Some(serde_json::json!(["main"]))
    );
    assert!(
        prop_value::<AnalyzeSymbolParams>("mode", "description")
            .unwrap_or_default()
            .as_str()
            .unwrap_or_default()
            .starts_with("Analysis mode.")
    );
    assert_eq!(
        prop_value::<FileInfo>("path", "description"),
        Some(serde_json::json!(
            "Path relative to the analyzed directory (absolute when outside base)."
        ))
    );
}
