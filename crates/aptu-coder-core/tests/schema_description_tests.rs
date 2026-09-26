// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::types::{AnalyzeDirectoryParams, AnalyzeSymbolParams, FileInfo};

fn prop_desc<T: schemars::JsonSchema>(prop: &str) -> Option<String> {
    let value = serde_json::to_value(schemars::schema_for!(T)).ok()?;
    value
        .get("properties")?
        .get(prop)?
        .get("description")?
        .as_str()
        .map(String::from)
}

#[test]
fn descriptions_match_previous_attribute_text() {
    let expected = "Restrict analysis to files changed relative to this git ref (branch, tag, or commit SHA). Empty string or unset means no filtering.";
    assert_eq!(
        prop_desc::<AnalyzeDirectoryParams>("git_ref").as_deref(),
        Some(expected)
    );
    assert_eq!(
        prop_desc::<AnalyzeSymbolParams>("git_ref").as_deref(),
        Some(expected)
    );
    assert!(
        prop_desc::<AnalyzeSymbolParams>("mode")
            .unwrap_or_default()
            .starts_with("Analysis mode.")
    );
    assert_eq!(
        prop_desc::<FileInfo>("path").as_deref(),
        Some("Path relative to the analyzed directory (absolute when outside base).")
    );
}
