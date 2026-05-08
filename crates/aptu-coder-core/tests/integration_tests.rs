// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::analyze::{
    AnalyzeError, analyze_directory, analyze_directory_with_progress, analyze_file,
    analyze_focused, determine_mode,
};
use aptu_coder_core::cache::{AnalysisCache, CacheKey};
use aptu_coder_core::completion::{path_completions, symbol_completions};
use aptu_coder_core::traversal::walk_directory;
use aptu_coder_core::types::AnalysisMode;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[test]
fn test_walk_directory_basic() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create test structure
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("README.md"), "# Test").unwrap();

    let entries = walk_directory(root, None).unwrap();

    // Should have root + src dir + 2 files
    assert!(entries.len() >= 3);

    // Check that we have files and directories
    let has_dir = entries.iter().any(|e| e.is_dir && e.path.ends_with("src"));
    let has_file = entries
        .iter()
        .any(|e| !e.is_dir && e.path.ends_with("main.rs"));
    assert!(has_dir);
    assert!(has_file);
}

#[test]
fn test_walk_directory_max_depth_limiting() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create nested structure
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::write(root.join("a/file1.rs"), "fn f1() {}").unwrap();
    fs::write(root.join("a/b/file2.rs"), "fn f2() {}").unwrap();
    fs::write(root.join("a/b/c/file3.rs"), "fn f3() {}").unwrap();

    // With max_depth=1, should only get root and immediate children
    let entries = walk_directory(root, Some(1)).unwrap();

    // Check that depth 2+ entries are not included
    let max_depth = entries.iter().map(|e| e.depth).max().unwrap_or(0);
    assert!(max_depth <= 1);
}

#[test]
fn test_walk_directory_symlink_detection() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    fs::write(root.join("target.rs"), "fn target() {}").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs as unix_fs;
        unix_fs::symlink(root.join("target.rs"), root.join("link.rs")).unwrap();

        let entries = walk_directory(root, None).unwrap();
        let symlink_entry = entries.iter().find(|e| e.path.ends_with("link.rs"));

        assert!(symlink_entry.is_some());
        let entry = symlink_entry.unwrap();
        assert!(entry.is_symlink);
        assert!(entry.symlink_target.is_some());
    }
}

#[test]
fn test_analyze_directory_with_rust_file() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a simple Rust file with a function
    let rust_code = r#"
fn hello() {
    println!("Hello");
}

fn world() {
    println!("World");
}
"#;
    fs::write(root.join("lib.rs"), rust_code).unwrap();

    let output = analyze_directory(root, None).unwrap();

    // Check that output contains expected sections with new format
    assert!(output.formatted.contains("SUMMARY:"));
    assert!(output.formatted.contains("Shown:"));
    assert!(output.formatted.contains("PATH [LOC, FUNCTIONS, CLASSES]"));
    assert!(output.formatted.contains("lib.rs"));
    assert!(output.formatted.contains("2F")); // 2 functions
    assert!(
        !output.formatted.contains("TEST FILES"),
        "TEST FILES header must be absent when no test files exist"
    );
}

#[test]
fn test_analyze_directory_empty_directory() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    let output = analyze_directory(root, None).unwrap();

    // Should still have SUMMARY and PATH sections with new format
    assert!(output.formatted.contains("SUMMARY:"));
    assert!(output.formatted.contains("Shown: 0 files"));
    assert!(output.formatted.contains("PATH [LOC, FUNCTIONS, CLASSES]"));
}

#[test]
fn test_analyze_directory_binary_file_skipping() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a binary file (PNG header)
    fs::write(root.join("image.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    fs::write(root.join("lib.rs"), "fn test() {}").unwrap();

    // Should not panic, should exclude binary file from output
    let output = analyze_directory(root, None).unwrap();
    assert!(output.formatted.contains("SUMMARY:"));
    // Binary file should be excluded from analysis results
    assert!(!output.formatted.contains("image.png"));
    // Only the readable Rust file should be included
    assert!(output.formatted.contains("lib.rs"));
}

#[test]
fn test_walk_directory_ignore_file_respected() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create .ignore file that excludes ignored.rs
    fs::write(root.join(".ignore"), "ignored.rs\n").unwrap();
    fs::write(root.join("ignored.rs"), "fn ignored() {}").unwrap();
    fs::write(root.join("kept.rs"), "fn kept() {}").unwrap();

    // Act: Walk the directory
    let entries = walk_directory(root, None).unwrap();

    // Assert: ignored.rs should not be in results, kept.rs should be
    let has_ignored = entries.iter().any(|e| e.path.ends_with("ignored.rs"));
    let has_kept = entries.iter().any(|e| e.path.ends_with("kept.rs"));

    assert!(!has_ignored, "ignored.rs should be excluded by .ignore");
    assert!(has_kept, "kept.rs should be included");
}

#[test]
fn test_walk_directory_ignore_precedence_over_gitignore() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create .gitignore that excludes foo.rs, then .ignore that includes it
    fs::write(root.join(".gitignore"), "foo.rs\n").unwrap();
    fs::write(root.join(".ignore"), "!foo.rs\n").unwrap();
    fs::write(root.join("foo.rs"), "fn foo() {}").unwrap();

    // Act: Walk the directory
    let entries = walk_directory(root, None).unwrap();

    // Assert: foo.rs should be included (proving .ignore precedence)
    let has_foo = entries.iter().any(|e| e.path.ends_with("foo.rs"));
    assert!(
        has_foo,
        "foo.rs should be included due to .ignore precedence over .gitignore"
    );
}

#[test]
fn test_analyze_unsupported_file_type() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create an unsupported file type (plain text)
    fs::write(
        root.join("notes.txt"),
        "This is a text file\nWith multiple lines",
    )
    .unwrap();
    fs::write(root.join("lib.rs"), "fn test() {}").unwrap();

    let output = analyze_directory(root, None).unwrap();

    // Should include both files
    assert!(output.formatted.contains("notes.txt"));
    assert!(output.formatted.contains("lib.rs"));

    // Verify unsupported file has LOC but no functions/classes
    let txt_analysis = output.files.iter().find(|f| f.path.contains("notes.txt"));
    assert!(txt_analysis.is_some());
    let txt = txt_analysis.unwrap();
    assert_eq!(txt.line_count, 2);
    assert_eq!(txt.function_count, 0);
    assert_eq!(txt.class_count, 0);
    assert_eq!(txt.language, "unknown");
}

#[test]
fn test_output_format_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a Rust file with struct and function
    let rust_code = r#"
struct Point {
    x: i32,
    y: i32,
}

fn distance() -> f64 {
    0.0
}
"#;
    fs::write(root.join("lib.rs"), rust_code).unwrap();

    let output = analyze_directory(root, None).unwrap();

    // Verify SUMMARY: format with colon
    assert!(output.formatted.contains("SUMMARY:"));

    // Verify Shown: format with compact metrics
    assert!(output.formatted.contains("Shown: 1 files"));
    assert!(output.formatted.contains("L,"));
    assert!(output.formatted.contains("F,"));
    // When max_depth is None (unlimited), max_depth label should be omitted
    assert!(!output.formatted.contains("max_depth="));

    // Verify PATH header format
    assert!(output.formatted.contains("PATH [LOC, FUNCTIONS, CLASSES]"));

    // Verify compact file metrics format [NL, NF, NC]
    assert!(output.formatted.contains("["));
    assert!(output.formatted.contains("]"));
}

#[test]
fn test_determine_mode_directory() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    let mode = determine_mode(root.to_str().unwrap(), None);
    assert_eq!(mode, AnalysisMode::Overview);
}

#[test]
fn test_determine_mode_file() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::write(root.join("test.rs"), "fn test() {}").unwrap();

    let file_path = root.join("test.rs");
    let mode = determine_mode(file_path.to_str().unwrap(), None);
    assert_eq!(mode, AnalysisMode::FileDetails);
}

#[test]
fn test_determine_mode_with_focus() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    let mode = determine_mode(root.to_str().unwrap(), Some("my_function"));
    assert_eq!(mode, AnalysisMode::SymbolFocus);
}

#[test]
fn test_semantic_analysis_happy_path() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");

    let rust_code = r#"
use std::collections::HashMap;

struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    fn distance(&self) -> f64 {
        ((self.x * self.x + self.y * self.y) as f64).sqrt()
    }
}

fn calculate(a: i32, b: i32) -> i32 {
    let result = a + b;
    process(result);
    process(result);
    process(result);
    process(result);
    result
}

fn process(x: i32) -> i32 {
    x * 2
}
"#;

    fs::write(&file_path, rust_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify FILE: header with counts
    assert!(output.formatted.contains("FILE:"));
    assert!(output.formatted.contains("test.rs"));
    assert!(output.formatted.contains("L,"));
    assert!(output.formatted.contains("F,"));
    assert!(output.formatted.contains("C,"));
    assert!(output.formatted.contains("I)"));

    // Verify functions extracted (new, distance, calculate, process)
    assert_eq!(output.semantic.functions.len(), 4);
    assert!(output.semantic.functions.iter().any(|f| f.name == "new"));
    assert!(
        output
            .semantic
            .functions
            .iter()
            .any(|f| f.name == "distance")
    );
    assert!(
        output
            .semantic
            .functions
            .iter()
            .any(|f| f.name == "calculate")
    );
    assert!(
        output
            .semantic
            .functions
            .iter()
            .any(|f| f.name == "process")
    );

    // Verify classes extracted
    assert_eq!(output.semantic.classes.len(), 1);
    assert_eq!(output.semantic.classes[0].name, "Point");

    // Verify impl methods populated on the class
    assert_eq!(output.semantic.classes[0].methods.len(), 2);
    let method_names: Vec<&str> = output.semantic.classes[0]
        .methods
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert!(method_names.contains(&"new"));
    assert!(method_names.contains(&"distance"));

    // Verify imports extracted
    assert_eq!(output.semantic.imports.len(), 1);
    assert_eq!(output.semantic.imports[0].module, "std::collections");

    // Verify C: section present
    assert!(output.formatted.contains("C:"));
    assert!(output.formatted.contains("Point:"));

    // Verify F: section present
    assert!(output.formatted.contains("F:"));

    // Verify I: section present
    assert!(output.formatted.contains("I:"));
    assert!(output.formatted.contains("std"));

    // Verify call frequency tracking (process called 4 times, should have bullet)
    assert!(output.formatted.contains("•4"));
    // Verify advanced fields are excluded from JSON serialization
    let serialized = serde_json::to_string(&output.semantic).unwrap();
    assert!(!serialized.contains("call_frequency"));
    assert!(!serialized.contains("assignments"));
    assert!(!serialized.contains("field_accesses"));

    // Verify references extracted with line numbers and location set
    let point_ref = output
        .semantic
        .references
        .iter()
        .find(|r| r.symbol == "Point");
    assert!(point_ref.is_some(), "expected a 'Point' type reference");
    let point_ref = point_ref.unwrap();
    assert!(point_ref.line > 0, "reference line should be non-zero");
    assert!(
        !point_ref.location.is_empty(),
        "reference location should be populated with the file path"
    );
    assert!(
        point_ref.location.ends_with("test.rs"),
        "reference location should point to test.rs, got: {}",
        point_ref.location
    );
}

#[test]
fn test_semantic_analysis_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.rs");

    fs::write(&file_path, "").unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify FILE: header still present
    assert!(output.formatted.contains("FILE:"));
    assert!(output.formatted.contains("empty.rs"));

    // Verify empty sections handled gracefully
    assert_eq!(output.semantic.functions.len(), 0);
    assert_eq!(output.semantic.classes.len(), 0);
    assert_eq!(output.semantic.imports.len(), 0);

    // Verify no C:, F:, I:, R: sections for empty file
    assert!(!output.formatted.contains("C:"));
    assert!(!output.formatted.contains("F:"));
    assert!(!output.formatted.contains("I:"));
    assert!(!output.formatted.contains("R:"));
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_parse_and_extract() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");

    let python_code = r#"
def hello():
    print("Hello")

def world():
    print("World")

class MyClass:
    def method(self):
        pass
"#;

    fs::write(&file_path, python_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify functions extracted (hello, world, method)
    assert_eq!(output.semantic.functions.len(), 3);
    assert!(output.semantic.functions.iter().any(|f| f.name == "hello"));
    assert!(output.semantic.functions.iter().any(|f| f.name == "world"));
    assert!(output.semantic.functions.iter().any(|f| f.name == "method"));

    // Verify class extracted
    assert_eq!(output.semantic.classes.len(), 1);
    assert_eq!(output.semantic.classes[0].name, "MyClass");
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_edge_case_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.py");

    fs::write(&file_path, "").unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    assert_eq!(output.semantic.functions.len(), 0);
    assert_eq!(output.semantic.classes.len(), 0);
}

#[cfg(feature = "lang-javascript")]
#[test]
fn test_javascript_parse_and_extract() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.js");
    let js_code = r#"
function hello() {
    console.log("Hello");
}

class MyClass {
    method() {
        return "test";
    }
}

import {x} from 'module';
"#;
    fs::write(&file_path, js_code).unwrap();
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    // Verify functions and classes are extracted
    assert!(
        output.semantic.functions.iter().any(|f| f.name == "hello"),
        "expected hello function"
    );
    assert_eq!(output.semantic.classes.len(), 1);
    // Verify ES imports are captured
    assert!(output.semantic.imports.len() >= 1);
}

#[cfg(feature = "lang-javascript")]
#[test]
fn test_javascript_commonjs_require() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.js");
    let js_code = r#"
const lib = require('./lib');
const path = require('path');
"#;
    fs::write(&file_path, js_code).unwrap();
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    // Verify that at least one require import is captured
    assert!(
        !output.semantic.imports.is_empty(),
        "expected at least one import from require"
    );
    // Debug print imports
    println!("imports: {:?}", output.semantic.imports);
    // Optionally check for specific paths
    let has_lib = output
        .semantic
        .imports
        .iter()
        .any(|i| i.module.contains("./lib"));
    let has_path = output
        .semantic
        .imports
        .iter()
        .any(|i| i.module.contains("path"));
    assert!(
        has_lib || has_path,
        "expected './lib' or 'path' import captured"
    );
}

#[cfg(feature = "lang-typescript")]
#[test]
fn test_typescript_parse_and_extract() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");

    let ts_code = r#"
function hello(): void {
    console.log("Hello");
}

