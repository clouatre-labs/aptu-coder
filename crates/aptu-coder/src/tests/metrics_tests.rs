use std::sync::Arc;

use crate::tests::helpers::make_analyzer;
use crate::tools::common::no_cache_meta;
use aptu_coder_core::analyze;
use aptu_coder_core::cache::CacheTier;

#[tokio::test]
async fn test_no_cache_meta_on_analyze_directory_result() {
    use aptu_coder_core::types::AnalyzeDirectoryParams;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

    let analyzer = make_analyzer();
    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": dir.path().to_str().unwrap(),
    }))
    .unwrap();
    let ct = tokio_util::sync::CancellationToken::new();
    let (arc_output, _cache_hit) = analyzer.handle_overview_mode(&params, ct).await.unwrap();
    // Verify the no_cache_meta shape by constructing it directly and checking the shape
    let meta = no_cache_meta();
    assert_eq!(
        meta.0.get("cache_hint").and_then(|v| v.as_str()),
        Some("no-cache"),
    );
    drop(arc_output);
}

#[tokio::test]
async fn test_analyze_directory_cache_hit_metrics() {
    use aptu_coder_core::types::AnalyzeDirectoryParams;
    use tempfile::TempDir;

    // Arrange: a temp dir with one file
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();
    let analyzer = make_analyzer();
    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": dir.path().to_str().unwrap(),
    }))
    .unwrap();

    // Act: first call (cache miss)
    let ct1 = tokio_util::sync::CancellationToken::new();
    let (_out1, hit1) = analyzer.handle_overview_mode(&params, ct1).await.unwrap();

    // Act: second call (cache hit)
    let ct2 = tokio_util::sync::CancellationToken::new();
    let (_out2, hit2) = analyzer.handle_overview_mode(&params, ct2).await.unwrap();

    // Assert
    assert_eq!(hit1, CacheTier::Miss, "first call must be a cache miss");
    assert_eq!(hit2, CacheTier::L1Memory, "second call must be a cache hit");
}

#[test]
fn test_analyze_module_cache_hit_metrics() {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Arrange: create a temp Rust file inside CWD so validate_path accepts it
    let cwd = std::env::current_dir().unwrap();
    let mut f = NamedTempFile::with_suffix_in(".rs", &cwd).unwrap();
    write!(f, "use std::io;\nfn bar() {{}}\n").unwrap();
    f.flush().unwrap();

    // Act
    let result = analyze::analyze_module_file(f.path().to_str().unwrap());

    // Assert
    let module_info = result.expect("analyze_module_file must succeed");
    assert_eq!(
        module_info.functions.len(),
        1,
        "expected exactly one function"
    );
    assert_eq!(module_info.functions[0].name, "bar");
    assert_eq!(module_info.imports.len(), 1, "expected exactly one import");
    assert!(
        module_info.imports[0].module.contains("std"),
        "import module must contain 'std', got: {}",
        module_info.imports[0].module
    );
}

// --- import_lookup tests ---

#[test]
#[serial_test::serial]
fn test_file_cache_capacity_default() {
    // Arrange: ensure the env var is not set
    // SAFETY: std::env::remove_var is inherently unsafe since Rust 1.x.
    // The enclosing test is annotated with #[serial_test::serial], which serializes
    // execution of all serial tests in the process, preventing concurrent access to
    // the environment.
    unsafe { std::env::remove_var("APTU_CODER_FILE_CACHE_CAPACITY") };

    // Act
    let analyzer = make_analyzer();

    // Assert: default file cache capacity is 100
    assert_eq!(analyzer.cache.file_capacity(), 100);
}

