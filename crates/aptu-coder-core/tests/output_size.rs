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
        char_count >= 500 && char_count <= 50000,
        "Overview output size {} out of range [500, 50000]",
        char_count
    );
}

#[test]
fn test_file_details_output_size() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let file_path = format!("{}/src/formatter/summary.rs", manifest_dir);
    let output = analyze::analyze_file(&file_path, None).unwrap();
    let char_count = output.formatted.len();

    println!("File details output size: {} chars", char_count);
    assert!(
        char_count >= 500 && char_count <= 30000,
        "File details output size {} out of range [500, 30000]",
        char_count
    );
}

#[test]
fn test_symbol_focus_output_size() {
    let output =
        analyze::analyze_focused(Path::new("src"), "analyze_directory", 2, None, None).unwrap();
    let char_count = output.formatted.len();

    println!("Symbol focus output size: {} chars", char_count);
    assert!(
        char_count >= 100 && char_count <= 10000,
        "Symbol focus output size {} out of range [100, 10000]",
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