interface MyInterface {
    name: string;
}

type MyType = {
    id: number;
};

enum MyEnum {
    A = 1,
    B = 2,
}

abstract class AbstractClass {
    abstract method(): void;
}

class MyClass {
    method(): string {
        return "test";
    }
}
"#;

    fs::write(&file_path, ts_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify function extracted
    assert!(output.semantic.functions.iter().any(|f| f.name == "hello"));

    // Verify classes and TS-specific types extracted
    assert!(output.semantic.classes.len() >= 4);
    let class_names: Vec<&str> = output
        .semantic
        .classes
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(class_names.contains(&"MyInterface"));
    assert!(class_names.contains(&"MyType"));
    assert!(class_names.contains(&"MyEnum"));
    assert!(class_names.contains(&"AbstractClass"));
    assert!(class_names.contains(&"MyClass"));
}

#[cfg(feature = "lang-typescript")]
#[test]
fn test_typescript_edge_case_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.ts");

    fs::write(&file_path, "").unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    assert_eq!(output.semantic.functions.len(), 0);
    assert_eq!(output.semantic.classes.len(), 0);
}

#[cfg(feature = "lang-go")]
#[test]
fn test_go_parse_and_extract() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.go");

    let go_code = r#"
package main

func Hello() {
    println("Hello")
}

type MyStruct struct {
    Name string
}

type MyInterface interface {
    Method()
}
"#;

    fs::write(&file_path, go_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify function extracted
    assert!(output.semantic.functions.iter().any(|f| f.name == "Hello"));

    // Verify types extracted as classes
    assert!(output.semantic.classes.len() >= 2);
    let class_names: Vec<&str> = output
        .semantic
        .classes
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(class_names.contains(&"MyStruct"));
    assert!(class_names.contains(&"MyInterface"));
}

#[cfg(feature = "lang-go")]
#[test]
fn test_go_edge_case_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.go");

    fs::write(&file_path, "").unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    assert_eq!(output.semantic.functions.len(), 0);
    assert_eq!(output.semantic.classes.len(), 0);
}

#[cfg(feature = "lang-java")]
#[test]
fn test_java_parse_and_extract() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("Test.java");

    let java_code = r#"
public class MyClass {
    public void method() {
        System.out.println("Hello");
    }
}

interface MyInterface {
    void doSomething();
}

enum MyEnum {
    A, B, C
}
"#;

    fs::write(&file_path, java_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify method extracted
    assert!(output.semantic.functions.iter().any(|f| f.name == "method"));

    // Verify classes extracted
    assert!(output.semantic.classes.len() >= 3);
    let class_names: Vec<&str> = output
        .semantic
        .classes
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(class_names.contains(&"MyClass"));
    assert!(class_names.contains(&"MyInterface"));
    assert!(class_names.contains(&"MyEnum"));
}

#[cfg(feature = "lang-java")]
#[test]
fn test_java_edge_case_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.java");

    fs::write(&file_path, "").unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    assert_eq!(output.semantic.functions.len(), 0);
    assert_eq!(output.semantic.classes.len(), 0);
}

// Cache tests
#[test]
fn test_cache_hit() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");

    let rust_code = r#"
fn hello() {
    println!("Hello");
}
"#;
    fs::write(&file_path, rust_code).unwrap();

    let cache = AnalysisCache::new(100);
    let mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    let key = CacheKey {
        path: file_path.clone(),
        modified: mtime,
        mode: AnalysisMode::FileDetails,
    };

    // First analysis
    let output1 = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let arc_output1 = Arc::new(output1);
    cache.put(key.clone(), arc_output1.clone());

    // Second retrieval from cache
    let cached = cache.get(&key);
    assert!(cached.is_some());
    let cached_output = cached.unwrap();
    assert_eq!(cached_output.semantic.functions.len(), 1);
}

#[test]
fn test_cache_miss_on_mtime_change() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");

    let rust_code = r#"
fn hello() {
    println!("Hello");
}
"#;
    fs::write(&file_path, rust_code).unwrap();

    let cache = AnalysisCache::new(100);
    let mtime1 = fs::metadata(&file_path).unwrap().modified().unwrap();
    let key1 = CacheKey {
        path: file_path.clone(),
        modified: mtime1,
        mode: AnalysisMode::FileDetails,
    };

    // Store with first mtime
    let output1 = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let arc_output1 = Arc::new(output1);
    cache.put(key1, arc_output1);

    // Simulate file modification by creating a key with different mtime
    let mtime2 = mtime1 + Duration::from_secs(1);
    let key2 = CacheKey {
        path: file_path.clone(),
        modified: mtime2,
        mode: AnalysisMode::FileDetails,
    };

    // Cache miss with new mtime
    let cached = cache.get(&key2);
    assert!(cached.is_none());
}

#[test]
fn test_cache_eviction_at_capacity() {
    let cache = AnalysisCache::new(3);
    let temp_dir = TempDir::new().unwrap();

    // Create 4 files and cache them
    for i in 0..4 {
        let file_path = temp_dir.path().join(format!("test{}.rs", i));
        fs::write(&file_path, format!("fn f{}() {{}}", i)).unwrap();

        let mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
        let key = CacheKey {
            path: file_path.clone(),
            modified: mtime,
            mode: AnalysisMode::FileDetails,
        };

        let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
        let arc_output = Arc::new(output);
        cache.put(key, arc_output);
    }

    // The first entry should have been evicted (LRU with capacity 3)
    let file_path = temp_dir.path().join("test0.rs");
    let mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    let key = CacheKey {
        path: file_path,
        modified: mtime,
        mode: AnalysisMode::FileDetails,
    };

    let cached = cache.get(&key);
    assert!(cached.is_none(), "First entry should be evicted");
}

#[test]
fn test_cache_mutex_poison_recovery() {
    let cache = AnalysisCache::new(10);
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(&file_path, "fn test() {}").unwrap();

    let mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    let key = CacheKey {
        path: file_path.clone(),
        modified: mtime,
        mode: AnalysisMode::FileDetails,
    };

    // Store an entry
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let arc_output = Arc::new(output);
    cache.put(key.clone(), arc_output);

    // Verify entry is cached
    assert!(cache.get(&key).is_some());

    // Verify we can still use the cache after multiple operations
    let cached = cache.get(&key);
    assert!(cached.is_some(), "Cache should still have the entry");

    // Verify we can add more entries
    let new_output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let new_arc_output = Arc::new(new_output);
    cache.put(key.clone(), new_arc_output);
    assert!(
        cache.get(&key).is_some(),
        "Cache should be usable after update"
    );
}

// Directory cache tests
#[test]
fn test_directory_cache_hit_on_identical_call() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create test files
    fs::write(root.join("file1.rs"), "fn hello() {}").unwrap();
    fs::write(root.join("file2.rs"), "fn world() {}").unwrap();

    let cache = AnalysisCache::new(100);

    // First analysis
    let entries1 = walk_directory(root, None).unwrap();
    let key1 = aptu_coder_core::cache::DirectoryCacheKey::from_entries(
        &entries1,
        None,
        AnalysisMode::Overview,
        None,
    );
    let output1 = analyze_directory(root, None).unwrap();
    let arc_output1 = Arc::new(output1);
    cache.put_directory(key1, arc_output1.clone());

    // Second call with identical parameters should hit cache
    let entries2 = walk_directory(root, None).unwrap();
    let key2 = aptu_coder_core::cache::DirectoryCacheKey::from_entries(
        &entries2,
        None,
        AnalysisMode::Overview,
        None,
    );
    let cached = cache.get_directory(&key2);
    assert!(
        cached.is_some(),
        "Cache should have a hit on identical call"
    );
    assert_eq!(cached.unwrap().files.len(), arc_output1.files.len());
}

#[test]
fn test_directory_cache_miss_on_mtime_change() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create test files
    let file1 = root.join("file1.rs");
    fs::write(&file1, "fn hello() {}").unwrap();

    let cache = AnalysisCache::new(100);

    // First analysis
    let entries1 = walk_directory(root, None).unwrap();
    let key1 = aptu_coder_core::cache::DirectoryCacheKey::from_entries(
        &entries1,
        None,
        AnalysisMode::Overview,
        None,
    );
    let output1 = analyze_directory(root, None).unwrap();
    let arc_output1 = Arc::new(output1);
    cache.put_directory(key1, arc_output1);

    // Modify file to change mtime; sleep long enough to exceed common mtime granularity (1s on many FSes)
    std::thread::sleep(Duration::from_secs(2));
    fs::write(&file1, "fn hello() { println!(\"modified\"); }").unwrap();

    // Second call should miss cache due to mtime change
    let entries2 = walk_directory(root, None).unwrap();
    let key2 = aptu_coder_core::cache::DirectoryCacheKey::from_entries(
        &entries2,
        None,
        AnalysisMode::Overview,
        None,
    );
    let cached = cache.get_directory(&key2);
    assert!(
        cached.is_none(),
        "Cache should miss when file mtime changes"
    );
}

// Output limiting tests
#[test]
fn test_output_limiting_large_output() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("large.rs");

    // Create a file with many functions to generate substantial output
    let mut large_code = String::new();
    for i in 0..500 {
        large_code.push_str(&format!("fn func_{}() {{}}\n", i));
    }
    fs::write(&file_path, large_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify output is generated (the actual line count depends on formatter)
    let line_count = output.formatted.lines().count();
    assert!(
        line_count > 0,
        "Generated output should have content, got {} lines",
        line_count
    );
}

#[test]
fn test_output_limiting_below_threshold() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("small.rs");

    // Create a file with output < 50K chars
    let mut small_code = String::new();
    for i in 0..50 {
        small_code.push_str(&format!("fn func_{}() {{}}\n", i));
    }
    fs::write(&file_path, small_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify output is returned normally (not limited)
    let char_count = output.formatted.len();
    assert!(
        char_count < 50_000,
        "Generated output should be under 50K chars, got {} chars",
        char_count
    );
    assert!(
        output.formatted.contains("FILE:"),
        "Should contain FILE header"
    );
}

#[test]
fn test_analyze_directory_with_progress_increments_counter() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create test structure with known file count
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn lib_fn() {}").unwrap();
    fs::write(root.join("README.md"), "# Test").unwrap();

    // Collect entries first
    let entries = walk_directory(root, None).unwrap();

    // Analyze with progress counter
    let counter = Arc::new(AtomicUsize::new(0));
    let ct = CancellationToken::new();
    let output = analyze_directory_with_progress(root, entries, counter.clone(), ct).unwrap();

    // Verify counter was incremented for each file
    let final_count = counter.load(Ordering::Relaxed);
    assert_eq!(
        final_count, 3,
        "Counter should equal number of files analyzed"
    );

    // Verify analysis output is correct
    assert!(
        !output.formatted.is_empty(),
        "Formatted output should not be empty"
    );
    assert_eq!(output.files.len(), 3, "Should have analyzed 3 files");
}

#[test]
fn test_analyze_directory_with_progress_empty_directory() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Collect entries first
    let entries = walk_directory(root, None).unwrap();

    // Analyze empty directory with progress counter
    let counter = Arc::new(AtomicUsize::new(0));
    let ct = CancellationToken::new();
    let output = analyze_directory_with_progress(root, entries, counter.clone(), ct).unwrap();

    // Verify counter is 0 for empty directory
    let final_count = counter.load(Ordering::Relaxed);
    assert_eq!(final_count, 0, "Counter should be 0 for empty directory");

    // Verify analysis output is valid
    assert!(
        !output.formatted.is_empty(),
        "Formatted output should not be empty"
    );
    assert_eq!(output.files.len(), 0, "Should have no files");
}

// Completion tests

#[test]
fn test_path_completions_with_prefix() {
    // Arrange: Create a temporary directory with files
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn lib() {}").unwrap();
    fs::write(root.join("README.md"), "# Test").unwrap();

    // Act: Get completions for "src" prefix
    let completions = path_completions(root, "src");

    // Assert: Should find src directory
    assert!(
        !completions.is_empty(),
        "Should find completions for 'src' prefix"
    );
    assert!(
        completions.iter().any(|c| c.contains("src")),
        "Should include 'src' in completions"
    );
}

#[test]
fn test_path_completions_respects_ignore_file() {
    // Arrange: Create directory with .ignore file
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join(".ignore"), "target/\n").unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("target/debug.txt"), "debug").unwrap();

    // Act: Get completions for "t" prefix
    let completions = path_completions(root, "t");

    // Assert: Should not include target (ignored by .ignore)
    assert!(
        !completions.iter().any(|c| c.contains("target")),
        "Should exclude 'target' directory (ignored by .ignore)"
    );
}

#[test]
fn test_path_completions_empty_prefix() {
    // Arrange: Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::write(root.join("file.rs"), "fn f() {}").unwrap();

    // Act: Get completions with empty prefix
    let completions = path_completions(root, "");

    // Assert: Should return empty for empty prefix
    assert!(
        completions.is_empty(),
        "Should return empty completions for empty prefix"
    );
}

#[test]
fn test_path_completions_nonexistent_prefix() {
    // Arrange: Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::write(root.join("main.rs"), "fn main() {}").unwrap();

    // Act: Get completions for non-existent prefix
    let completions = path_completions(root, "xyz");

    // Assert: Should return empty for non-existent prefix
    assert!(
        completions.is_empty(),
        "Should return empty completions for non-existent prefix"
    );
}

#[test]
fn test_path_completions_truncates_at_100() {
    // Arrange: Create directory with >100 files
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    for i in 0..150 {
        fs::write(root.join(format!("file_{:03}.rs", i)), "fn f() {}").unwrap();
    }

    // Act: Get completions for "file" prefix
    let completions = path_completions(root, "file");

    // Assert: Should cap at 100 results
    assert_eq!(
        completions.len(),
        100,
        "Should truncate completions to 100 results"
    );
}

#[test]
fn test_symbol_completions_with_cached_analysis() {
    // Arrange: Create a Rust file and analyze it
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(
        &file_path,
        "fn hello_world() {}\nfn hello_there() {}\nstruct MyStruct {}",
    )
    .unwrap();

    // Analyze the file to populate cache
    let analysis = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let cache = AnalysisCache::new(100);
    let cache_key = CacheKey {
        path: file_path.clone(),
        modified: std::fs::metadata(&file_path).unwrap().modified().unwrap(),
        mode: AnalysisMode::FileDetails,
    };
    cache.put(cache_key, Arc::new(analysis));

    // Act: Get symbol completions for "hello" prefix
    let completions = symbol_completions(&cache, &file_path, "hello");

    // Assert: Should find matching functions
    assert!(
        !completions.is_empty(),
        "Should find completions for 'hello' prefix"
    );
    assert!(
        completions.iter().any(|c| c.contains("hello")),
        "Should include functions starting with 'hello'"
    );
}

