// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder_core::analyze::analyze_focused;
use aptu_coder_core::formatter::format_focused_summary;
use aptu_coder_core::graph::CallGraph;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_symbol_focus_summary_explicit_true() {
    // Arrange: Create a fixture with a function that has callers and callees
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target_fn() {
    helper_fn();
}

pub fn helper_fn() {
}

pub fn caller_fn() {
    target_fn();
}
"#,
    )
    .unwrap();

    // Act: Analyze symbol with focus
    let output = analyze_focused(root, "target_fn", 1, None, None).unwrap();

    // Assert: Output should be full format (not summary, because this is small output)
    // The summary format should have FOCUS header with counts
    assert!(
        output.formatted.contains("FOCUS:"),
        "Should have FOCUS header"
    );
    assert!(
        output.formatted.contains("target_fn"),
        "Should mention the symbol name"
    );
}

#[test]
fn test_symbol_focus_summary_format() {
    // Arrange: Build a small call graph manually for testing format_focused_summary
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    let test_path = root.join("test.rs");

    let mut graph = CallGraph::new();

    // Add a definition
    graph
        .definitions
        .insert("my_func".to_string(), vec![(test_path.clone(), 10)]);

    // Add some callers
    graph.callers.insert(
        "my_func".to_string(),
        vec![
            aptu_coder_core::types::CallEdge {
                path: root.join("caller1.rs"),
                line: 20,
                neighbor_name: "caller_a".to_string(),
                is_impl_trait: false,
            },
            aptu_coder_core::types::CallEdge {
                path: root.join("caller2.rs"),
                line: 30,
                neighbor_name: "caller_b".to_string(),
                is_impl_trait: false,
            },
        ],
    );

    // Add some callees
    graph.callees.insert(
        "my_func".to_string(),
        vec![
            aptu_coder_core::types::CallEdge {
                path: root.join("lib.rs"),
                line: 40,
                neighbor_name: "std::println".to_string(),
                is_impl_trait: false,
            },
            aptu_coder_core::types::CallEdge {
                path: root.join("lib.rs"),
                line: 50,
                neighbor_name: "helper".to_string(),
                is_impl_trait: false,
            },
        ],
    );

    // Act: Format as summary
    let summary =
        format_focused_summary(&graph, "my_func", 1, Some(root)).expect("Should format summary");

    // Assert: Check key sections exist
    assert!(
        summary.contains("FOCUS: my_func"),
        "Should have FOCUS header"
    );
    assert!(summary.contains("DEPTH: 1"), "Should show depth");
    assert!(summary.contains("DEFINED:"), "Should have DEFINED section");
    assert!(
        summary.contains("CALLERS (top 10):"),
        "Should have CALLERS section"
    );
    assert!(
        summary.contains("CALLEES (top 10):"),
        "Should have CALLEES section"
    );
    assert!(
        summary.contains("SUGGESTION:"),
        "Should have SUGGESTION section"
    );
}

#[test]
fn test_symbol_focus_summary_size_limit() {
    // Arrange: Create a small fixture
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn simple_fn() {
}
"#,
    )
    .unwrap();

    // Act: Analyze symbol
    let output = analyze_focused(root, "simple_fn", 1, None, None).unwrap();

    // Assert: Output should be well under 5000 chars for small code
    assert!(
        output.formatted.len() < 5000,
        "Summary output should be compact"
    );
}

#[test]
fn test_symbol_focus_summary_with_test_callers() {
    // Arrange: Create fixture with both production and test callers
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();

    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target_fn() {
}

pub fn prod_caller() {
    target_fn();
}
"#,
    )
    .unwrap();

    fs::write(
        root.join("tests/test.rs"),
        r#"
#[test]
fn test_target() {
    target_fn();
}
"#,
    )
    .unwrap();

    // Act: Analyze symbol
    let output = analyze_focused(root, "target_fn", 1, None, None).unwrap();

    // Assert: Should have CALLERS section showing production callers
    assert!(
        output.formatted.contains("CALLERS"),
        "Should mention callers"
    );
    // The test caller summary should appear if there are test callers detected
    // (exact format depends on test file detection)
}

#[test]
fn test_use_summary_true_calls_format_focused_summary() {
    // Arrange: Create a fixture with a Rust file that has a symbol with callers/callees
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target_fn() {
    helper_fn();
}

pub fn helper_fn() {
    println!("helper");
}

pub fn caller_fn() {
    target_fn();
}

pub fn another_caller() {
    target_fn();
}
"#,
    )
    .unwrap();

    // Act: Call analyze_focused_with_progress with use_summary=true
    use aptu_coder_core::analyze::analyze_focused_with_progress;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio_util::sync::CancellationToken;

    let counter = Arc::new(AtomicUsize::new(0));
    let ct = CancellationToken::new();
    use aptu_coder_core::analyze::FocusedAnalysisConfig;
    use aptu_coder_core::types::SymbolMatchMode;
    let params = FocusedAnalysisConfig {
        focus: "target_fn".to_string(),
        match_mode: SymbolMatchMode::Exact,
        follow_depth: 1,
        max_depth: None,
        ast_recursion_limit: None,
        use_summary: true,
        impl_only: None,
        def_use: false,
        parse_timeout_micros: None,
    };
    let output = analyze_focused_with_progress(root, &params, counter, ct).unwrap();

    // Assert: Output should contain summary format markers
    assert!(
        output.formatted.contains("FOCUS:"),
        "Should have FOCUS header (summary marker)"
    );
    assert!(
        output.formatted.contains("CALLERS (top 10):"),
        "Should have 'CALLERS (top 10):' (summary marker, not full 'CALLERS:')"
    );
    assert!(
        !output.formatted.contains("CALLERS:\n"),
        "Should NOT have full format marker 'CALLERS:\\n'"
    );
}

#[test]
fn test_use_summary_false_calls_format_focused_full() {
    // Arrange: Create a fixture similar to above
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target_fn() {
    helper_fn();
}

pub fn helper_fn() {
    println!("helper");
}

pub fn caller_fn() {
    target_fn();
}

pub fn another_caller() {
    target_fn();
}
"#,
    )
    .unwrap();

    // Act: Call analyze_focused_with_progress with use_summary=false
    use aptu_coder_core::analyze::analyze_focused_with_progress;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio_util::sync::CancellationToken;

    let counter = Arc::new(AtomicUsize::new(0));
    let ct = CancellationToken::new();
    use aptu_coder_core::analyze::FocusedAnalysisConfig;
    use aptu_coder_core::types::SymbolMatchMode;
    let params = FocusedAnalysisConfig {
        focus: "target_fn".to_string(),
        match_mode: SymbolMatchMode::Exact,
        follow_depth: 1,
        max_depth: None,
        ast_recursion_limit: None,
        use_summary: false,
        impl_only: None,
        def_use: false,
        parse_timeout_micros: None,
    };
    let output = analyze_focused_with_progress(root, &params, counter, ct).unwrap();

    // Assert: Output should contain full format markers
    assert!(
        output.formatted.contains("CALLERS:\n"),
        "Should have full format marker 'CALLERS:\\n'"
    );
    assert!(
        !output.formatted.contains("CALLERS (top 10):"),
        "Should NOT have summary marker 'CALLERS (top 10):'"
    );
}