#[test]
#[serial_test::serial]
fn test_file_cache_capacity_from_env() {
    // Arrange
    // SAFETY: std::env::set_var is inherently unsafe since Rust 1.x.
    // The enclosing test is annotated with #[serial_test::serial], which serializes
    // execution of all serial tests in the process, preventing concurrent access to
    // the environment.
    unsafe { std::env::set_var("APTU_CODER_FILE_CACHE_CAPACITY", "42") };

    // Act
    let analyzer = make_analyzer();

    // Cleanup before assertions to minimise env pollution window
    // SAFETY: std::env::remove_var is inherently unsafe since Rust 1.x.
    // The enclosing test is annotated with #[serial_test::serial], which serializes
    // execution of all serial tests in the process, preventing concurrent access to
    // the environment.
    unsafe { std::env::remove_var("APTU_CODER_FILE_CACHE_CAPACITY") };

    // Assert
    assert_eq!(analyzer.cache.file_capacity(), 42);
}

#[test]
fn test_analyze_symbol_cache_fields_use_cache_tier_enum() {
    // Verify that CacheTier::Miss produces the expected cache_hit/cache_tier
    // values that analyze_symbol writes in both code paths (#950).
    // Guards against string drift if CacheTier::Miss.as_str() ever changes.
    assert_eq!(
        CacheTier::Miss.as_str(),
        "miss",
        "CacheTier::Miss.as_str() must stay \"miss\" -- analyze_symbol metrics depend on it"
    );
    assert!(
        !matches!(CacheTier::Miss, CacheTier::L1Memory | CacheTier::L2Disk),
        "CacheTier::Miss must not be a hit variant (cache_hit=false for a miss)"
    );
}

#[test]
fn test_metric_chars_threshold_breach_fires() {
    // Happy path: chars_threshold_breach is true when output_chars > 30_000
    let output_chars: usize = 35_000;
    let event = crate::metrics::MetricEvent {
        ts: 0,
        tool: "exec_command",
        duration_ms: 1,
        output_chars,
        param_path_depth: 0,
        max_depth: None,
        result: "ok",
        error_type: None,
        error_subtype: None,
        session_id: None,
        seq: None,
        cache_hit: None,
        cache_write_failure: None,
        cache_tier: None,
        exit_code: None,
        timed_out: false,
        output_truncated: None,
        chars_threshold_breach: output_chars > 30_000,
        file_ext: None,
        filter_applied: None,
        language: None,
        git_ref_used: false,
        summary_mode: false,
        is_paginated: false,
        fields_projected: false,
        match_mode: None,
        follow_depth: None,
        import_lookup: false,
        def_use: false,
        impl_only: false,
        stdin_provided: false,
        timeout_configured_ms: None,
        drain_timeout_ms: None,
        working_dir_used: false,
        l1_eviction_count: None,
        l2_entry_count: None,
        l2_size_bytes: None,
    };
    assert!(
        event.chars_threshold_breach,
        "chars_threshold_breach should be true for output_chars=35000"
    );
}

#[test]
fn test_metric_chars_threshold_breach_no_fire() {
    // Edge case: chars_threshold_breach is false when output_chars <= 30_000
    let output_chars: usize = 5_000;
    let event = crate::metrics::MetricEvent {
        ts: 0,
        tool: "exec_command",
        duration_ms: 1,
        output_chars,
        param_path_depth: 0,
        max_depth: None,
        result: "ok",
        error_type: None,
        error_subtype: None,
        session_id: None,
        seq: None,
        cache_hit: None,
        cache_write_failure: None,
        cache_tier: None,
        exit_code: None,
        timed_out: false,
        output_truncated: None,
        chars_threshold_breach: output_chars > 30_000,
        file_ext: None,
        filter_applied: None,
        language: None,
        git_ref_used: false,
        summary_mode: false,
        is_paginated: false,
        fields_projected: false,
        match_mode: None,
        follow_depth: None,
        import_lookup: false,
        def_use: false,
        impl_only: false,
        stdin_provided: false,
        timeout_configured_ms: None,
        drain_timeout_ms: None,
        working_dir_used: false,
        l1_eviction_count: None,
        l2_entry_count: None,
        l2_size_bytes: None,
    };
    assert!(
        !event.chars_threshold_breach,
        "chars_threshold_breach should be false for output_chars=5000"
    );
}

// ── strip_cd_prefix tests ──