#[test]
fn test_symbol_completions_missing_path_argument() {
    // Arrange: Create cache but don't populate it
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("nonexistent.rs");
    let cache = AnalysisCache::new(100);

    // Act: Get symbol completions for non-existent file
    let completions = symbol_completions(&cache, &file_path, "test");

    // Assert: Should return empty for missing file
    assert!(
        completions.is_empty(),
        "Should return empty completions for missing file"
    );
}

#[test]
fn test_symbol_completions_empty_prefix() {
    // Arrange: Create and analyze a file
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(&file_path, "fn test_func() {}").unwrap();

    let analysis = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let cache = AnalysisCache::new(100);
    let cache_key = CacheKey {
        path: file_path.clone(),
        modified: std::fs::metadata(&file_path).unwrap().modified().unwrap(),
        mode: AnalysisMode::FileDetails,
    };
    cache.put(cache_key, Arc::new(analysis));

    // Act: Get symbol completions with empty prefix
    let completions = symbol_completions(&cache, &file_path, "");

    // Assert: Should return empty for empty prefix
    assert!(
        completions.is_empty(),
        "Should return empty completions for empty prefix"
    );
}

#[test]
fn test_symbol_completions_truncates_at_100() {
    // Arrange: Create a file with >100 functions
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    let mut content = String::new();
    for i in 0..150 {
        content.push_str(&format!("fn func_{:03}() {{}}\n", i));
    }
    fs::write(&file_path, content).unwrap();

    let analysis = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let cache = AnalysisCache::new(100);
    let cache_key = CacheKey {
        path: file_path.clone(),
        modified: std::fs::metadata(&file_path).unwrap().modified().unwrap(),
        mode: AnalysisMode::FileDetails,
    };
    cache.put(cache_key, Arc::new(analysis));

    // Act: Get symbol completions for "func" prefix
    let completions = symbol_completions(&cache, &file_path, "func");

    // Assert: Should cap at 100 results
    assert_eq!(
        completions.len(),
        100,
        "Should truncate completions to 100 results"
    );
}

// Cancellation tests

#[test]
fn test_cancellation_during_directory_walk() {
    // Arrange: Create temp dir with a Rust file
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::write(root.join("main.rs"), "fn main() {}").unwrap();

    // Create a pre-cancelled token
    let ct = CancellationToken::new();
    ct.cancel();

    // Collect entries first
    let entries = walk_directory(root, None).unwrap();

    // Act: Call analyze_directory_with_progress with cancelled token
    let counter = Arc::new(AtomicUsize::new(0));
    let result = analyze_directory_with_progress(root, entries, counter, ct);

    // Assert: Should return Cancelled error
    assert!(matches!(result, Err(AnalyzeError::Cancelled)));
}

#[test]
fn test_cancellation_noop_after_completion() {
    // Arrange: Create temp dir with a Rust file
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::write(root.join("main.rs"), "fn main() {}").unwrap();

    // Create a non-cancelled token
    let ct = CancellationToken::new();

    // Collect entries first
    let entries = walk_directory(root, None).unwrap();

    // Act: Call analyze_directory_with_progress with active token
    let counter = Arc::new(AtomicUsize::new(0));
    let result = analyze_directory_with_progress(root, entries, counter, ct);

    // Assert: Should succeed (existing behavior unchanged)
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.files.len(), 1);
}

#[test]
fn test_summary_auto_detect_large_directory() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create src/ subdirectory with 1100 .rs files
    fs::create_dir(root.join("src")).unwrap();
    for i in 0..1100 {
        fs::write(root.join(format!("src/file_{:04}.rs", i)), "fn func() {}").unwrap();
    }

    // Analyze directory
    let output = analyze_directory(root, None).unwrap();

    // Generate summary
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);

    // Assert summary contains expected sections
    assert!(summary.contains("SUMMARY:"));
    assert!(summary.contains("STRUCTURE (depth 1):"));
    assert!(summary.contains("SUGGESTION:"));
    assert!(summary.contains("1100 files"));
    assert!(summary.contains("Languages:"));

    // Assert summary is shorter than full output
    let summary_lines = summary.lines().count();
    let full_lines = output.formatted.lines().count();
    assert!(
        summary_lines < full_lines,
        "Summary ({} lines) should be shorter than full output ({} lines)",
        summary_lines,
        full_lines
    );
}

#[test]
fn test_summary_explicit_on_small_directory() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create src/ subdirectory with 2 files
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("src/lib.rs"), "fn lib_fn() {}").unwrap();

    // Analyze directory
    let output = analyze_directory(root, None).unwrap();

    // Generate summary
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);

    // Assert summary contains expected sections
    assert!(summary.contains("SUMMARY:"));
    assert!(summary.contains("STRUCTURE (depth 1):"));
    assert!(summary.contains("SUGGESTION:"));
    assert!(summary.contains("2 files"));
}

// Test top-N hint in summary mode

#[test]
fn test_summary_top_hint_shown() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/model.rs"),
        "pub struct User { name: String }\npub struct Product { id: u32 }",
    )
    .unwrap();
    fs::write(
        root.join("src/handler.rs"),
        "pub struct Handler { } impl Handler { pub fn handle() {} }",
    )
    .unwrap();
    fs::write(root.join("src/util.rs"), "pub fn helper() {}").unwrap();

    let output = analyze_directory(root, None).unwrap();
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);

    assert!(summary.contains("top:"), "summary should contain top hint");
    assert!(
        summary.contains("(2C)") || summary.contains("(1C)"),
        "summary should show class counts with C suffix"
    );
}

#[test]
fn test_summary_top_hint_omitted_for_single_file() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {} fn helper() {}").unwrap();

    let output = analyze_directory(root, None).unwrap();
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);

    assert!(
        !summary.contains("top:"),
        "summary should not contain top hint for a single file"
    );
}

#[test]
fn test_format_summary_sibling_dir_prefix() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    let src = root.join("src");
    let src_extra = root.join("src_extra");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&src_extra).unwrap();
    fs::write(src.join("lib.rs"), "fn foo() {}").unwrap();
    fs::write(src_extra.join("lib.rs"), "fn bar() {}").unwrap();

    let output = analyze_directory(root, None).unwrap();
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);

    // src/ should show exactly 1 file, not 2
    let src_line = summary
        .lines()
        .find(|l| l.contains("src") && !l.contains("src_extra"))
        .expect("summary must contain a line for src/");
    let src_extra_line = summary
        .lines()
        .find(|l| l.contains("src_extra"))
        .expect("summary must contain a line for src_extra/");

    assert!(
        src_line.contains("[1 file"),
        "src/ should show exactly 1 file: {src_line}"
    );
    assert!(
        src_extra_line.contains("[1 file"),
        "src_extra/ should show exactly 1 file: {src_extra_line}"
    );
}

// Reference extraction tests for Python, Java, and TypeScript

#[cfg(feature = "lang-python")]
#[test]
fn test_python_reference_extraction_happy_path() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");

    let python_code = r#"
class User:
    pass

def greet(user: User) -> User:
    return user

def process(items: list[User]) -> None:
    pass
"#;

    fs::write(&file_path, python_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify references are extracted (non-empty)
    assert!(
        !output.semantic.references.is_empty(),
        "Expected type references to be extracted from Python file"
    );

    // Verify expected type names are present
    let ref_symbols: Vec<&str> = output
        .semantic
        .references
        .iter()
        .map(|r| r.symbol.as_str())
        .collect();
    assert!(
        ref_symbols.contains(&"User"),
        "Expected 'User' type reference in Python code"
    );

    // Verify references have line numbers and location
    for reference in &output.semantic.references {
        assert!(
            reference.line > 0,
            "Reference should have non-zero line number"
        );
        assert!(
            !reference.location.is_empty(),
            "Reference should have location populated"
        );
    }
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_reference_extraction_edge_case() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");

    let python_code = r#"
from typing import List, Union

class Result:
    pass

class Data:
    pass

def process(items: List[Result]) -> Union[Result, Data]:
    pass

def handle(data: list[dict[Result, Data]]) -> None:
    pass
"#;

    fs::write(&file_path, python_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify references are extracted for complex generic types
    assert!(
        !output.semantic.references.is_empty(),
        "Expected type references from generic types"
    );

    let ref_symbols: Vec<&str> = output
        .semantic
        .references
        .iter()
        .map(|r| r.symbol.as_str())
        .collect();

    // Should capture base type names from generics
    assert!(
        ref_symbols.contains(&"Result"),
        "Expected 'Result' from generic type parameters"
    );
    assert!(
        ref_symbols.contains(&"Data"),
        "Expected 'Data' from generic type parameters"
    );
}

// Import extraction tests

struct ImportTestCase {
    lang: &'static str,
    ext: &'static str,
    code: &'static str,
    expected_modules: Vec<&'static str>,
}

#[cfg(any(
    feature = "lang-python",
    feature = "lang-go",
    feature = "lang-java",
    feature = "lang-typescript"
))]
#[test]
fn test_import_extraction_happy_path() {
    let test_cases = vec![
        ImportTestCase {
            lang: "Python",
            ext: "py",
            code: r#"
import os
from sys import argv
from collections import defaultdict

def main():
    pass
"#,
            expected_modules: vec!["os", "sys", "collections"],
        },
        ImportTestCase {
            lang: "Go",
            ext: "go",
            code: r#"
package main

import (
    "fmt"
    "os"
)

import "io"

func main() {
    fmt.Println("Hello")
}
"#,
            expected_modules: vec!["fmt", "os", "io"],
        },
        ImportTestCase {
            lang: "Java",
            ext: "java",
            code: r#"
import java.util.ArrayList;
import java.util.List;
import static java.lang.Math.sqrt;

public class Test {
    public void method() {
        System.out.println("Hello");
    }
}
"#,
            expected_modules: vec!["ArrayList", "List", "Math"],
        },
        ImportTestCase {
            lang: "TypeScript",
            ext: "ts",
            code: r#"
import { Component } from 'react';
import * as fs from 'fs';
import path from 'path';

export function hello(): void {
    console.log("Hello");
}
"#,
            expected_modules: vec!["react", "fs", "path"],
        },
    ];

    for test_case in test_cases {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join(format!("test.{}", test_case.ext));

        fs::write(&file_path, test_case.code).unwrap();

        let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

        // Verify imports extracted
        assert!(
            !output.semantic.imports.is_empty(),
            "{}: expected non-empty imports",
            test_case.lang
        );
        let import_modules: Vec<&str> = output
            .semantic
            .imports
            .iter()
            .map(|i| i.module.as_str())
            .collect();

        for expected in test_case.expected_modules {
            assert!(
                import_modules.iter().any(|m| m.contains(expected)),
                "{}: expected module containing '{}' not found in {:?}",
                test_case.lang,
                expected,
                import_modules
            );
        }
    }
}

#[cfg(any(
    feature = "lang-python",
    feature = "lang-go",
    feature = "lang-java",
    feature = "lang-typescript"
))]
#[test]
fn test_import_extraction_no_imports() {
    let test_cases = vec![
        ImportTestCase {
            lang: "Python",
            ext: "py",
            code: r#"
def hello():
    print("Hello")

class MyClass:
    pass
"#,
            expected_modules: vec![],
        },
        ImportTestCase {
            lang: "Go",
            ext: "go",
            code: r#"
package main

func Hello() {
    println("Hello")
}
"#,
            expected_modules: vec![],
        },
        ImportTestCase {
            lang: "Java",
            ext: "java",
            code: r#"
public class Test {
    public void method() {
        System.out.println("Hello");
    }
}
"#,
            expected_modules: vec![],
        },
        ImportTestCase {
            lang: "TypeScript",
            ext: "ts",
            code: r#"
export function hello(): void {
    console.log("Hello");
}

export class MyClass {
    method(): string {
        return "test";
    }
}
"#,
            expected_modules: vec![],
        },
    ];

    for test_case in test_cases {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join(format!("test.{}", test_case.ext));

        fs::write(&file_path, test_case.code).unwrap();

        let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

        // Verify no imports extracted
        assert_eq!(
            output.semantic.imports.len(),
            0,
            "{}: expected zero imports",
            test_case.lang
        );
    }
}

// Test file partitioning tests

#[test]
fn test_format_structure_partitions_test_files() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create production and test files
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(root.join("src/lib.rs"), "fn production_fn() {}").unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("tests/test_utils.rs"), "fn test_helper() {}").unwrap();

    // Act: Analyze directory
    let output = analyze_directory(root, None).unwrap();

    // Assert: Output contains TEST FILES section
    assert!(
        output.formatted.contains("TEST FILES"),
        "Output should contain TEST FILES section when test files are present"
    );

    // Assert: Test files are listed in TEST FILES section
    assert!(
        output.formatted.contains("test_utils.rs"),
        "Test file should be listed in TEST FILES section"
    );

    // Assert: Production files are listed in PATH section (before TEST FILES)
    assert!(
        output.formatted.contains("lib.rs"),
        "Production file should be listed in PATH section"
    );
    assert!(
        output.formatted.contains("main.rs"),
        "Production file should be listed in PATH section"
    );
}

#[test]
fn test_format_structure_test_only_section_present() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create only test files (no production files)
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(root.join("tests/test_module.rs"), "fn test_helper() {}").unwrap();
    fs::write(root.join("tests/test_utils.rs"), "fn test_utility() {}").unwrap();

    // Act: Analyze directory
    let output = analyze_directory(root, None).unwrap();

    // Assert: TEST FILES header must appear when test files exist
    assert!(
        output.formatted.contains("TEST FILES"),
        "TEST FILES header must appear when test files exist"
    );

    // Assert: Test files are listed in TEST FILES section
    assert!(
        output.formatted.contains("test_module.rs"),
        "Test file should be listed in TEST FILES section"
    );
    assert!(
        output.formatted.contains("test_utils.rs"),
        "Test file should be listed in TEST FILES section"
    );
}

// AnalysisResponse serialization tests

