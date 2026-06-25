// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::analyze::{analyze_directory, analyze_file, analyze_module_file};
use std::path::Path;

#[tokio::test]
async fn test_analyze_directory_is_idempotent() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let path_str = path.to_str().unwrap();

    let result1 = analyze_directory(Path::new(path_str), None).unwrap();
    let result2 = analyze_directory(Path::new(path_str), None).unwrap();

    // Compare file count and order
    assert_eq!(
        result1.files.len(),
        result2.files.len(),
        "file count must be stable"
    );

    // Compare all file paths are in identical order
    let paths1: Vec<&str> = result1.files.iter().map(|f| f.path.as_str()).collect();
    let paths2: Vec<&str> = result2.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths1, paths2,
        "file paths must be in identical order across calls"
    );

    // Compare formatted output is identical (determinism)
    assert_eq!(
        result1.formatted, result2.formatted,
        "formatted output must be byte-identical"
    );
}

#[tokio::test]
async fn test_analyze_file_is_idempotent() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/formatter/mod.rs");
    let path_str = path.to_str().unwrap();

    let result1 = analyze_file(path_str, None).unwrap();
    let result2 = analyze_file(path_str, None).unwrap();

    // Compare function count
    assert_eq!(
        result1.semantic.functions.len(),
        result2.semantic.functions.len(),
        "function count must be stable"
    );

    // Compare function names in identical order
    let names1: Vec<&str> = result1
        .semantic
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let names2: Vec<&str> = result2
        .semantic
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(
        names1, names2,
        "function names must be in identical order across calls"
    );

    // Compare formatted output
    assert_eq!(
        result1.formatted, result2.formatted,
        "formatted output must be byte-identical"
    );
}

#[tokio::test]
async fn test_analyze_module_is_idempotent() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/formatter/mod.rs");
    let path_str = path.to_str().unwrap();

    let result1 = analyze_module_file(path_str).unwrap();
    let result2 = analyze_module_file(path_str).unwrap();

    assert_eq!(result1.name, result2.name, "module name must be stable");

    // Compare function names in identical order
    let fn1: Vec<&str> = result1.functions.iter().map(|f| f.name.as_str()).collect();
    let fn2: Vec<&str> = result2.functions.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fn1, fn2, "function names must be identical across calls");

    // Compare imports in identical order
    let imp1: Vec<&str> = result1.imports.iter().map(|i| i.module.as_str()).collect();
    let imp2: Vec<&str> = result2.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(imp1, imp2, "imports must be identical across calls");
}

#[tokio::test]
async fn test_traversal_sort_idempotent() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let path_str = path.to_str().unwrap();

    let result1 = analyze_directory(Path::new(path_str), None).unwrap();
    let result2 = analyze_directory(Path::new(path_str), None).unwrap();

    // Verify that walk entries are in sorted order (deterministic traversal)
    let mut paths1: Vec<&str> = result1
        .entries
        .iter()
        .map(|e| e.path.to_str().unwrap())
        .collect();
    let mut paths2: Vec<&str> = result2
        .entries
        .iter()
        .map(|e| e.path.to_str().unwrap())
        .collect();

    paths1.sort();
    paths2.sort();

    // Both runs should have same paths
    assert_eq!(paths1, paths2, "all entries must be consistent across runs");

    // Verify entries from first result are already sorted
    let entries1_paths: Vec<&str> = result1
        .entries
        .iter()
        .map(|e| e.path.to_str().unwrap())
        .collect();
    let mut entries1_sorted = entries1_paths.clone();
    entries1_sorted.sort();
    assert_eq!(
        entries1_paths, entries1_sorted,
        "entries must be sorted by path"
    );
}
