// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::analyze::{analyze_directory, analyze_file};
use tempfile::TempDir;

#[test]
fn test_acceptance_overview_mode() {
    // This test requires the aptu repo to be cloned
    // For now, we test with a local temp directory
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a simple project structure
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn helper() {}").unwrap();
    std::fs::write(root.join("README.md"), "# Test Project").unwrap();

    let output = analyze_directory(root, Some(2)).unwrap();

    // Verify output format
    assert!(output.formatted.contains("SUMMARY:"));
    assert!(output.formatted.contains("PATH [LOC, FUNCTIONS, CLASSES]"));
    assert!(output.formatted.contains("main.rs"));
    assert!(output.formatted.contains("lib.rs"));
    assert!(!output.files.is_empty());
}

#[test]
fn test_acceptance_file_details_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");

    let rust_code = r#"
use std::collections::HashMap;

pub struct Point {
    x: i32,
    y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    pub fn distance(&self) -> f64 {
        ((self.x * self.x + self.y * self.y) as f64).sqrt()
    }
}

pub fn calculate(a: i32, b: i32) -> i32 {
    a + b
}
"#;

    std::fs::write(&file_path, rust_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify output contains expected sections
    assert!(output.formatted.contains("FILE:"));
    assert!(output.formatted.contains("test.rs"));
    assert!(output.formatted.contains("F:"));
    assert!(output.formatted.contains("C:"));
    assert!(output.formatted.contains("I:"));

    // Verify semantic analysis
    assert_eq!(output.semantic.functions.len(), 3); // new, distance, calculate
    assert_eq!(output.semantic.classes.len(), 1); // Point
    assert_eq!(output.semantic.imports.len(), 1); // HashMap import
}