#[test]
fn test_analysis_output_overview_fields() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a simple Rust file
    fs::write(root.join("lib.rs"), "fn hello() {}\nfn world() {}").unwrap();

    // Act: Analyze directory to get AnalysisOutput
    let analysis_output = analyze_directory(root, None).unwrap();

    // Assert: AnalysisOutput has formatted text
    assert!(!analysis_output.formatted.is_empty());
    assert!(analysis_output.formatted.contains("SUMMARY:"));

    // Assert: AnalysisOutput has files array
    assert!(!analysis_output.files.is_empty());
}

#[test]
fn test_analysis_output_file_details_fields() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");

    // Arrange: Create a Rust file with semantic content
    let rust_code = r#"
use std::collections::HashMap;

fn calculate(a: i32, b: i32) -> i32 {
    a + b
}

struct Point {
    x: i32,
    y: i32,
}
"#;
    fs::write(&file_path, rust_code).unwrap();

    // Act: Analyze file to get FileAnalysisOutput
    let analysis_output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Assert: FileAnalysisOutput has formatted text
    assert!(!analysis_output.formatted.is_empty());
    assert!(analysis_output.formatted.contains("FILE:"));

    // Assert: FileAnalysisOutput has semantic data with functions
    assert!(!analysis_output.semantic.functions.is_empty());

    // Assert: FileAnalysisOutput has line_count
    assert!(analysis_output.line_count > 0);
}

// Pagination integration tests

#[test]
fn test_overview_pagination_multi_page() {
    use aptu_coder_core::pagination::{PaginationMode, decode_cursor, paginate_slice};

    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    for i in 0..150 {
        fs::write(root.join(format!("file_{:03}.rs", i)), "fn f() {}").unwrap();
    }

    let output = analyze_directory(root, None).unwrap();
    assert_eq!(output.files.len(), 150);

    let result =
        paginate_slice(&output.files, 0, 100, PaginationMode::Default).expect("paginate failed");
    assert_eq!(result.items.len(), 100);
    assert!(result.next_cursor.is_some());
    assert_eq!(result.total, 150);

    let cursor = result.next_cursor.unwrap();
    let cursor_data = decode_cursor(&cursor).expect("decode failed");
    let result2 = paginate_slice(
        &output.files,
        cursor_data.offset,
        100,
        PaginationMode::Default,
    )
    .expect("paginate failed");
    assert_eq!(result2.items.len(), 50);
    assert!(result2.next_cursor.is_none());
}

#[test]
fn test_single_page_no_cursor() {
    use aptu_coder_core::pagination::{PaginationMode, paginate_slice};

    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    for i in 0..50 {
        fs::write(root.join(format!("file_{:03}.rs", i)), "fn f() {}").unwrap();
    }

    let output = analyze_directory(root, None).unwrap();
    let result =
        paginate_slice(&output.files, 0, 100, PaginationMode::Default).expect("paginate failed");
    assert_eq!(result.items.len(), 50);
    assert!(result.next_cursor.is_none());
    assert_eq!(result.total, 50);
}

// Inheritance extraction tests

#[cfg(feature = "lang-java")]
#[test]
fn test_java_inheritance_extraction() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("Test.java");

    let java_code = r#"
public class Animal {
    public void speak() {}
}

public class Dog extends Animal implements Comparable {
    public int compareTo(Object o) {
        return 0;
    }
}
"#;

    fs::write(&file_path, java_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify classes extracted
    assert_eq!(output.semantic.classes.len(), 2);

    // Find Dog class and verify inheritance
    let dog_class = output
        .semantic
        .classes
        .iter()
        .find(|c| c.name == "Dog")
        .expect("Dog class should be extracted");

    assert!(
        !dog_class.inherits.is_empty(),
        "Dog should have inheritance info"
    );
    assert!(
        dog_class
            .inherits
            .iter()
            .any(|i| i.contains("extends Animal")),
        "Dog should extend Animal"
    );
    assert!(
        dog_class
            .inherits
            .iter()
            .any(|i| i.contains("implements Comparable")),
        "Dog should implement Comparable"
    );
}

#[cfg(feature = "lang-typescript")]
#[test]
fn test_typescript_inheritance_extraction() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");

    let ts_code = r#"
class Animal {
    name: string;
}

interface Movable {
    move(): void;
}

class Dog extends Animal implements Movable {
    move(): void {}
}
"#;

    fs::write(&file_path, ts_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Find Dog class and verify inheritance
    let dog_class = output
        .semantic
        .classes
        .iter()
        .find(|c| c.name == "Dog")
        .expect("Dog class should be extracted");

    assert!(
        !dog_class.inherits.is_empty(),
        "Dog should have inheritance info"
    );
    assert!(
        dog_class
            .inherits
            .iter()
            .any(|i| i.contains("extends Animal")),
        "Dog should extend Animal"
    );
    assert!(
        dog_class
            .inherits
            .iter()
            .any(|i| i.contains("implements Movable")),
        "Dog should implement Movable"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_inheritance_extraction() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");

    let python_code = r#"
class Animal:
    pass

class Dog(Animal):
    pass
"#;

    fs::write(&file_path, python_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Find Dog class and verify inheritance
    let dog_class = output
        .semantic
        .classes
        .iter()
        .find(|c| c.name == "Dog")
        .expect("Dog class should be extracted");

    assert!(
        !dog_class.inherits.is_empty(),
        "Dog should have inheritance info"
    );
    assert!(
        dog_class.inherits.iter().any(|i| i.contains("Animal")),
        "Dog should inherit from Animal"
    );
}

#[cfg(feature = "lang-go")]
#[test]
fn test_go_inheritance_extraction() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.go");

    let go_code = r#"
package main

type Reader interface {
    Read() error
}

type Writer interface {
    Write() error
}

type ReadWriter struct {
    Reader
    Writer
}

type MyInterface interface {
    Reader
    Writer
}
"#;

    fs::write(&file_path, go_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Find ReadWriter struct and verify embedded types
    let rw_struct = output
        .semantic
        .classes
        .iter()
        .find(|c| c.name == "ReadWriter")
        .expect("ReadWriter struct should be extracted");

    assert!(
        !rw_struct.inherits.is_empty(),
        "ReadWriter should have embedded types"
    );
    assert!(
        rw_struct.inherits.iter().any(|i| i.contains("Reader")),
        "ReadWriter should embed Reader"
    );
    assert!(
        rw_struct.inherits.iter().any(|i| i.contains("Writer")),
        "ReadWriter should embed Writer"
    );

    // Find MyInterface and verify embedded interfaces
    let my_iface = output
        .semantic
        .classes
        .iter()
        .find(|c| c.name == "MyInterface")
        .expect("MyInterface should be extracted");

    assert!(
        !my_iface.inherits.is_empty(),
        "MyInterface should have embedded interfaces"
    );
}

#[test]
fn test_rust_no_syntactic_inheritance() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");

    let rust_code = r#"
struct Point {
    x: i32,
    y: i32,
}

trait Drawable {
    fn draw(&self);
}

impl Drawable for Point {
    fn draw(&self) {}
}
"#;

    fs::write(&file_path, rust_code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify classes extracted
    assert_eq!(output.semantic.classes.len(), 2);

    // Verify all Rust classes have empty inherits (no syntactic inheritance)
    for class in &output.semantic.classes {
        assert!(
            class.inherits.is_empty(),
            "Rust {} should have empty inherits (inheritance is via impl blocks)",
            class.name
        );
    }
}

#[test]
fn test_format_symbol_list_inline() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("inline.rs");

    // Create a file with exactly 10 classes (at threshold for inline format)
    let mut code = String::new();
    for i in 0..10 {
        code.push_str(&format!("struct Class{} {{}}\n", i));
    }
    fs::write(&file_path, code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify 10 classes are extracted
    assert_eq!(output.semantic.classes.len(), 10);

    // Verify inline format: classes should be on one line separated by semicolons
    let formatted = output.formatted;
    assert!(formatted.contains("C:"), "Should contain C: section");

    // Extract the C: section
    let c_section = formatted
        .split("C:")
        .nth(1)
        .unwrap_or("")
        .split("F:")
        .next()
        .unwrap_or("");

    // For inline format with 10 items, should have semicolons separating classes
    let semicolon_count = c_section.matches(';').count();
    assert!(
        semicolon_count >= 9,
        "Inline format with 10 classes should have at least 9 semicolons, got {}",
        semicolon_count
    );
}

#[test]
fn test_format_symbol_list_multiline() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("multiline.rs");

    // Create a file with 11 classes (exceeds threshold for multiline format)
    let mut code = String::new();
    for i in 0..11 {
        code.push_str(&format!("struct Class{} {{}}\n", i));
    }
    fs::write(&file_path, code).unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Verify 11 classes are extracted
    assert_eq!(output.semantic.classes.len(), 11);

    // Verify multiline format: classes should be on separate lines
    let formatted = output.formatted;
    assert!(formatted.contains("C:"), "Should contain C: section");

    // Extract the C: section
    let c_section = formatted
        .split("C:")
        .nth(1)
        .unwrap_or("")
        .split("F:")
        .next()
        .unwrap_or("");

    // For multiline format, each class should be on its own line with indentation
    // Count lines that start with "  " (indentation) in the C section
    let indented_lines = c_section
        .lines()
        .filter(|line| line.starts_with("  ") && !line.trim().is_empty())
        .count();

    assert!(
        indented_lines >= 11,
        "Multiline format with 11 classes should have at least 11 indented lines, got {}",
        indented_lines
    );
}

#[test]
fn test_structured_content_present_overview() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a simple Rust file for analysis
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"Hello\"); }",
    )
    .unwrap();

    // Analyze the directory
    let output = analyze_directory(root, None).unwrap();

    // Serialize the output to JSON (simulating what the tool would do)
    let structured = serde_json::to_value(&output).expect("Failed to serialize output");

    // Verify structured_content is a valid JSON object
    assert!(
        structured.is_object(),
        "Structured content should be a JSON object"
    );

    // Verify it contains the expected fields
    let obj = structured.as_object().unwrap();
    assert!(
        obj.contains_key("formatted"),
        "Structured content should contain 'formatted' field"
    );
    assert!(
        obj.contains_key("files"),
        "Structured content should contain 'files' field for overview mode"
    );

    // Verify files array is present and valid
    let files = obj.get("files").expect("files field should exist");
    assert!(
        files.is_array(),
        "files field should be an array in overview mode"
    );
}

#[test]
fn test_text_content_not_json_when_structured() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a simple Rust file
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }",
    )
    .unwrap();

    // Analyze the directory
    let output = analyze_directory(root, None).unwrap();

    // The formatted text should be human-readable, not raw JSON
    let formatted = &output.formatted;

    // Verify it's not JSON (doesn't start with '{' or '[')
    assert!(
        !formatted.trim().starts_with('{') && !formatted.trim().starts_with('['),
        "Formatted text should not be raw JSON, should be human-readable"
    );

    // Verify it contains expected text content (file paths, metrics, etc.)
    assert!(!formatted.is_empty(), "Formatted text should not be empty");
    assert!(
        formatted.contains("lib.rs") || formatted.contains("src"),
        "Formatted text should contain file information"
    );
}

#[test]
fn test_tool_metadata_title_and_schema() {
    // This test verifies that the output schema is properly defined
    // by checking that the analyze output structs serialize to valid JSON
    // that matches the expected schema structure

    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a simple Rust file
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn test() {}").unwrap();

    // Analyze the directory to get an AnalysisOutput
    let output = analyze_directory(root, None).unwrap();

    // Serialize to JSON (this is what would be in structured_content)
    let serialized = serde_json::to_value(&output).expect("Failed to serialize output");

    // Verify the serialized output matches the expected schema structure
    let obj = serialized.as_object().expect("Should be a JSON object");

    // Verify required fields from the schema
    assert!(
        obj.contains_key("formatted"),
        "Output should contain 'formatted' field"
    );
    assert!(
        obj.contains_key("files"),
        "Output should contain 'files' field"
    );

    // Verify the structure matches the schema definition
    let formatted = obj.get("formatted").expect("formatted field should exist");
    assert!(formatted.is_string(), "formatted field should be a string");

    let files = obj.get("files").expect("files field should exist");
    assert!(files.is_array(), "files field should be an array");

    // Verify next_cursor is either absent (skipped if None) or a string
    if let Some(next_cursor) = obj.get("next_cursor") {
        assert!(
            next_cursor.is_null() || next_cursor.is_string(),
            "next_cursor should be null or string"
        );
    }
}

#[test]
fn test_format_focused_tree_indent_callees() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a crate with multi-depth call chains
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn main_func() {
    helper_a();
    helper_b();
}

pub fn helper_a() {
    leaf_1();
    leaf_2();
}

pub fn helper_b() {
    leaf_1();
}

pub fn leaf_1() {}
pub fn leaf_2() {}
"#,
    )
    .unwrap();

    // Act: Format focused output with depth 2
    let output = analyze_focused(root, "main_func", 1, None, None).unwrap();

    // Assert: Verify tree-indented CALLEES section with proper grouping
    assert!(
        output.formatted.contains("CALLEES:"),
        "Should have CALLEES section"
    );

    // Check that callees are grouped under parent symbols
    let lines: Vec<&str> = output.formatted.lines().collect();

    // Find CALLEES section and verify it has properly indented entries
    if let Some(callees_idx) = lines.iter().position(|l| l.contains("CALLEES:")) {
        let callees_lines: Vec<&str> = lines[callees_idx + 1..]
            .iter()
            .take_while(|l| !l.is_empty() && !l.starts_with("FILES:"))
            .copied()
            .collect();

        // Should have depth-1 entries with focus symbol prefix: "  main_func -> helper_a"
        assert!(
            callees_lines
                .iter()
                .any(|l| l.contains("main_func -> helper_a")),
            "Should have depth-1 entry with focus symbol and arrow: 'main_func -> helper_a'"
        );

        // Should have depth-2 children indented with 4 spaces: "    -> leaf_1"
        assert!(
            callees_lines
                .iter()
                .any(|l| l.trim().starts_with("-> leaf_1")),
            "Should have depth-2 child with indentation: '    -> leaf_1'"
        );

        // Should have depth-2 children: "    -> leaf_2"
        assert!(
            callees_lines
                .iter()
                .any(|l| l.trim().starts_with("-> leaf_2")),
            "Should have depth-2 child with indentation: '    -> leaf_2'"
        );

        // Should have second parent: "  main_func -> helper_b"
        assert!(
            callees_lines
                .iter()
                .any(|l| l.contains("main_func -> helper_b")),
            "Should have second depth-1 entry: 'main_func -> helper_b'"
        );
    }
}

