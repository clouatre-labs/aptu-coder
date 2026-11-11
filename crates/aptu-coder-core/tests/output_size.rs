// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::analyze;
use std::path::Path;

#[test]
fn test_overview_output_size() {
    let output = analyze::analyze_directory(Path::new("."), None).unwrap();
    let char_count = output.formatted.len();

    println!("Overview output size: {} chars", char_count);
    assert!(
        (500..=50000).contains(&char_count),
        "Overview output size {} out of range [500, 50000]",
        char_count
    );
}

#[test]
fn test_file_details_output_size() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let file_path = format!("{}/src/graph/call_graph.rs", manifest_dir);
    let output = analyze::analyze_file(&file_path, None).unwrap();
    let char_count = output.formatted.len();

    println!("File details output size: {} chars", char_count);
    assert!(
        (500..=30000).contains(&char_count),
        "File details output size {} out of range [500, 30000]",
        char_count
    );
}

#[test]
fn test_summary_mode_produces_smaller_output() {
    use aptu_coder_core::formatter::format_summary;

    let output = analyze::analyze_directory(Path::new("."), None).unwrap();
    let full_len = output.formatted.len();
    let summarized = format_summary(&output.entries, &output.files, None, None);
    let summary_len = summarized.len();

    assert!(
        summary_len < full_len,
        "summary output ({}) should be smaller than full output ({})",
        summary_len,
        full_len
    );
}