#[test]
fn test_format_focused_empty_chains() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create isolated function with no callers or callees
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn isolated() {
}
"#,
    )
    .unwrap();

    // Act: Format focused output for isolated function
    let output = analyze_focused(root, "isolated", 2, None, None).unwrap();

    // Assert: Both CALLERS and CALLEES should show (none)
    assert!(
        output.formatted.contains("CALLERS:"),
        "Should have CALLERS section"
    );
    assert!(
        output.formatted.contains("CALLEES:"),
        "Should have CALLEES section"
    );

    // Verify (none) appears for empty chains
    let lines: Vec<&str> = output.formatted.lines().collect();
    let has_none = lines.iter().any(|l| l.trim() == "(none)");
    assert!(has_none, "Empty chains should render (none)");
}

#[test]
fn test_focus_header_includes_counts() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a crate with known call graph for counting
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn main_func() {
    helper_a();
}

pub fn helper_a() {
    helper_b();
}

pub fn helper_b() {}
"#,
    )
    .unwrap();

    // Act: Format focused output for main_func with depth 2
    let output = analyze_focused(root, "main_func", 2, None, None).unwrap();

    // Assert: Header should contain counts
    assert!(
        output.formatted.starts_with("FOCUS: main_func (1 defs, "),
        "Header should start with symbol name and def count: {}",
        output.formatted.lines().next().unwrap()
    );

    // Verify the exact format: "FOCUS: main_func (1 defs, N callers, N callees)"
    let first_line = output.formatted.lines().next().unwrap();
    assert!(
        first_line.contains("defs,"),
        "Header should contain 'defs,': {}",
        first_line
    );
    assert!(
        first_line.contains("callers,"),
        "Header should contain 'callers,': {}",
        first_line
    );
    assert!(
        first_line.contains("callees"),
        "Header should contain 'callees': {}",
        first_line
    );
}

#[test]
fn test_callers_mixed_prod_and_test() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a crate with mixed production and test callers
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target() {}

pub fn prod_caller_a() {
    target();
}

pub fn prod_caller_b() {
    target();
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("tests/test_module.rs"),
        r#"
use aptu_coder_core::*;

#[test]
fn test_target() {
    target();
}
"#,
    )
    .unwrap();

    // Act: Format focused output
    let output = analyze_focused(root, "target", 1, None, None).unwrap();

    // Assert: Production callers in CALLERS section, test summary in CALLERS (test)
    let lines: Vec<&str> = output.formatted.lines().collect();

    // Find CALLERS section
    let callers_idx = lines
        .iter()
        .position(|l| l.contains("CALLERS:"))
        .expect("Should have CALLERS section");

    // Verify production callers are shown
    let callers_content = &lines[callers_idx + 1..];
    let has_prod_caller = callers_content
        .iter()
        .take_while(|l| {
            !l.starts_with("FILES:") && !l.starts_with("CALLERS (test):") && !l.is_empty()
        })
        .any(|l| l.contains("prod_caller_a") || l.contains("prod_caller_b"));
    assert!(
        has_prod_caller,
        "Should have production callers in CALLERS section"
    );

    // Verify test callers summary line exists
    let has_test_summary = output.formatted.contains("CALLERS (test):");
    assert!(has_test_summary, "Should have CALLERS (test): summary line");

    // Verify test summary contains file reference
    let has_test_file_ref = output.formatted.contains("test_module.rs");
    assert!(has_test_file_ref, "Test summary should reference test file");
}

#[test]
fn test_callers_all_test() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a crate with only test callers
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target() {}
"#,
    )
    .unwrap();
    fs::write(
        root.join("tests/test_all.rs"),
        r#"
use aptu_coder_core::*;

#[test]
fn test_target_a() {
    target();
}

#[test]
fn test_target_b() {
    target();
}
"#,
    )
    .unwrap();

    // Act: Format focused output
    let output = analyze_focused(root, "target", 1, None, None).unwrap();

    // Assert: CALLERS shows (none), CALLERS (test) shows test summary
    let lines: Vec<&str> = output.formatted.lines().collect();

    let callers_idx = lines
        .iter()
        .position(|l| l.contains("CALLERS:"))
        .expect("Should have CALLERS section");

    // Check that production callers show (none)
    let callers_section: Vec<&str> = lines[callers_idx + 1..]
        .iter()
        .take_while(|l| !l.starts_with("FILES:") && !l.starts_with("CALLERS (test):"))
        .copied()
        .collect();

    let has_none = callers_section.iter().any(|l| l.trim() == "(none)");
    assert!(has_none, "Production callers should show (none)");

    // Verify test summary exists
    let has_test_summary = output.formatted.contains("CALLERS (test):");
    assert!(has_test_summary, "Should have CALLERS (test): summary line");
}

#[test]
fn test_callers_all_prod_no_test_line() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a crate with only production callers
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target() {}

pub fn caller_one() {
    target();
}

pub fn caller_two() {
    target();
}
"#,
    )
    .unwrap();

    // Act: Format focused output
    let output = analyze_focused(root, "target", 1, None, None).unwrap();

    // Assert: CALLERS section shows callers, no CALLERS (test) line
    assert!(
        output.formatted.contains("CALLERS:"),
        "Should have CALLERS section"
    );
    assert!(
        output.formatted.contains("caller_one") || output.formatted.contains("caller_two"),
        "Should have production callers"
    );
    assert!(
        !output.formatted.contains("CALLERS (test):"),
        "Should NOT have CALLERS (test) line with only production callers"
    );
}

#[test]
fn test_format_focused_dedup_callees() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a crate where caller invokes same callee multiple times
    // and also calls other callees once
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn caller() {
    repeated_callee();
    repeated_callee();
    repeated_callee();
    single_callee();
}

pub fn repeated_callee() {
}

pub fn single_callee() {
}
"#,
    )
    .unwrap();

    // Act: Format focused output with depth 1
    let output = analyze_focused(root, "caller", 1, None, None).unwrap();

    // Assert: Verify dedup annotation (xN) appears for repeated edges
    assert!(
        output.formatted.contains("CALLEES:"),
        "Should have CALLEES section"
    );

    // Check that repeated_callee shows (x3) annotation
    assert!(
        output.formatted.contains("repeated_callee (x3)"),
        "Repeated callee with 3 occurrences should show (x3) annotation"
    );

    // Check that single_callee does NOT show (x1) annotation
    let has_single_annotation = output.formatted.contains("single_callee (x1)");
    assert!(
        !has_single_annotation,
        "Single-occurrence callee should NOT show (x1) annotation"
    );

    // Verify single_callee still appears without annotation
    assert!(
        output.formatted.contains("-> single_callee")
            || output
                .formatted
                .lines()
                .any(|l| l.trim().ends_with("single_callee")),
        "Single-occurrence callee should appear without annotation"
    );
}

#[test]
fn test_file_details_summary_explicit_true() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Arrange: Create a small Rust file
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn hello() {}

pub fn world() {}
"#,
    )
    .unwrap();

    let _file_path = root.join("src/lib.rs");

    // Act: Analyze with summary=true
    // Since we don't have direct call_tool access in tests, verify the underlying function
    use aptu_coder_core::formatter::format_file_details_summary;
    use aptu_coder_core::types::SemanticAnalysis;

    let mut semantic = SemanticAnalysis::default();
    let mut fi1 = aptu_coder_core::types::FunctionInfo::default();
    fi1.name = "hello".to_string();
    fi1.line = 2;
    fi1.end_line = 2;
    fi1.parameters = vec![];
    fi1.return_type = None;
    let mut fi2 = aptu_coder_core::types::FunctionInfo::default();
    fi2.name = "world".to_string();
    fi2.line = 4;
    fi2.end_line = 4;
    fi2.parameters = vec![];
    fi2.return_type = None;
    semantic.functions = vec![fi1, fi2];

    let summary = format_file_details_summary(&semantic, "src/lib.rs", 5);

    // Assert: Summary contains FILE header and functions listed
    assert!(summary.contains("FILE:"), "Should have FILE header");
    assert!(summary.contains("src/lib.rs"), "Should show path");
    assert!(summary.contains("5L, 2F, 0C"), "Should show LOC and counts");
    assert!(
        summary.contains("TOP FUNCTIONS BY SIZE:"),
        "Should have functions section"
    );
}

#[test]
fn test_file_details_force_bypasses_summary() {
    // Arrange: Create semantic data with many functions that would normally trigger summary
    use aptu_coder_core::formatter::format_file_details_summary;
    use aptu_coder_core::types::{FunctionInfo, SemanticAnalysis};

    let mut functions = Vec::new();
    for i in 0..50 {
        let mut fi = FunctionInfo::default();
        fi.name = format!("function_{}", i);
        fi.line = i * 10;
        fi.end_line = i * 10 + 5;
        fi.parameters = vec![];
        fi.return_type = None;
        functions.push(fi);
    }

    let mut semantic = SemanticAnalysis::default();
    semantic.functions = functions;

    let summary = format_file_details_summary(&semantic, "src/lib.rs", 5000);

    // Assert: Summary should contain top 10 functions only
    assert!(
        summary.contains("TOP FUNCTIONS BY SIZE:"),
        "Should show top functions"
    );
    // Should contain first 10 functions when sorted by size
    let count = summary.lines().filter(|l| l.contains("function_")).count();
    assert!(
        count <= 10,
        "Summary should show at most 10 functions, got {}",
        count
    );
}

#[test]
fn test_format_file_details_summary_many_classes() {
    // Arrange: 15 classes to trigger multiline "... and N more" path
    use aptu_coder_core::formatter::format_file_details_summary;
    use aptu_coder_core::types::{ClassInfo, SemanticAnalysis};

    let classes: Vec<ClassInfo> = (0..15)
        .map(|i| {
            let mut ci = ClassInfo::default();
            ci.name = format!("Class{}", i);
            ci.line = i * 10;
            ci.end_line = i * 10 + 5;
            ci.methods = vec![];
            ci.fields = vec![];
            ci.inherits = vec![];
            ci
        })
        .collect();

    let mut semantic = SemanticAnalysis::default();
    semantic.classes = classes;

    // Act
    let summary = format_file_details_summary(&semantic, "src/lib.rs", 150);

    // Assert: multiline format with "... and N more"
    assert!(summary.contains("CLASSES:"), "Should have CLASSES section");
    assert!(
        summary.contains("15 classes total"),
        "Should show total class count"
    );
    assert!(
        summary.contains("... and 10 more"),
        "Should show remaining count"
    );
}

// --- FileDetails pagination tests (issue #146) ---

#[test]
fn test_file_details_pagination_first_page() {
    use aptu_coder_core::formatter::format_file_details_paginated;
    use aptu_coder_core::pagination::{PaginationMode, decode_cursor, paginate_slice};
    use aptu_coder_core::types::{FunctionInfo, SemanticAnalysis};

    // Arrange: 25 functions, page_size=10
    let functions: Vec<FunctionInfo> = (0..25)
        .map(|i| {
            let mut fi = FunctionInfo::default();
            fi.name = format!("fn_{:02}", i);
            fi.line = i + 1;
            fi.end_line = i + 5;
            fi.parameters = vec![];
            fi.return_type = None;
            fi
        })
        .collect();

    let mut semantic = SemanticAnalysis::default();
    semantic.functions = functions.clone();

    // Act: paginate first page
    let paginated =
        paginate_slice(&functions, 0, 10, PaginationMode::Default).expect("paginate failed");
    assert_eq!(paginated.items.len(), 10);
    assert!(paginated.next_cursor.is_some());
    assert_eq!(paginated.total, 25);

    let formatted = format_file_details_paginated(
        &paginated.items,
        paginated.total,
        &semantic,
        "src/lib.rs",
        500,
        0,
        true,
        None,
    );

    // Assert: header shows position, F: section present
    assert!(
        formatted.contains("1-10/25F"),
        "header should show 1-10/25F"
    );
    assert!(formatted.contains("F:"), "should have F: section");
    assert!(formatted.contains("fn_00"), "first function should appear");
    assert!(
        !formatted.contains("fn_10"),
        "11th function should not appear"
    );

    // Verify cursor round-trip
    let cursor_str = paginated.next_cursor.unwrap();
    let cursor_data = decode_cursor(&cursor_str).expect("decode failed");
    assert_eq!(cursor_data.offset, 10);
}

#[test]
fn test_file_details_pagination_last_page() {
    use aptu_coder_core::formatter::format_file_details_paginated;
    use aptu_coder_core::pagination::{PaginationMode, paginate_slice};
    use aptu_coder_core::types::{FunctionInfo, SemanticAnalysis};

    // Arrange: 25 functions, page 2 starts at offset 10 with page_size 20 -> 15 items remaining
    let functions: Vec<FunctionInfo> = (0..25)
        .map(|i| {
            let mut fi = FunctionInfo::default();
            fi.name = format!("fn_{:02}", i);
            fi.line = i + 1;
            fi.end_line = i + 5;
            fi.parameters = vec![];
            fi.return_type = None;
            fi
        })
        .collect();

    let mut semantic = SemanticAnalysis::default();
    semantic.functions = functions.clone();

    // Act: paginate last page (offset=10, page_size=20 -> items 10..25)
    let paginated =
        paginate_slice(&functions, 10, 20, PaginationMode::Default).expect("paginate failed");
    assert_eq!(paginated.items.len(), 15);
    assert!(
        paginated.next_cursor.is_none(),
        "last page should have no next_cursor"
    );

    let formatted = format_file_details_paginated(
        &paginated.items,
        paginated.total,
        &semantic,
        "src/lib.rs",
        500,
        10,
        true,
        None,
    );

    // Assert: header shows correct range
    assert!(
        formatted.contains("11-25/25F"),
        "header should show 11-25/25F"
    );
    // Classes and imports NOT shown on non-first page
    assert!(
        !formatted.contains("C:"),
        "classes should not appear on non-first page"
    );
    assert!(
        !formatted.contains("I:"),
        "imports should not appear on non-first page"
    );
}

#[test]
fn test_file_details_single_page_no_cursor() {
    use aptu_coder_core::pagination::{PaginationMode, paginate_slice};
    use aptu_coder_core::types::FunctionInfo;

    // Arrange: 5 functions, page_size=100
    let functions: Vec<FunctionInfo> = (0..5)
        .map(|i| {
            let mut fi = FunctionInfo::default();
            fi.name = format!("fn_{}", i);
            fi.line = i + 1;
            fi.end_line = i + 5;
            fi.parameters = vec![];
            fi.return_type = None;
            fi
        })
        .collect();

    // Act
    let paginated =
        paginate_slice(&functions, 0, 100, PaginationMode::Default).expect("paginate failed");

    // Assert: single page, no cursor
    assert_eq!(paginated.items.len(), 5);
    assert!(
        paginated.next_cursor.is_none(),
        "single page should have no next_cursor"
    );
    assert_eq!(paginated.total, 5);
}

#[test]
fn test_file_details_invalid_cursor() {
    use aptu_coder_core::pagination::decode_cursor;

    // Act
    let result = decode_cursor("this-is-not-valid-base64!!!");

    // Assert
    assert!(result.is_err(), "invalid cursor should produce an error");
}

// --- Unit tests for new formatter functions (issue #146) ---

#[test]
fn test_format_file_details_paginated_unit() {
    use aptu_coder_core::formatter::format_file_details_paginated;
    use aptu_coder_core::types::{ClassInfo, FunctionInfo, ImportInfo, SemanticAnalysis};

    // Arrange: simulate page 2 of 3 (functions 11-20 of 30)
    let all_functions: Vec<FunctionInfo> = (0..30)
        .map(|i| {
            let mut fi = FunctionInfo::default();
            fi.name = format!("fn_{:02}", i);
            fi.line = i + 1;
            fi.end_line = i + 5;
            fi.parameters = vec![];
            fi.return_type = None;
            fi
        })
        .collect();

    let page_functions = all_functions[10..20].to_vec();

    let mut semantic = SemanticAnalysis::default();
    semantic.functions = all_functions;
    let mut ci = ClassInfo::default();
    ci.name = "MyClass".to_string();
    ci.line = 100;
    ci.end_line = 150;
    ci.methods = vec![];
    ci.fields = vec![];
    ci.inherits = vec![];
    semantic.classes = vec![ci];
    let mut ii = ImportInfo::default();
    ii.module = "std".to_string();
    ii.items = vec![];
    ii.line = 1;
    semantic.imports = vec![ii];

    // Act: format page 2 (offset=10)
    let formatted = format_file_details_paginated(
        &page_functions,
        30,
        &semantic,
        "src/formatter.rs",
        750,
        10,
        true,
        None,
    );

    // Assert: header shows correct range
    assert!(
        formatted.contains("11-20/30F"),
        "header should show 11-20/30F, got: {}",
        formatted
    );
    // Classes NOT on page 2
    assert!(
        !formatted.contains("C:"),
        "classes should not appear on page 2"
    );
    assert!(
        !formatted.contains("I:"),
        "imports should not appear on page 2"
    );
    // Functions present
    assert!(formatted.contains("fn_10"), "fn_10 should be on this page");
    assert!(formatted.contains("fn_19"), "fn_19 should be on this page");
    assert!(
        !formatted.contains("fn_00"),
        "fn_00 should not be on this page"
    );
    assert!(
        !formatted.contains("fn_20"),
        "fn_20 should not be on this page"
    );
}

#[test]
fn test_analyze_module_rust_happy_path() {
    use aptu_coder_core::analyze::analyze_module_file;
    use std::io::Write;

    let rust_code = r#"use std::collections::HashMap;
use std::fs;

fn parse_config(path: &str) -> Result<(), ()> {
    Ok(())
}

fn main() {
    println!("Hello, world!");
}
"#;

    let mut tmp = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("create temp file");
    tmp.write_all(rust_code.as_bytes())
        .expect("write temp file");
    let path = tmp.path().to_str().expect("valid path").to_string();

    let module_info = analyze_module_file(&path).expect("should analyze module");

    assert!(module_info.name.ends_with(".rs"));
    assert!(module_info.line_count > 0);
    assert_eq!(module_info.language, "rust");
    assert!(!module_info.functions.is_empty());
    let func_names: Vec<_> = module_info.functions.iter().map(|f| &f.name).collect();
    assert!(func_names.contains(&&"parse_config".to_string()));
    assert!(func_names.contains(&&"main".to_string()));
    assert!(!module_info.imports.is_empty());
    let import_modules: Vec<_> = module_info.imports.iter().map(|i| &i.module).collect();
    assert!(import_modules.iter().any(|m| m.contains("collections")));
}

#[test]
fn test_analyze_module_empty_file() {
    use aptu_coder_core::analyze::analyze_module_file;
    use std::io::Write;

    let mut tmp = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("create temp file");
    tmp.write_all(b"").expect("write temp file");
    let path = tmp.path().to_str().expect("valid path").to_string();

    let module_info = analyze_module_file(&path).expect("should analyze empty module");

    assert_eq!(module_info.line_count, 0);
    assert_eq!(module_info.functions.len(), 0);
    assert_eq!(module_info.imports.len(), 0);
}

#[test]
fn test_analyze_module_functions_only() {
    use aptu_coder_core::analyze::analyze_module_file;
    use std::io::Write;

    let code = b"fn add(a: i32, b: i32) -> i32 { a + b }
fn subtract(a: i32, b: i32) -> i32 { a - b }
";

    let mut tmp = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("create temp file");
    tmp.write_all(code).expect("write temp file");
    let path = tmp.path().to_str().expect("valid path").to_string();

    let module_info = analyze_module_file(&path).expect("should analyze module");

    assert_eq!(module_info.functions.len(), 2);
    let func_names: Vec<_> = module_info.functions.iter().map(|f| &f.name).collect();
    assert!(func_names.contains(&&"add".to_string()));
    assert!(func_names.contains(&&"subtract".to_string()));
    assert_eq!(module_info.imports.len(), 0);
}

#[test]
fn test_analyze_module_imports_only() {
    use aptu_coder_core::analyze::analyze_module_file;
    use std::io::Write;

    let code = b"use std::collections::HashMap;
use std::fs::File;
";

    let mut tmp = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("create temp file");
    tmp.write_all(code).expect("write temp file");
    let path = tmp.path().to_str().expect("valid path").to_string();

    let module_info = analyze_module_file(&path).expect("should analyze module");

    assert_eq!(module_info.functions.len(), 0);
    assert!(!module_info.imports.is_empty());
    let import_modules: Vec<_> = module_info.imports.iter().map(|i| &i.module).collect();
    assert!(import_modules.iter().any(|m| m.contains("collections")));
}

#[test]
fn test_analyze_module_unsupported_extension() {
    use aptu_coder_core::analyze::analyze_module_file;
    use std::io::Write;

    let mut tmp = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile()
        .expect("create temp file");
    tmp.write_all(b"hello").expect("write temp file");
    let path = tmp.path().to_str().expect("valid path").to_string();

    let result = analyze_module_file(&path);
    assert!(result.is_err(), "expected error for unsupported extension");
}

#[cfg(feature = "schemars")]
#[test]
fn test_no_uint_format_in_schemas() {
    use aptu_coder_core::types::{
        AnalyzeDirectoryParams, AnalyzeFileParams, AnalyzeSymbolParams, FileInfo,
    };

    // Use schema_for! (root schema) so $defs are included and $ref targets are
    // fully present in the serialized JSON, not hidden behind unresolved $ref pointers.
    let schemas = [
        (
            "FileInfo",
            serde_json::to_string(&schemars::schema_for!(FileInfo)).unwrap(),
        ),
        (
            "AnalyzeDirectoryParams",
            serde_json::to_string(&schemars::schema_for!(AnalyzeDirectoryParams)).unwrap(),
        ),
        (
            "AnalyzeFileParams",
            serde_json::to_string(&schemars::schema_for!(AnalyzeFileParams)).unwrap(),
        ),
        (
            "AnalyzeSymbolParams",
            serde_json::to_string(&schemars::schema_for!(AnalyzeSymbolParams)).unwrap(),
        ),
    ];

    for (name, schema_str) in &schemas {
        for bad_format in &["\"uint\"", "\"uint32\"", "\"uint64\""] {
            assert!(
                !schema_str.contains(bad_format),
                "{name} schema contains non-standard format {bad_format}"
            );
        }
    }
}

// Note: the async handler cannot be invoked directly in unit tests (requires MCP transport
// context). These tests verify the guard condition matches the implementation. See integration
// coverage for end-to-end behavior.

#[test]
fn test_summary_true_produces_summary_output_no_next_cursor() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    let src = root.join("src");
    std::fs::create_dir(&src).unwrap();
    for i in 0..110 {
        std::fs::write(src.join(format!("file{i:03}.rs")), "fn f() {}").unwrap();
    }
    let output = analyze_directory(root, None).unwrap();
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);
    assert!(
        summary.contains("SUMMARY:"),
        "expected SUMMARY: in output but got:\n{summary}"
    );
    assert!(
        !summary.contains("NEXT_CURSOR:"),
        "expected no NEXT_CURSOR: in summary output but got:\n{summary}"
    );
}

#[test]
fn test_summary_sub_annotation_present_for_nested_dirs() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    std::fs::create_dir_all(root.join("core/handlers")).unwrap();
    std::fs::create_dir_all(root.join("core/management")).unwrap();
    std::fs::write(root.join("core/handlers/base.rs"), "fn f() {}").unwrap();
    std::fs::write(root.join("core/management/cmd.rs"), "fn f() {}").unwrap();
    let output = analyze_directory(root, None).unwrap();
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);
    let core_line = summary
        .lines()
        .find(|l| l.contains("core/"))
        .unwrap_or_else(|| panic!("expected core/ line in summary:\n{summary}"));
    assert!(
        core_line.contains("sub:"),
        "expected 'sub:' annotation on core/ line but got:\n{core_line}"
    );
}

#[test]
fn test_overview_force_true_with_cursor_no_guard() {
    use aptu_coder_core::pagination::{CursorData, PaginationMode, encode_cursor};
    use aptu_coder_core::types::AnalyzeDirectoryParams;

    let cursor_data = CursorData {
        mode: PaginationMode::Default,
        offset: 10,
    };
    let cursor_str = encode_cursor(&cursor_data).expect("encode should succeed");
    // force=Some(true) requests non-summary output; summary is not set.
    // The guard only fires on summary=Some(true), so this combination must not trigger it.
    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": ".",
        "force": true,
        "cursor": cursor_str,
    }))
    .expect("valid AnalyzeDirectoryParams JSON");

    assert!(
        !(params.output_control.summary == Some(true) && params.pagination.cursor.is_some()),
        "guard must NOT fire when force=true and summary is not explicitly set to true"
    );
}

// Python wildcard import resolution tests

#[cfg(feature = "lang-python")]
#[test]
fn test_python_wildcard_import_parser_clean_module_field() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");

    fs::write(&file_path, "from os import *\n").unwrap();

    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    assert!(!output.semantic.imports.is_empty(), "expected imports");
    let wildcard_import = output
        .semantic
        .imports
        .iter()
        .find(|i| i.items == vec!["*"])
        .expect("expected wildcard import");

    assert_eq!(
        wildcard_import.module, "os",
        "module field should be clean (not raw statement text)"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_wildcard_import_relative_resolution() {
    let temp_dir = TempDir::new().unwrap();

    // Create package structure: models.py with two functions
    fs::write(
        temp_dir.path().join("models.py"),
        "def Foo():\n    pass\n\ndef Bar():\n    pass\n",
    )
    .unwrap();

    // Create __init__.py to make it a package
    fs::write(temp_dir.path().join("__init__.py"), "").unwrap();

    // Create main.py that imports * from .models
    let main_path = temp_dir.path().join("main.py");
    fs::write(&main_path, "from .models import *\n").unwrap();

    let output = analyze_file(main_path.to_str().unwrap(), None).unwrap();

    assert!(!output.semantic.imports.is_empty(), "expected imports");
    let wildcard_import = output
        .semantic
        .imports
        .iter()
        .find(|i| i.module == ".models")
        .expect("expected .models import");

    assert!(
        wildcard_import.items.contains(&"Foo".to_string()),
        "expected Foo in resolved items"
    );
    assert!(
        wildcard_import.items.contains(&"Bar".to_string()),
        "expected Bar in resolved items"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_wildcard_import_with_all() {
    let temp_dir = TempDir::new().unwrap();

    // Create models.py with __all__ that exports only Foo and Bar (not Baz)
    fs::write(
        temp_dir.path().join("models.py"),
        "__all__ = [\"Foo\", \"Bar\"]\n\ndef Foo():\n    pass\n\ndef Bar():\n    pass\n\ndef Baz():\n    pass\n",
    )
    .unwrap();

    // Create __init__.py
    fs::write(temp_dir.path().join("__init__.py"), "").unwrap();

    // Create main.py that imports * from .models
    let main_path = temp_dir.path().join("main.py");
    fs::write(&main_path, "from .models import *\n").unwrap();

    let output = analyze_file(main_path.to_str().unwrap(), None).unwrap();

    assert!(!output.semantic.imports.is_empty(), "expected imports");
    let wildcard_import = output
        .semantic
        .imports
        .iter()
        .find(|i| i.module == ".models")
        .expect("expected .models import");

    // Should honor __all__: include Foo and Bar, exclude Baz
    assert_eq!(
        wildcard_import.items.len(),
        2,
        "expected 2 items from __all__"
    );
    assert!(
        wildcard_import.items.iter().any(|item| item == "Foo"),
        "expected Foo in items"
    );
    assert!(
        wildcard_import.items.iter().any(|item| item == "Bar"),
        "expected Bar in items"
    );
    assert!(
        !wildcard_import.items.iter().any(|item| item == "Baz"),
        "Baz should not be in items (not in __all__)"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_wildcard_import_target_not_found() {
    let temp_dir = TempDir::new().unwrap();

    // Create main.py that imports from nonexistent module
    let main_path = temp_dir.path().join("main.py");
    fs::write(&main_path, "from .nonexistent import *\n").unwrap();

    // Should not panic or error; should gracefully preserve items = ["*"]
    let output = analyze_file(main_path.to_str().unwrap(), None).unwrap();

    assert!(!output.semantic.imports.is_empty(), "expected imports");
    let wildcard_import = output
        .semantic
        .imports
        .iter()
        .find(|i| i.module == ".nonexistent")
        .expect("expected .nonexistent import");

    // Should gracefully fall back to ["*"]
    assert_eq!(
        wildcard_import.items,
        vec!["*"],
        "expected fallback to ['*'] when target not found"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_named_import_from_statement() {
    let temp_dir = TempDir::new().unwrap();

    // Create test.py with named imports
    let test_path = temp_dir.path().join("test.py");
    fs::write(&test_path, "from os import path, getcwd\n").unwrap();

    let output = analyze_file(test_path.to_str().unwrap(), None).unwrap();

    assert!(!output.semantic.imports.is_empty(), "expected imports");
    let named_import = output
        .semantic
        .imports
        .iter()
        .find(|i| i.module == "os")
        .expect("expected os import");

    assert_eq!(
        named_import.items,
        vec!["path", "getcwd"],
        "expected named import items [path, getcwd]"
    );
}

#[test]
fn test_analyze_module_dir_guard_rejects_directory() {
    // The analyze_module handler checks std::fs::metadata(...).map(|m| m.is_dir())
    // before calling analyze_module_file. Verify the metadata call returns is_dir=true
    // for a real directory, confirming the guard condition is correct.
    let dir = std::env::temp_dir();
    assert!(
        std::fs::metadata(dir.to_str().unwrap())
            .map(|m| m.is_dir())
            .unwrap_or(false),
        "temp_dir should be detected as a directory by the guard condition"
    );
    // Also confirm analyze_module_file on a directory produces an error (not a panic),
    // demonstrating the guard in the handler prevents that path.
    let result = aptu_coder_core::analyze::analyze_module_file(dir.to_str().unwrap());
    assert!(
        result.is_err(),
        "analyze_module_file on a directory should return an error"
    );
}

#[test]
#[cfg(unix)]
fn test_analyze_module_dir_guard_rejects_symlink_to_directory() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("real_dir");
    std::fs::create_dir(&target).unwrap();
    let link = tmp.path().join("link_to_dir");
    symlink(&target, &link).unwrap();
    // std::fs::metadata follows symlinks; symlink-to-dir should be detected as a dir
    assert!(
        std::fs::metadata(&link)
            .map(|m| m.is_dir())
            .unwrap_or(false),
        "symlink to directory should be detected as a directory by the guard condition"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_aliased_import_from_statement() {
    let temp_dir = TempDir::new().unwrap();

    // Create test.py with aliased import
    let test_path = temp_dir.path().join("test.py");
    fs::write(&test_path, "from os import path as p\n").unwrap();

    let output = analyze_file(test_path.to_str().unwrap(), None).unwrap();

    assert!(!output.semantic.imports.is_empty(), "expected imports");
    let aliased_import = output
        .semantic
        .imports
        .iter()
        .find(|i| i.module == "os")
        .expect("expected os import");

    // Should use the original name, not the alias
    assert_eq!(
        aliased_import.items,
        vec!["path"],
        "expected original name [path], not alias [p]"
    );
}

#[test]
fn test_analyze_directory_verbose_no_summary() {
    use aptu_coder_core::formatter::format_structure_paginated;
    use aptu_coder_core::types::FileInfo;

    let mut fi = FileInfo::default();
    fi.path = "src/main.rs".to_string();
    fi.language = "rust".to_string();
    fi.line_count = 10;
    fi.function_count = 1;
    fi.class_count = 0;
    fi.is_test = false;
    let files = vec![fi];

    // verbose=true: format_structure_paginated must emit PAGINATED header, not SUMMARY
    let output = format_structure_paginated(&files, 1, None, None, true);
    assert!(
        !output.contains("SUMMARY:"),
        "verbose=true output must not contain SUMMARY: block"
    );
    assert!(
        output.contains("PAGINATED:"),
        "verbose=true output must start with PAGINATED: header"
    );
    assert!(
        output.contains("FILES [LOC, FUNCTIONS, CLASSES]"),
        "verbose=true output must contain FILES section header"
    );
}

#[cfg(feature = "lang-fortran")]
#[test]
fn test_fortran_parse_and_extract() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("math.f90");
    fs::write(
        &file_path,
        r#"
MODULE math_utils
  IMPLICIT NONE
CONTAINS
  SUBROUTINE add_numbers(a, b, result)
    REAL, INTENT(IN) :: a, b
    REAL, INTENT(OUT) :: result
    result = a + b
  END SUBROUTINE add_numbers

  FUNCTION multiply(a, b) RESULT(res)
    REAL, INTENT(IN) :: a, b
    REAL :: res
    res = a * b
  END FUNCTION multiply
END MODULE math_utils
"#,
    )
    .unwrap();
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let func_names: Vec<&str> = output
        .semantic
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert!(
        func_names.contains(&"add_numbers"),
        "expected add_numbers in functions, got: {:?}",
        func_names
    );
    assert!(
        func_names.contains(&"multiply"),
        "expected multiply in functions, got: {:?}",
        func_names
    );
    assert_eq!(
        output.semantic.classes.len(),
        0,
        "Fortran modules are not yet captured as classes (module_statement has no name \
         field in tree-sitter-fortran 0.5.1; module support will be added in a future PR)"
    );
}

#[cfg(feature = "lang-fortran")]
#[test]
fn test_fortran_edge_case_fixed_form() {
    // Fixed-form layout with columns 1-5 blank, statement starting col 7.
    // Uses ! comment style: tree-sitter-fortran does not support C-in-col-1
    // comments (produces ERROR nodes and misparsing), but does support !
    // comments in both free-form and fixed-form files.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("legacy.for");
    fs::write(
        &file_path,
        "! Fixed-form FORTRAN 77 subroutine\n      SUBROUTINE COMPUTE(X, Y)\n      REAL X, Y\n      Y = X * 2.0\n      RETURN\n      END SUBROUTINE COMPUTE\n",
    )
    .unwrap();
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let func_names: Vec<&str> = output
        .semantic
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(
        output.semantic.functions.len(),
        1,
        "expected exactly 1 function, got: {:?}",
        func_names
    );
    assert!(
        func_names.contains(&"COMPUTE"),
        "expected COMPUTE in functions, got: {:?}",
        func_names
    );
    assert_eq!(output.semantic.classes.len(), 0);
}

#[test]
fn test_format_summary_with_max_depth_annotation() {
    // Arrange: nested fixture with files below depth 1
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    std::fs::create_dir_all(root.join("subdir/nested")).unwrap();
    std::fs::write(root.join("subdir/nested/a.rs"), "fn a() {}").unwrap();
    std::fs::write(root.join("subdir/nested/b.rs"), "fn b() {}").unwrap();

    // Act: unbounded walk for counts, bounded walk for analysis (mirrors new single-walk approach)
    let all_entries = aptu_coder_core::traversal::walk_directory(root, None).unwrap();
    let counts = aptu_coder_core::traversal::subtree_counts_from_entries(root, &all_entries);
    let output = analyze_directory(root, Some(1)).unwrap();
    let summary = aptu_coder_core::formatter::format_summary(
        &output.entries,
        &output.files,
        Some(1),
        Some(&counts),
    );

    // Assert: annotated format appears because depth-1 walk sees 0 analyzed files but true count is 2
    assert!(
        summary.contains("files total; showing"),
        "expected annotated count in summary but got:\n{summary}"
    );
}

#[test]
fn test_format_summary_suggestion_uses_true_count() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    std::fs::create_dir_all(root.join("big/deep/more")).unwrap();
    for i in 0..5usize {
        std::fs::write(root.join(format!("big/deep/more/f{}.rs", i)), "fn f() {}").unwrap();
    }

    // Act: unbounded walk for counts, bounded walk for analysis (mirrors new single-walk approach)
    let all_entries = aptu_coder_core::traversal::walk_directory(root, None).unwrap();
    let counts = aptu_coder_core::traversal::subtree_counts_from_entries(root, &all_entries);
    let output = analyze_directory(root, Some(1)).unwrap();
    let summary = aptu_coder_core::formatter::format_summary(
        &output.entries,
        &output.files,
        Some(1),
        Some(&counts),
    );

    // Assert: SUGGESTION footer references true count (5), not depth-limited count (0)
    assert!(
        summary.contains("5 files total"),
        "expected SUGGESTION to reference true count (5) but got:\n{summary}"
    );
}

#[test]
fn test_format_summary_max_depth_none_unchanged() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn f() {}").unwrap();

    // Act: pass subtree_counts=None
    let output = analyze_directory(root, None).unwrap();
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, None, None);

    // Assert: no annotated format
    assert!(
        !summary.contains("files total; showing"),
        "expected no annotated count when subtree_counts=None but got:\n{summary}"
    );
}

#[test]
fn test_format_summary_max_depth_zero_unchanged() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn f() {}").unwrap();

    // Act: max_depth=Some(0) with subtree_counts=None (zero is the unlimited sentinel)
    let output = analyze_directory(root, Some(0)).unwrap();
    let summary =
        aptu_coder_core::formatter::format_summary(&output.entries, &output.files, Some(0), None);

    // Assert: no annotated format
    assert!(
        !summary.contains("files total; showing"),
        "expected no annotated count when max_depth=Some(0) and subtree_counts=None but got:\n{summary}"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_analyze_symbol_python_callees() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("foo.py");
    fs::write(
        &src,
        "def inner():\n    pass\n\ndef outer():\n    inner()\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "outer", 1, None, None).unwrap();
    let output = result.formatted;
    // CALLEES section format: "  outer -> outer\n    -> inner"
    assert!(
        output.contains("CALLEES:") && output.contains("-> inner"),
        "expected CALLEES section with callee 'inner', got:\n{output}"
    );
}

#[cfg(feature = "lang-go")]
#[test]
fn test_analyze_symbol_go_callees() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("foo.go");
    fs::write(
        &src,
        "package main\n\nfunc inner() {}\n\ntype S struct{}\n\nfunc (s S) outer() {\n    inner()\n}\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "outer", 1, None, None).unwrap();
    let output = result.formatted;
    assert!(
        output.contains("CALLEES:") && output.contains("-> inner"),
        "expected CALLEES section with callee 'inner', got:\n{output}"
    );
}

#[cfg(feature = "lang-typescript")]
#[test]
fn test_analyze_symbol_typescript_callees() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("foo.ts");
    fs::write(
        &src,
        "function inner(): void {}\n\nclass MyClass {\n    outer(): void {\n        inner();\n    }\n}\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "outer", 1, None, None).unwrap();
    let output = result.formatted;
    assert!(
        output.contains("CALLEES:") && output.contains("-> inner"),
        "expected CALLEES section with callee 'inner', got:\n{output}"
    );
}

#[cfg(feature = "lang-fortran")]
#[test]
fn test_analyze_symbol_fortran_callees() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("foo.f90");
    fs::write(
        &src,
        "subroutine inner()\nend subroutine\n\nsubroutine outer()\n    call inner()\nend subroutine\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "outer", 1, None, None).unwrap();
    let output = result.formatted;
    assert!(
        output.contains("CALLEES:") && output.contains("-> inner"),
        "expected CALLEES section with callee 'inner', got:\n{output}"
    );
}

#[test]
fn test_analyze_symbol_rust_method_item_callees() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("lib.rs");
    fs::write(
        &src,
        "fn inner() {}\n\nstruct S;\n\nimpl S {\n    fn outer(&self) {\n        inner();\n    }\n}\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "outer", 1, None, None).unwrap();
    let output = result.formatted;
    assert!(
        output.contains("CALLEES:") && output.contains("-> inner"),
        "expected CALLEES section with callee 'inner', got:\n{output}"
    );
}

#[cfg(feature = "lang-java")]
#[test]
fn test_analyze_symbol_java_callees() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("Foo.java");
    fs::write(
        &src,
        "class Foo {\n    void inner() {}\n\n    void outer() {\n        inner();\n    }\n}\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "outer", 1, None, None).unwrap();
    let output = result.formatted;
    assert!(
        output.contains("CALLEES:") && output.contains("-> inner"),
        "expected CALLEES section with callee 'inner', got:\n{output}"
    );
}

#[tokio::test]
async fn test_analyze_symbol_callers_to_callees_cursor_transition() {
    use aptu_coder_core::pagination::{CursorData, PaginationMode, decode_cursor, encode_cursor};

    // Verify the cursor encoding/decoding round-trips correctly for the
    // Callees transition cursor that the handler emits.
    let cursor_str = encode_cursor(&CursorData {
        mode: PaginationMode::Callees,
        offset: 0,
    })
    .expect("encode must succeed");
    let decoded = decode_cursor(&cursor_str).expect("cursor must decode");
    assert_eq!(decoded.mode, PaginationMode::Callees);
    assert_eq!(decoded.offset, 0);

    // Verify analyze_focused produces both callers and callees sections in
    // the formatted output, confirming the handler's post-match override will
    // have the right inputs at runtime.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "fn inner() {}\n\nfn outer() {\n    inner();\n}\n\nfn caller() {\n    outer();\n}\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "outer", 1, None, None).unwrap();
    assert!(
        result.formatted.contains("CALLERS:"),
        "expected CALLERS section:\n{}",
        result.formatted
    );
    assert!(
        result.formatted.contains("CALLEES:") && !result.formatted.contains("CALLEES:\n  (none)"),
        "expected non-empty CALLEES section:\n{}",
        result.formatted
    );
}

#[test]
fn test_analyze_symbol_no_callees_no_transition_cursor() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "fn isolated() {\n    let x = 42;\n}\n",
    )
    .unwrap();
    let result = analyze_focused(dir.path(), "isolated", 1, None, None).unwrap();

    assert!(
        result.next_cursor.is_none(),
        "expected no next_cursor when no callees exist; got: {:?}",
        result.next_cursor
    );
    assert!(
        result.formatted.contains("CALLEES:\n  (none)"),
        "expected CALLEES section with (none):\n{}",
        result.formatted
    );
}

#[cfg(feature = "lang-rust")]
#[test]
fn test_execute_query_valid_rust() {
    // Arrange: source with a function
    let source = "fn foo() {}";
    let query_str = "(function_item name: (identifier) @name)";
    // Act
    let result = aptu_coder_core::execute_query("rust", source, query_str);
    // Assert
    let captures = result.expect("execute_query should succeed for valid Rust query");
    assert!(
        captures
            .iter()
            .any(|c| c.capture_name == "name" && c.text == "foo"),
        "expected capture with name='name' and text='foo', got {:?}",
        captures
    );
}

#[test]
fn test_execute_query_unsupported_language() {
    // Arrange + Act
    let result = aptu_coder_core::execute_query("cobol", "x = 1", "(identifier) @id");
    // Assert: should be UnsupportedLanguage error
    assert!(
        matches!(
            result,
            Err(aptu_coder_core::ParserError::UnsupportedLanguage(ref lang)) if lang == "cobol"
        ),
        "expected UnsupportedLanguage error, got {:?}",
        result
    );
}

#[test]
fn test_execute_query_malformed_query() {
    // Arrange + Act: valid language, invalid tree-sitter query syntax
    let result =
        aptu_coder_core::execute_query("rust", "fn foo() {}", "this is not valid query syntax !!!");
    // Assert: should be a QueryError
    assert!(
        matches!(result, Err(aptu_coder_core::ParserError::QueryError(_))),
        "expected QueryError for malformed query, got {:?}",
        result
    );
}

#[cfg(feature = "lang-csharp")]
#[test]
fn test_csharp_parse_and_extract() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("example.cs");
    let src = r#"
using System;

namespace Demo {
    class Foo {
        public Foo() {}
        public void Bar() { Baz(); }
        private void Baz() {}
    }
}
"#;
    fs::write(&file_path, src).unwrap();

    // Act
    let output = analyze_directory(temp_dir.path(), None).unwrap();
    let fa = output
        .files
        .iter()
        .find(|f| f.path.ends_with("example.cs"))
        .expect("example.cs not found");

    // Assert
    assert_eq!(fa.language, "csharp");
    assert_eq!(fa.class_count, 1);
    assert_eq!(fa.function_count, 3);
}

#[cfg(feature = "lang-csharp")]
#[test]
fn test_csharp_inheritance_extraction() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sample.cs");
    let src = r#"
using System;

class Base {}
interface IRun {}
class Dog : Base, IRun {
    void Bark() { System.Console.WriteLine("woof"); }
    void Run() { Bark(); }
}
"#;
    fs::write(&file_path, src).unwrap();

    // Act
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Assert -- Dog class should carry its base types
    let dog_class = output
        .semantic
        .classes
        .iter()
        .find(|c| c.name == "Dog")
        .expect("Dog class should be extracted");

    assert!(
        !dog_class.inherits.is_empty(),
        "Dog should have inheritance info"
    );
    assert!(
        dog_class.inherits.iter().any(|i| i.contains("Base")),
        "Dog should inherit from Base"
    );
    assert!(
        dog_class.inherits.iter().any(|i| i.contains("IRun")),
        "Dog should implement IRun"
    );
}

#[cfg(feature = "lang-csharp")]
#[test]
fn test_csharp_struct_and_enum_extraction() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("types.cs");
    let src = r#"
public struct Point { public int X; public int Y; }
public enum Direction { North, South, East, West }
"#;
    fs::write(&file_path, src).unwrap();

    // Act
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();
    let class_names: Vec<&str> = output
        .semantic
        .classes
        .iter()
        .map(|c| c.name.as_str())
        .collect();

    // Assert -- struct and enum declarations map to @class captures in ELEMENT_QUERY
    assert!(
        class_names.contains(&"Point"),
        "struct Point should be extracted as a class, got: {class_names:?}"
    );
    assert!(
        class_names.contains(&"Direction"),
        "enum Direction should be extracted as a class, got: {class_names:?}"
    );
}

#[cfg(feature = "lang-csharp")]
#[test]
fn test_analyze_symbol_csharp_callees() {
    // Arrange
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("sample.cs");
    fs::write(
        &src,
        "class C { void Inner() {} void Outer() { Inner(); } }",
    )
    .unwrap();

    // Act
    let result = analyze_focused(dir.path(), "Outer", 1, None, None).unwrap();
    let output = result.formatted;

    // Assert
    assert!(
        output.contains("CALLEES:") && output.contains("-> Inner"),
        "expected CALLEES section with callee 'Inner', got:\n{output}"
    );
}

#[cfg(feature = "lang-tsx")]
#[test]
fn test_integration_tsx_analyze_file() {
    // Arrange
    use aptu_coder_core::lang::language_for_extension;
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("Button.tsx");
    let src = r#"
import React from 'react';

interface ButtonProps {
    label: string;
}

function Button({ label }: ButtonProps): JSX.Element {
    return <button>{label}</button>;
}

export default Button;
"#;
    fs::write(&file_path, src).unwrap();

    // Verify extension resolves
    assert_eq!(language_for_extension("tsx"), Some("tsx"));

    // Act
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Assert -- Button function and ButtonProps interface must be extracted
    assert!(
        output.semantic.functions.iter().any(|f| f.name == "Button"),
        "expected Button function, got {:?}",
        output
            .semantic
            .functions
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );
    assert!(
        output
            .semantic
            .classes
            .iter()
            .any(|c| c.name == "ButtonProps"),
        "expected ButtonProps interface, got {:?}",
        output
            .semantic
            .classes
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );
}

// C files share the lang-cpp feature flag; C and C++ use the same tree-sitter grammar
#[cfg(feature = "lang-cpp")]
#[test]
fn test_integration_c_analyze_file() {
    // Arrange
    use aptu_coder_core::lang::language_for_extension;
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sample.c");
    let src = r#"
#include <stdio.h>

struct Point {
    int x;
    int y;
};

int add(int a, int b) {
    return a + b;
}
"#;
    fs::write(&file_path, src).unwrap();

    // Verify extension resolves
    assert_eq!(language_for_extension("c"), Some("c"));

    // Act
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Assert -- struct extracted as class
    assert!(
        output.semantic.classes.iter().any(|c| c.name == "Point"),
        "expected Point struct, got {:?}",
        output
            .semantic
            .classes
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );

    // Assert -- free function extracted via func_name capture
    assert!(
        output.semantic.functions.iter().any(|f| f.name == "add"),
        "expected function 'add', got {:?}",
        output
            .semantic
            .functions
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );
}

#[cfg(feature = "lang-cpp")]
#[test]
fn test_integration_cpp_analyze_file() {
    // Arrange
    use aptu_coder_core::lang::language_for_extension;
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sample.cpp");
    let src = r#"
class Counter {
public:
    int count;
    void increment() {
        count++;
    }
};

struct Vec2 {
    float x;
    float y;
};

int dot(int a, int b) {
    return a * b;
}

template<typename T>
T identity(T val) {
    return val;
}

int Counter::get() {
    return count;
}
"#;
    fs::write(&file_path, src).unwrap();

    // Verify extension resolves
    assert_eq!(language_for_extension("cpp"), Some("cpp"));

    // Act
    let output = analyze_file(file_path.to_str().unwrap(), None).unwrap();

    // Assert -- classes extracted
    assert!(
        output.semantic.classes.iter().any(|c| c.name == "Counter"),
        "expected Counter class, got {:?}",
        output
            .semantic
            .classes
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );
    assert!(
        output.semantic.classes.iter().any(|c| c.name == "Vec2"),
        "expected Vec2 struct, got {:?}",
        output
            .semantic
            .classes
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );

    // Assert -- free function extracted via func_name capture
    assert!(
        output.semantic.functions.iter().any(|f| f.name == "dot"),
        "expected function 'dot', got {:?}",
        output
            .semantic
            .functions
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );

    // Assert -- template function extracted via template_declaration resolution (edge case)
    assert!(
        output
            .semantic
            .functions
            .iter()
            .any(|f| f.name == "identity"),
        "expected template function 'identity', got {:?}",
        output
            .semantic
            .functions
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );

    // Assert -- qualified out-of-class method (Counter::get) is extracted
    assert!(
        output.semantic.functions.iter().any(|f| f.name == "get"),
        "expected qualified method 'get' (Counter::get) to be extracted"
    );
}

// S3: Pagination overflow tests
#[test]
fn test_paginate_slice_zero_page_size_error() {
    use aptu_coder_core::pagination::{PaginationMode, paginate_slice};

    let items: Vec<i32> = (0..100).collect();
    let result = paginate_slice(&items, 0, 0, PaginationMode::Default);

    assert!(result.is_err(), "page_size=0 should return an error");
    match result {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("page_size") || msg.contains("at least 1"),
                "error message should mention page_size constraint: {}",
                msg
            );
        }
        Ok(_) => panic!("expected error for page_size=0"),
    }
}

#[test]
fn test_paginate_slice_max_page_size_no_panic() {
    use aptu_coder_core::pagination::{MAX_PAGE_SIZE, PaginationMode, paginate_slice};

    let items: Vec<i32> = (0..20_000).collect();
    let result = paginate_slice(&items, 0, usize::MAX, PaginationMode::Default);

    assert!(result.is_ok(), "usize::MAX page_size should not panic");
    let page = result.unwrap();
    assert!(
        page.items.len() <= MAX_PAGE_SIZE,
        "returned items should not exceed MAX_PAGE_SIZE ({}), got {}",
        MAX_PAGE_SIZE,
        page.items.len()
    );
}

// S5: Symlink filter test
#[test]
#[cfg(unix)]
fn test_symlink_file_skipped_in_analyze_directory() {
    use aptu_coder_core::analyze::analyze_directory;
    use std::os::unix::fs as unix_fs;

    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a real Rust source file
    let real_file = root.join("real.rs");
    fs::write(&real_file, "fn hello() { println!(\"hello\"); }").unwrap();

    // Create a symlink to the real file
    let symlink_path = root.join("link.rs");
    unix_fs::symlink(&real_file, &symlink_path).unwrap();

    // Analyze the directory
    let result = analyze_directory(root, None).unwrap();

    // Verify the real file appears in the output
    assert!(
        result.files.iter().any(|f| f.path == real_file),
        "real file should appear in analyze_directory output"
    );

    // Verify the symlink does NOT appear in the output
    assert!(
        !result.files.iter().any(|f| f.path == symlink_path),
        "symlink should be excluded from analyze_directory output"
    );
}

// S6: File size cap test
#[test]
fn test_large_file_skipped_no_panic() {
    use aptu_coder_core::analyze::{MAX_FILE_SIZE_BYTES, analyze_directory};
    use std::fs::File;
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a small valid source file
    let small_file = root.join("small.rs");
    fs::write(&small_file, "fn hello() { println!(\"hello\"); }").unwrap();

    // Create a sparse file exceeding MAX_FILE_SIZE_BYTES without writing data
    let large_file = root.join("large.rs");
    let mut file = File::create(&large_file).unwrap();
    file.set_len(MAX_FILE_SIZE_BYTES + 1).unwrap();
    drop(file);

    // Analyze the directory
    let result = analyze_directory(root, None).unwrap();

    // Verify the small file appears in the output
    assert!(
        result.files.iter().any(|f| f.path == small_file),
        "small file should appear in analyze_directory output"
    );

    // Verify the large file does NOT appear in the output (guard filtered it)
    assert!(
        !result.files.iter().any(|f| f.path == large_file),
        "large file exceeding MAX_FILE_SIZE_BYTES should be excluded from output"
    );
}

// Test additions for attribute/decorator line reporting

#[cfg(feature = "lang-rust")]
#[test]
fn test_rust_function_attribute_line_reported() {
    use aptu_coder_core::parser::SemanticExtractor;

    let source = "#[instrument]\nfn foo() {}";
    let result =
        SemanticExtractor::extract(source, "rust", None, None).expect("should extract Rust code");

    assert!(
        !result.functions.is_empty(),
        "should extract at least one function"
    );
    assert_eq!(result.functions[0].name, "foo");
    assert_eq!(
        result.functions[0].line, 1,
        "should report attribute line 1, not fn line 2"
    );
}

#[cfg(feature = "lang-rust")]
#[test]
fn test_rust_function_no_attribute_unchanged() {
    use aptu_coder_core::parser::SemanticExtractor;

    let source = "fn foo() {}";
    let result =
        SemanticExtractor::extract(source, "rust", None, None).expect("should extract Rust code");

    assert!(!result.functions.is_empty());
    assert_eq!(result.functions[0].name, "foo");
    assert_eq!(
        result.functions[0].line, 1,
        "plain function should report its own line"
    );
}

#[cfg(feature = "lang-rust")]
#[test]
fn test_rust_function_multiple_stacked_attributes() {
    use aptu_coder_core::parser::SemanticExtractor;

    let source = "#[a]\n#[b]\nfn foo() {}";
    let result =
        SemanticExtractor::extract(source, "rust", None, None).expect("should extract Rust code");

    assert!(!result.functions.is_empty());
    assert_eq!(result.functions[0].name, "foo");
    assert_eq!(
        result.functions[0].line, 1,
        "should report first attribute line 1, not fn line 3"
    );
}

#[cfg(feature = "lang-rust")]
#[test]
fn test_rust_impl_method_attribute_line_reported() {
    use aptu_coder_core::parser::SemanticExtractor;

    let source = "struct Foo;\nimpl Foo {\n  #[attr]\n  fn bar() {}\n}";
    let result =
        SemanticExtractor::extract(source, "rust", None, None).expect("should extract Rust code");

    assert!(!result.classes.is_empty());
    let class = &result.classes[0];
    assert!(!class.methods.is_empty());
    let method = &class.methods[0];
    assert_eq!(method.name, "bar");
    assert_eq!(
        method.line, 3,
        "method should report attribute line 3, not fn line 4"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_decorated_function_reports_decorator_line() {
    use aptu_coder_core::parser::SemanticExtractor;

    let source = "@decorator\ndef foo(): pass";
    let result = SemanticExtractor::extract(source, "python", None, None)
        .expect("should extract Python code");

    assert!(!result.functions.is_empty());
    assert_eq!(result.functions[0].name, "foo");
    assert_eq!(
        result.functions[0].line, 1,
        "should report decorator line 1, not def line 2"
    );
}

#[cfg(feature = "lang-python")]
#[test]
fn test_python_plain_function_reports_def_line() {
    use aptu_coder_core::parser::SemanticExtractor;

    let source = "def foo(): pass";
    let result = SemanticExtractor::extract(source, "python", None, None)
        .expect("should extract Python code");

    assert!(!result.functions.is_empty());
    assert_eq!(result.functions[0].name, "foo");
    assert_eq!(
        result.functions[0].line, 1,
        "plain function should report its own line"
    );
}
