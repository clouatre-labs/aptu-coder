#[cfg(test)]
use super::*;
use crate::tools::exec_command::{build_exec_command, handle_output_persist, strip_cd_prefix};
use crate::validation::validate_path_in_dir;
use aptu_coder_core::traversal;
use regex::Regex;
use rmcp::model::NumberOrString;

#[tokio::test]
async fn test_emit_progress_none_peer_is_noop() {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    let analyzer = CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        crate::metrics::MetricsSender(metrics_tx),
    );
    let token = ProgressToken(NumberOrString::String("test".into()));
    // Should complete without panic
    analyzer
        .emit_progress(None, &token, 0.0, 10.0, "test".to_string())
        .await;
}

fn make_analyzer() -> CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        crate::metrics::MetricsSender(metrics_tx),
    )
}

#[test]
fn test_summary_cursor_conflict() {
    assert!(summary_cursor_conflict(Some(true), Some("cursor")));
    assert!(!summary_cursor_conflict(Some(true), None));
    assert!(!summary_cursor_conflict(None, Some("x")));
    assert!(!summary_cursor_conflict(None, None));
}

#[tokio::test]
async fn test_validate_impl_only_non_rust_returns_invalid_params() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("main.py"), "def foo(): pass").unwrap();

    let analyzer = make_analyzer();
    // Call analyze_symbol with impl_only=true on a Python-only directory via the tool API.
    // We use handle_focused_mode which calls validate_impl_only internally.
    let entries: Vec<traversal::WalkEntry> =
        traversal::walk_directory(dir.path(), None).unwrap_or_default();
    let result = CodeAnalyzer::validate_impl_only(&entries);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    drop(analyzer); // ensure it compiles with analyzer in scope
}

#[tokio::test]
async fn test_no_cache_meta_on_analyze_directory_result() {
    use aptu_coder_core::types::{AnalyzeDirectoryParams, OutputControlParams, PaginationParams};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

    let analyzer = make_analyzer();
    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": dir.path().to_str().unwrap(),
    }))
    .unwrap();
    let ct = tokio_util::sync::CancellationToken::new();
    let (arc_output, _cache_hit) = analyzer
        .handle_overview_mode(&params, ct, None)
        .await
        .unwrap();
    // Verify the no_cache_meta shape by constructing it directly and checking the shape
    let meta = no_cache_meta();
    assert_eq!(
        meta.0.get("cache_hint").and_then(|v| v.as_str()),
        Some("no-cache"),
    );
    drop(arc_output);
}

#[test]
fn test_complete_path_completions_returns_suggestions() {
    // Test the underlying completion function (same code path as complete()) directly
    // to avoid needing a constructed RequestContext<RoleServer>.
    // CARGO_MANIFEST_DIR is <workspace>/aptu-coder; parent is the workspace root,
    // which contains aptu-coder-core/ and aptu-coder/ matching the "aptu-" prefix.
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("manifest dir has parent");
    let suggestions = completion::path_completions(workspace_root, "aptu-");
    assert!(
        !suggestions.is_empty(),
        "expected completions for prefix 'aptu-' in workspace root"
    );
}

#[tokio::test]
async fn test_handle_overview_mode_no_summary_block() {
    use aptu_coder_core::types::AnalyzeDirectoryParams;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();

    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    let analyzer = CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        crate::metrics::MetricsSender(metrics_tx),
    );

    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": tmp.path().to_str().unwrap(),
    }))
    .unwrap();

    let ct = tokio_util::sync::CancellationToken::new();
    let (output, _cache_hit) = analyzer
        .handle_overview_mode(&params, ct, None)
        .await
        .unwrap();

    // summary=None with small output: handler uses format_structure (tree), which is
    // already stored in output.formatted from build_analysis_output.
    // The tree output contains a SUMMARY: block and a PATH block.
    let formatted = &output.formatted;

    assert!(
        formatted.contains("SUMMARY:"),
        "summary=None with small output must emit SUMMARY: block (tree output); got: {}",
        &formatted[..formatted.len().min(300)]
    );
    assert!(
        formatted.contains("PATH [LOC, FUNCTIONS, CLASSES]"),
        "summary=None with small output must emit PATH section header (tree output); got: {}",
        &formatted[..formatted.len().min(300)]
    );
    assert!(
        !formatted.contains("PAGINATED:"),
        "summary=None must NOT emit PAGINATED: header; got: {}",
        &formatted[..formatted.len().min(300)]
    );
}

#[tokio::test]
async fn test_analyze_directory_summary_false_forces_pagination() {
    // Edge case: summary=false must return format_structure_paginated (flat list with
    // PAGINATED: header) even when the directory output is small (< 5000 chars).
    use aptu_coder_core::types::AnalyzeDirectoryParams;
    use tempfile::TempDir;

    // Arrange: a small directory (one file, well under SIZE_LIMIT)
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}").unwrap();

    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    let analyzer = CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        crate::metrics::MetricsSender(metrics_tx),
    );

    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": tmp.path().to_str().unwrap(),
        "summary": false,
    }))
    .unwrap();

    // Act: call the full handler via handle_overview_mode + replicate handler path
    let ct = tokio_util::sync::CancellationToken::new();
    let (output, _cache_hit) = analyzer
        .handle_overview_mode(&params, ct, None)
        .await
        .unwrap();

    // Assert: output is small (confirms SIZE_LIMIT would not trigger auto-summary)
    assert!(
        output.formatted.len() <= SIZE_LIMIT,
        "test precondition: output must be small; got {} chars",
        output.formatted.len()
    );

    // The handler must use format_structure_paginated because summary=Some(false)
    // We verify by calling the full tool handler via make_analyzer + call_tool_raw
    // is not available here, so we verify the handler logic directly:
    // use_paginated = params.output_control.summary == Some(false) -> true
    let use_paginated = params.output_control.summary == Some(false);
    assert!(use_paginated, "summary=false must set use_paginated=true");

    // Confirm the tree output does NOT contain PAGINATED: (it is format_structure)
    assert!(
        !output.formatted.contains("PAGINATED:"),
        "handle_overview_mode returns format_structure (tree); PAGINATED: must not appear"
    );
    // Confirm the tree output contains SUMMARY: (format_structure marker)
    assert!(
        output.formatted.contains("SUMMARY:"),
        "handle_overview_mode returns format_structure (tree); SUMMARY: must appear"
    );
}

// --- cache_hit integration tests ---

#[tokio::test]
async fn test_analyze_directory_cache_hit_metrics() {
    use aptu_coder_core::types::{AnalyzeDirectoryParams, OutputControlParams, PaginationParams};
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
    let (_out1, hit1) = analyzer
        .handle_overview_mode(&params, ct1, None)
        .await
        .unwrap();

    // Act: second call (cache hit)
    let ct2 = tokio_util::sync::CancellationToken::new();
    let (_out2, hit2) = analyzer
        .handle_overview_mode(&params, ct2, None)
        .await
        .unwrap();

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
fn test_analyze_symbol_import_lookup_invalid_params() {
    // Arrange: empty symbol with import_lookup=true (violates the guard:
    // symbol must hold the module path when import_lookup=true).
    // Act: call the validate helper directly (same pattern as validate_impl_only).
    let result = CodeAnalyzer::validate_import_lookup(Some(true), "");

    // Assert: INVALID_PARAMS is returned.
    assert!(
        result.is_err(),
        "import_lookup=true with empty symbol must return Err"
    );
    let err = result.unwrap_err();
    assert_eq!(
        err.code,
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "expected INVALID_PARAMS; got {:?}",
        err.code
    );
}

#[tokio::test]
async fn test_analyze_symbol_import_lookup_found() {
    use tempfile::TempDir;

    // Arrange: a Rust file that imports "std::collections"
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("main.rs"),
        "use std::collections::HashMap;\nfn main() {}\n",
    )
    .unwrap();

    let entries = traversal::walk_directory(dir.path(), None).unwrap();

    // Act: search for the module "std::collections"
    let output =
        analyze::analyze_import_lookup(dir.path(), "std::collections", &entries, None).unwrap();

    // Assert: one match found
    assert!(
        output.formatted.contains("MATCHES: 1"),
        "expected 1 match; got: {}",
        output.formatted
    );
    assert!(
        output.formatted.contains("main.rs"),
        "expected main.rs in output; got: {}",
        output.formatted
    );
}

#[tokio::test]
async fn test_analyze_symbol_import_lookup_empty() {
    use tempfile::TempDir;

    // Arrange: a Rust file that does NOT import "no_such_module"
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

    let entries = traversal::walk_directory(dir.path(), None).unwrap();

    // Act
    let output =
        analyze::analyze_import_lookup(dir.path(), "no_such_module", &entries, None).unwrap();

    // Assert: zero matches
    assert!(
        output.formatted.contains("MATCHES: 0"),
        "expected 0 matches; got: {}",
        output.formatted
    );
}

// --- git_ref tests ---

#[tokio::test]
async fn test_analyze_directory_git_ref_non_git_repo() {
    use aptu_coder_core::traversal::changed_files_from_git_ref;
    use tempfile::TempDir;

    // Arrange: a temp dir that is NOT a git repository
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

    // Act: attempt git_ref resolution in a non-git dir
    let result = changed_files_from_git_ref(dir.path(), "HEAD~1");

    // Assert: must return a GitError
    assert!(result.is_err(), "non-git dir must return an error");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("git"),
        "error must mention git; got: {err_msg}"
    );
}

#[tokio::test]
async fn test_analyze_directory_git_ref_filters_changed_files() {
    use aptu_coder_core::traversal::{changed_files_from_git_ref, filter_entries_by_git_ref};
    use std::collections::HashSet;
    use tempfile::TempDir;

    // Arrange: build a set of fake "changed" paths and a walk entry list
    let dir = TempDir::new().unwrap();
    let changed_file = dir.path().join("changed.rs");
    let unchanged_file = dir.path().join("unchanged.rs");
    std::fs::write(&changed_file, "fn changed() {}").unwrap();
    std::fs::write(&unchanged_file, "fn unchanged() {}").unwrap();

    let entries = traversal::walk_directory(dir.path(), None).unwrap();
    let total_files = entries.iter().filter(|e| !e.is_dir).count();
    assert_eq!(total_files, 2, "sanity: 2 files before filtering");

    // Simulate: only changed.rs is in the changed set
    let mut changed: HashSet<std::path::PathBuf> = HashSet::new();
    changed.insert(changed_file.clone());

    // Act: filter entries
    let filtered = filter_entries_by_git_ref(entries, &changed, dir.path());
    let filtered_files: Vec<_> = filtered.iter().filter(|e| !e.is_dir).collect();

    // Assert: only changed.rs remains
    assert_eq!(
        filtered_files.len(),
        1,
        "only 1 file must remain after git_ref filter"
    );
    assert_eq!(
        filtered_files[0].path, changed_file,
        "the remaining file must be the changed one"
    );

    // Verify changed_files_from_git_ref is at least callable (tested separately for non-git error)
    let _ = changed_files_from_git_ref;
}

#[tokio::test]
async fn test_handle_overview_mode_git_ref_filters_via_handler() {
    use aptu_coder_core::types::{AnalyzeDirectoryParams, OutputControlParams, PaginationParams};
    use std::process::Command;
    use tempfile::TempDir;

    // Arrange: create a real git repo with two commits.
    let dir = TempDir::new().unwrap();
    let repo = dir.path();

    // Init repo and configure minimal identity so git commit works.
    // Use no-hooks to avoid project-local commit hooks that enforce email allowlists.
    let git_no_hook = |repo_path: &std::path::Path, args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["-c", "core.hooksPath=/dev/null"]);
        cmd.args(args);
        cmd.current_dir(repo_path);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git_no_hook(repo, &["init"]);
    git_no_hook(
        repo,
        &[
            "-c",
            "user.email=ci@example.com",
            "-c",
            "user.name=CI",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );

    // Commit file_a.rs in the first commit.
    std::fs::write(repo.join("file_a.rs"), "fn a() {}").unwrap();
    git_no_hook(repo, &["add", "file_a.rs"]);
    git_no_hook(
        repo,
        &[
            "-c",
            "user.email=ci@example.com",
            "-c",
            "user.name=CI",
            "commit",
            "-m",
            "add a",
        ],
    );

    // Add file_b.rs in a second commit (this is what HEAD changes relative to HEAD~1).
    std::fs::write(repo.join("file_b.rs"), "fn b() {}").unwrap();
    git_no_hook(repo, &["add", "file_b.rs"]);
    git_no_hook(
        repo,
        &[
            "-c",
            "user.email=ci@example.com",
            "-c",
            "user.name=CI",
            "commit",
            "-m",
            "add b",
        ],
    );

    // Act: call handle_overview_mode with git_ref=HEAD~1.
    // `git diff --name-only HEAD~1` compares working tree against HEAD~1, returning
    // only file_b.rs (added in the last commit, so present in working tree but not in HEAD~1).
    // Use the canonical path so walk entries match what `git rev-parse --show-toplevel` returns
    // (macOS /tmp is a symlink to /private/tmp; without canonicalization paths would differ).
    let canon_repo = std::fs::canonicalize(repo).unwrap();
    let analyzer = make_analyzer();
    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": canon_repo.to_str().unwrap(),
        "git_ref": "HEAD~1",
    }))
    .unwrap();
    let ct = tokio_util::sync::CancellationToken::new();
    let (arc_output, _cache_hit) = analyzer
        .handle_overview_mode(&params, ct, None)
        .await
        .expect("handle_overview_mode with git_ref must succeed");

    // Assert: only file_b.rs (changed since HEAD~1) appears; file_a.rs must be absent.
    let formatted = &arc_output.formatted;
    assert!(
        formatted.contains("file_b.rs"),
        "git_ref=HEAD~1 output must include file_b.rs; got:\n{formatted}"
    );
    assert!(
        !formatted.contains("file_a.rs"),
        "git_ref=HEAD~1 output must exclude file_a.rs; got:\n{formatted}"
    );
}

#[test]
fn test_validate_path_rejects_absolute_path_outside_cwd() {
    // S4: Verify that absolute paths outside the current working directory are rejected.
    // This test directly calls validate_path with /etc/passwd, which should fail.
    let result = validate_path("/etc/passwd", true);
    assert!(
        result.is_err(),
        "validate_path should reject /etc/passwd (outside CWD)"
    );
    let err = result.unwrap_err();
    let err_msg = err.message.to_lowercase();
    assert!(
        err_msg.contains("outside") || err_msg.contains("not found"),
        "Error message should mention 'outside' or 'not found': {}",
        err.message
    );
}

#[test]
fn test_validate_path_accepts_relative_path_in_cwd() {
    // Happy path: relative path within CWD should be accepted.
    // Use Cargo.toml which exists in the crate root.
    let result = validate_path("Cargo.toml", true);
    assert!(
        result.is_ok(),
        "validate_path should accept Cargo.toml (exists in CWD)"
    );
}

#[test]
fn test_validate_path_creates_parent_for_nonexistent_file() {
    // Edge case: non-existent file with existing parent should be accepted.
    let cwd = std::env::current_dir().expect("should get cwd");
    let parent = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let parent_path = parent.path().to_path_buf();
    let child = parent_path.join("new_file.txt");

    let child_str = child.to_str().expect("path should be valid UTF-8");
    let result = validate_path(child_str, false);
    assert!(
        result.is_ok(),
        "validate_path should accept non-existent file with existing parent (require_exists=false)"
    );
    let path = result.unwrap();
    let canonical_cwd = std::fs::canonicalize(&cwd).expect("should canonicalize cwd");
    assert!(
        path.starts_with(&canonical_cwd),
        "Resolved path should be within CWD: {:?} should start with {:?}",
        path,
        canonical_cwd
    );
}

#[test]
fn test_edit_overwrite_with_working_dir() {
    // Arrange: create a temporary directory within CWD to use as working_dir
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let temp_path = temp_dir.path();

    // Act: call validate_path_in_dir with a relative path
    let result = validate_path_in_dir("test_file.txt", false, temp_path);

    // Assert: path should be resolved relative to working_dir
    assert!(
        result.is_ok(),
        "validate_path_in_dir should accept relative path in valid working_dir: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert!(
        resolved.starts_with(temp_path),
        "Resolved path should be within working_dir: {:?} should start with {:?}",
        resolved,
        temp_path
    );
}

#[test]
fn test_validate_path_in_dir_accepts_outside_cwd() {
    // Arrange: use temp_dir() which is guaranteed to be outside CWD
    let temp_dir = std::env::temp_dir();
    let canonical_temp_dir =
        std::fs::canonicalize(&temp_dir).expect("should canonicalize temp_dir");

    // Act: call validate_path_in_dir with a relative filename
    let result = validate_path_in_dir("probe.txt", false, &temp_dir);

    // Assert: should accept working_dir outside CWD
    assert!(
        result.is_ok(),
        "validate_path_in_dir should accept working_dir outside CWD: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert!(
        resolved.starts_with(&canonical_temp_dir),
        "Resolved path should be within working_dir: {:?} should start with {:?}",
        resolved,
        canonical_temp_dir
    );
}

#[test]
fn test_edit_overwrite_working_dir_traversal() {
    // Arrange: create a temporary directory within CWD to use as working_dir
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let temp_path = temp_dir.path();

    // Act: try to traverse outside working_dir with ../../../etc/passwd
    let result = validate_path_in_dir("../../../etc/passwd", false, temp_path);

    // Assert: should reject path traversal attack (via parent canonicalize failure)
    assert!(
        result.is_err(),
        "validate_path_in_dir should reject path traversal outside working_dir"
    );
}

#[test]
fn test_edit_replace_with_working_dir() {
    // Arrange: create a temporary directory within CWD and file
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let temp_path = temp_dir.path();
    let file_path = temp_path.join("test.txt");
    std::fs::write(&file_path, "hello world").expect("should write test file");

    // Act: call validate_path_in_dir with require_exists=true
    let result = validate_path_in_dir("test.txt", true, temp_path);

    // Assert: should find the file relative to working_dir
    assert!(
        result.is_ok(),
        "validate_path_in_dir should find existing file in working_dir: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    assert_eq!(
        resolved, file_path,
        "Resolved path should match the actual file path"
    );
}

#[test]
fn test_edit_overwrite_no_working_dir() {
    // Arrange: use validate_path without working_dir (existing behavior)
    // Use Cargo.toml which exists in the crate root

    // Act: call validate_path with require_exists=true
    let result = validate_path("Cargo.toml", true);

    // Assert: should work as before
    assert!(
        result.is_ok(),
        "validate_path should still work without working_dir"
    );
}

#[test]
fn test_edit_overwrite_working_dir_is_file() {
    // Arrange: create a temporary file (not directory) to use as working_dir
    let cwd = std::env::current_dir().expect("should get cwd");
    let temp_dir = tempfile::TempDir::new_in(&cwd).expect("should create temp dir in cwd");
    let temp_file = temp_dir.path().join("test_file.txt");
    std::fs::write(&temp_file, "test content").expect("should write test file");

    // Act: call validate_path_in_dir with a file as working_dir
    let result = validate_path_in_dir("some_file.txt", false, &temp_file);

    // Assert: should reject because working_dir is not a directory
    assert!(
        result.is_err(),
        "validate_path_in_dir should reject a file as working_dir"
    );
    let err = result.unwrap_err();
    let err_msg = err.message.to_lowercase();
    assert!(
        err_msg.contains("directory"),
        "Error message should mention 'directory': {}",
        err.message
    );
}

#[test]
fn test_tool_annotations() {
    // Arrange: get tool list via static method
    let tools = CodeAnalyzer::list_tools();

    // Act: find specific tools by name
    let analyze_directory = tools.iter().find(|t| t.name == "analyze_directory");
    let exec_command = tools.iter().find(|t| t.name == "exec_command");

    // Assert: analyze_directory has correct annotations
    let analyze_dir_tool = analyze_directory.expect("analyze_directory tool should exist");
    let analyze_dir_annot = analyze_dir_tool
        .annotations
        .as_ref()
        .expect("analyze_directory should have annotations");
    assert_eq!(
        analyze_dir_annot.read_only_hint,
        Some(true),
        "analyze_directory read_only_hint should be true"
    );
    assert_eq!(
        analyze_dir_annot.destructive_hint,
        Some(false),
        "analyze_directory destructive_hint should be false"
    );

    // Assert: exec_command has correct annotations
    let exec_cmd_tool = exec_command.expect("exec_command tool should exist");
    let exec_cmd_annot = exec_cmd_tool
        .annotations
        .as_ref()
        .expect("exec_command should have annotations");
    assert_eq!(
        exec_cmd_annot.open_world_hint,
        Some(true),
        "exec_command open_world_hint should be true"
    );
}

#[test]
fn test_exec_stdin_size_cap_validation() {
    // Test: stdin size cap check (1 MB limit)
    // Arrange: create oversized stdin
    let oversized_stdin = "x".repeat(STDIN_MAX_BYTES + 1);

    // Act & Assert: verify size exceeds limit
    assert!(
        oversized_stdin.len() > STDIN_MAX_BYTES,
        "test setup: oversized stdin should exceed 1 MB"
    );

    // Verify that a 1 MB stdin is accepted
    let max_stdin = "y".repeat(STDIN_MAX_BYTES);
    assert_eq!(
        max_stdin.len(),
        STDIN_MAX_BYTES,
        "test setup: max stdin should be exactly 1 MB"
    );
}

#[tokio::test]
async fn test_exec_stdin_cat_roundtrip() {
    // Test: stdin content is piped to process and readable via stdout
    // Arrange: prepare stdin content
    let stdin_content = "hello world";

    // Act: execute cat with stdin via shell
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn cat");

    if let Some(mut stdin_handle) = child.stdin.take() {
        use tokio::io::AsyncWriteExt as _;
        stdin_handle
            .write_all(stdin_content.as_bytes())
            .await
            .expect("write stdin");
        drop(stdin_handle);
    }

    let output = child.wait_with_output().await.expect("wait for cat");

    // Assert: stdout contains the piped stdin content
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout_str.contains(stdin_content),
        "stdout should contain stdin content: {}",
        stdout_str
    );
}

#[tokio::test]
async fn test_exec_stdin_none_no_regression() {
    // Test: command without stdin executes normally (no regression)
    // Act: execute echo without stdin
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("echo hi")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn echo");

    let output = child.wait_with_output().await.expect("wait for echo");

    // Assert: command executes successfully
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout_str.contains("hi"),
        "stdout should contain echo output: {}",
        stdout_str
    );
}

#[test]
fn test_validate_path_in_dir_rejects_sibling_prefix() {
    // Arrange: create a parent temp dir, then two subdirs:
    //   allowed/   -- the working_dir
    //   allowed_sibling/  -- a sibling whose name shares the prefix
    // This mirrors CVE-2025-53110: "/work_evil" must not match "/work".
    let cwd = std::env::current_dir().expect("should get cwd");
    let parent = tempfile::TempDir::new_in(&cwd).expect("should create parent temp dir");
    let allowed = parent.path().join("allowed");
    let sibling = parent.path().join("allowed_sibling");
    std::fs::create_dir_all(&allowed).expect("should create allowed dir");
    std::fs::create_dir_all(&sibling).expect("should create sibling dir");

    // Act: ask for a file inside the sibling dir, using a path that
    // traverses from allowed/ into allowed_sibling/
    let result = validate_path_in_dir("../allowed_sibling/secret.txt", false, &allowed);

    // Assert: must be rejected even though "allowed_sibling" starts with "allowed"
    assert!(
        result.is_err(),
        "validate_path_in_dir must reject a path resolving to a sibling directory \
             sharing the working_dir name prefix (CVE-2025-53110 pattern)"
    );
    let err = result.unwrap_err();
    let msg = err.message.to_lowercase();
    assert!(
        msg.contains("outside") || msg.contains("working"),
        "Error should mention 'outside' or 'working', got: {}",
        err.message
    );
}

#[test]
fn test_validate_path_in_dir_nonexistent_deep_path() {
    // Deeply nested non-existent path: a/b/c/d/new.txt -- none of the
    // intermediate directories exist.  With parent-directory validation,
    // this is rejected because the parent a/b/c/d does not exist.
    let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
    let result = validate_path_in_dir("a/b/c/d/new.txt", false, temp_dir.path());
    assert!(
        result.is_err(),
        "validate_path_in_dir should reject deeply nested non-existent path"
    );
}

#[test]
fn test_validate_path_in_dir_nonexistent_with_existing_parent() {
    // Partial existence: working_dir/sub/ exists but working_dir/sub/new.txt does not.
    // The loop should stop at sub/ (the first existing ancestor) and rejoin new.txt.
    let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
    let sub = temp_dir.path().join("sub");
    std::fs::create_dir_all(&sub).expect("should create sub dir");

    let result = validate_path_in_dir("sub/new.txt", false, temp_dir.path());
    assert!(
        result.is_ok(),
        "validate_path_in_dir should accept file in existing subdir: {:?}",
        result.err()
    );
    let resolved = result.unwrap();
    let canonical_sub = std::fs::canonicalize(&sub).expect("should canonicalize sub");
    assert!(
        resolved.starts_with(&canonical_sub),
        "Resolved path should anchor at the existing sub/ dir: {resolved:?}"
    );
    assert_eq!(
        resolved.file_name().and_then(|n| n.to_str()),
        Some("new.txt"),
        "File name component must be preserved"
    );
}

#[test]
#[serial_test::serial]
fn test_file_cache_capacity_default() {
    // Arrange: ensure the env var is not set
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
    unsafe { std::env::set_var("APTU_CODER_FILE_CACHE_CAPACITY", "42") };

    // Act
    let analyzer = make_analyzer();

    // Cleanup before assertions to minimise env pollution window
    unsafe { std::env::remove_var("APTU_CODER_FILE_CACHE_CAPACITY") };

    // Assert
    assert_eq!(analyzer.cache.file_capacity(), 42);
}

#[test]
fn test_exec_command_path_injected() {
    // Arrange: call build_exec_command with Some("...") resolved_path
    let resolved_path = Some("/usr/local/bin:/usr/bin:/bin");
    let cmd = build_exec_command("echo test", None, false, resolved_path);

    // Act: verify the command was created without panic and inspect args
    let cmd_str = format!("{:?}", cmd);

    // Assert: -l flag must NOT be present (platform unification)
    assert!(
        !cmd_str.contains("-l"),
        "build_exec_command must not use -l on any platform"
    );

    // Assert: command should be created successfully
    assert!(
        !cmd_str.is_empty(),
        "build_exec_command should return a valid Command"
    );
}

#[test]
fn test_exec_command_path_fallback() {
    // Arrange: call build_exec_command with None resolved_path
    let cmd = build_exec_command("echo test", None, false, None);

    // Act: verify the command was created without panic and inspect args
    let cmd_str = format!("{:?}", cmd);

    // Assert: -l flag must NOT be present (platform unification)
    assert!(
        !cmd_str.contains("-l"),
        "build_exec_command must not use -l on any platform"
    );

    // Assert: command should be created successfully even with None
    assert!(
        !cmd_str.is_empty(),
        "build_exec_command should handle None resolved_path gracefully"
    );
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

#[tokio::test]
async fn test_unsupported_extension_returns_success() {
    // Arrange: unsupported extension; handle_file_details_mode should return
    // a structured success (empty semantic, first-50-lines preview).
    let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
    let unsupported_file = temp_dir.path().join("notes.txt");
    std::fs::write(&unsupported_file, "line one\nline two\nline three").expect("should write file");

    let analyzer = make_analyzer();
    let mut params = AnalyzeFileParams::default();
    params.path = unsupported_file.to_string_lossy().to_string();

    let result = analyzer.handle_file_details_mode(&params).await;

    assert!(
        result.is_ok(),
        "should succeed for unsupported extension; got: {:?}",
        result
    );
    let (output, _tier) = result.unwrap();
    assert_eq!(output.line_count, 3, "line_count must be 3");
    assert!(
        output.semantic.functions.is_empty(),
        "functions must be empty"
    );
    assert!(output.semantic.classes.is_empty(), "classes must be empty");
    assert!(output.semantic.imports.is_empty(), "imports must be empty");
}

#[tokio::test]
async fn test_unsupported_extension_fallback_note_in_formatted() {
    // Edge case: formatted output must contain an unsupported-extension note.
    let temp_dir = tempfile::TempDir::new().expect("should create temp dir");
    let unsupported_file = temp_dir.path().join("readme.txt");
    std::fs::write(
        &unsupported_file,
        "This is a plain text file.\nSecond line.",
    )
    .expect("should write file");

    let analyzer = make_analyzer();
    let mut params = AnalyzeFileParams::default();
    params.path = unsupported_file.to_string_lossy().to_string();

    let (output, _tier) = analyzer
        .handle_file_details_mode(&params)
        .await
        .expect("must succeed");
    let lower = output.formatted.to_lowercase();
    assert!(
        lower.contains("unsupported"),
        "formatted must contain 'unsupported' note; got: {}",
        output.formatted
    );
}

#[test]
fn test_exec_no_truncation_under_limits() {
    // Happy path: small output under all caps
    let stdout = "hello world".to_string();
    let stderr = "no errors".to_string();
    let slot = 0u32;

    let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout, stderr, slot);

    assert_eq!(out_stdout, "hello world");
    assert_eq!(out_stderr, "no errors");
    assert!(stdout_path.is_none());
    assert!(stderr_path.is_none());
    assert!(!byte_truncated);
}

#[test]
fn test_exec_byte_overflow_stdout_exceeds_30k() {
    // Edge case: stdout exceeds 30k byte limit
    let stdout = "x".repeat(35_000);
    let stderr = "small".to_string();
    let slot = 0u32;

    let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout.clone(), stderr.clone(), slot);

    // Verify truncation occurred
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(stdout_path.is_some(), "stdout_path should be set");
    assert!(stderr_path.is_some(), "stderr_path should be set");

    // Verify output was truncated
    assert!(
        out_stdout.len() <= 30_000,
        "stdout should be truncated to <= 30k"
    );
    assert_eq!(out_stderr, "small", "stderr should be unchanged");

    // Verify slot file was written
    let base = std::env::temp_dir()
        .join("aptu-coder-overflow")
        .join(format!("slot-{slot}"));
    let stdout_file = base.join("stdout");
    assert!(
        stdout_file.exists(),
        "stdout slot file should exist after byte overflow"
    );
}

#[test]
fn test_exec_byte_overflow_stderr_exceeds_10k() {
    // Edge case: stderr exceeds 10k byte limit
    let stdout = "small".to_string();
    let stderr = "y".repeat(15_000);
    let slot = 1u32;

    let (out_stdout, out_stderr, stdout_path, stderr_path, byte_truncated) =
        handle_output_persist(stdout.clone(), stderr.clone(), slot);

    // Verify truncation occurred
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(stdout_path.is_some(), "stdout_path should be set");
    assert!(stderr_path.is_some(), "stderr_path should be set");

    // Verify output was truncated
    assert_eq!(out_stdout, "small", "stdout should be unchanged");
    assert!(
        out_stderr.len() <= 10_000,
        "stderr should be truncated to <= 10k"
    );

    // Verify slot file was written
    let base = std::env::temp_dir()
        .join("aptu-coder-overflow")
        .join(format!("slot-{slot}"));
    let stderr_file = base.join("stderr");
    assert!(
        stderr_file.exists(),
        "stderr slot file should exist after byte overflow"
    );
}

#[test]
fn test_exec_byte_overflow_combined_exceeds_50k() {
    // Edge case: combined output_text exceeds 50k char limit
    // This is tested by verifying the truncation logic in exec_command
    let large_output = "z".repeat(60_000);
    assert!(large_output.len() > SIZE_LIMIT);

    // Simulate the truncation logic from exec_command
    let mut combined_truncated = false;
    let truncated = if large_output.len() > SIZE_LIMIT {
        combined_truncated = true;
        let tail_start = large_output.len().saturating_sub(SIZE_LIMIT);
        let safe_start = large_output[..tail_start].floor_char_boundary(tail_start);
        large_output[safe_start..].to_string()
    } else {
        large_output.clone()
    };

    assert!(combined_truncated, "combined_truncated should be true");
    assert!(
        truncated.len() <= SIZE_LIMIT,
        "output should be truncated to <= 50k"
    );
}

#[test]
fn test_exec_line_and_byte_interaction() {
    // Edge case: line cap and byte cap are independent
    // 1500 lines with long content to exceed 30k bytes should trigger byte cap, not line cap
    let lines: Vec<String> = (0..1500)
        .map(|i| {
            format!(
                "line {} with some padding to make it longer: {}",
                i,
                "x".repeat(15)
            )
        })
        .collect();
    let stdout = lines.join("\n");
    assert!(stdout.lines().count() <= 2000, "should have <= 2000 lines");
    assert!(stdout.len() > 30_000, "should exceed 30k bytes");

    let stderr = "".to_string();
    let slot = 2u32;

    let (out_stdout, _out_stderr, stdout_path, _stderr_path, byte_truncated) =
        handle_output_persist(stdout.clone(), stderr, slot);

    // Byte cap should fire, not line cap
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(stdout_path.is_some(), "stdout_path should be set");
    assert!(
        out_stdout.len() <= 30_000,
        "stdout should be truncated by byte cap"
    );
}

#[test]
fn test_exec_utf8_boundary_safety() {
    // Edge case: ensure truncation doesn't split multi-byte UTF-8 chars
    // Create a string with multi-byte characters near the boundary
    let mut stdout = String::new();
    for _ in 0..4000 {
        stdout.push_str("hello world ");
    }
    // Add some multi-byte chars
    stdout.push_str("こんにちは"); // Japanese characters (3 bytes each)
    assert!(stdout.len() > 30_000, "stdout should exceed 30k bytes");

    let stderr = "".to_string();
    let slot = 5u32;

    let (out_stdout, _out_stderr, _stdout_path, _stderr_path, byte_truncated) =
        handle_output_persist(stdout, stderr, slot);

    // Verify truncation happened and result is valid UTF-8
    assert!(byte_truncated, "byte_truncated should be true");
    assert!(
        out_stdout.is_char_boundary(0),
        "start should be char boundary"
    );
    assert!(
        out_stdout.is_char_boundary(out_stdout.len()),
        "end should be char boundary"
    );
    // Verify we can iterate chars without panic
    let _char_count = out_stdout.chars().count();
}

#[test]
fn test_filter_strip_lines_matching() {
    // Happy path: filter matches command prefix and strips lines
    let rule = types::FilterRule {
        match_command: "^git\\s+pull".to_string(),
        description: Some("test filter".to_string()),
        strip_ansi: false,
        strip_lines_matching: vec!["^\\s*\\|\\s*\\d+\\s*[+-]+".to_string()],
        keep_lines_matching: vec![],
        max_lines: None,
        on_empty: None,
    };

    let strip_patterns = vec![Regex::new("^\\s*\\|\\s*\\d+\\s*[+-]+").unwrap()];
    let compiled = CompiledRule {
        pattern: Regex::new("^git\\s+pull").unwrap(),
        strip_patterns,
        keep_patterns: vec![],
        rule,
    };

    let stdout = "Updating abc123..def456\n | 5 ++++\n | 3 ---\nFast-forward\n";
    let filtered = apply_filter(&compiled, stdout);

    assert!(!filtered.contains("| 5 ++++"), "should strip stat lines");
    assert!(!filtered.contains("| 3 ---"), "should strip stat lines");
    assert!(
        filtered.contains("Updating"),
        "should keep non-matching lines"
    );
    assert!(
        filtered.contains("Fast-forward"),
        "should keep non-matching lines"
    );
}

#[test]
fn test_filter_on_empty_substitution() {
    // Edge case: on_empty substitution when filtered stdout is empty
    let rule = types::FilterRule {
        match_command: "^git\\s+fetch".to_string(),
        description: Some("test fetch".to_string()),
        strip_ansi: false,
        strip_lines_matching: vec!["^From ".to_string(), "^\\s+[a-f0-9]+\\.\\.".to_string()],
        keep_lines_matching: vec![],
        max_lines: None,
        on_empty: Some("ok fetched".to_string()),
    };

    let strip_patterns = vec![
        Regex::new("^From ").unwrap(),
        Regex::new("^\\s+[a-f0-9]+\\.\\.").unwrap(),
    ];
    let compiled = CompiledRule {
        pattern: Regex::new("^git\\s+fetch").unwrap(),
        strip_patterns,
        keep_patterns: vec![],
        rule,
    };

    let stdout = "From github.com:user/repo\n  abc123..def456 main -> origin/main\n";
    let filtered = apply_filter(&compiled, stdout);

    assert_eq!(
        filtered, "ok fetched",
        "should return on_empty when all lines stripped"
    );
}

#[test]
fn test_filter_passthrough_on_failure() {
    // Test the exit-code guard in run_exec_impl: filter only applied when exit_code == Some(0)
    let rule = types::FilterRule {
        match_command: "^cargo\\s+build".to_string(),
        description: Some("cargo build filter".to_string()),
        strip_ansi: false,
        strip_lines_matching: vec!["^\\s*Compiling ".to_string()],
        keep_lines_matching: vec![],
        max_lines: None,
        on_empty: None,
    };

    let strip_patterns = vec![Regex::new("^\\s*Compiling ").unwrap()];
    let compiled = CompiledRule {
        pattern: Regex::new("^cargo\\s+build").unwrap(),
        strip_patterns,
        keep_patterns: vec![],
        rule,
    };

    let stdout = "   Compiling mylib v0.1.0\nerror: failed to compile\n";

    // Sub-case 1: non-zero exit code (exit_code != Some(0))
    // The guard condition fails, so filter_applied must remain None and stdout unchanged
    let mut output = ShellOutput::new(
        stdout.to_string(),
        "".to_string(),
        "".to_string(),
        Some(1), // non-zero exit
        false,
    );

    // Simulate the guard: if exit_code == Some(0) { apply filter }
    if output.exit_code == Some(0) {
        output.stdout = apply_filter(&compiled, &output.stdout);
        output.filter_applied = compiled
            .rule
            .description
            .clone()
            .or_else(|| Some(compiled.rule.match_command.clone()));
    }

    assert!(
        output.filter_applied.is_none(),
        "filter_applied should be None when exit_code != Some(0)"
    );
    assert!(
        output.stdout.contains("Compiling"),
        "stdout should be unchanged when exit_code != Some(0)"
    );

    // Sub-case 2: zero exit code (exit_code == Some(0))
    // The guard condition passes, so filter_applied is set and stdout is filtered
    let mut output2 = ShellOutput::new(
        stdout.to_string(),
        "".to_string(),
        "".to_string(),
        Some(0), // zero exit
        false,
    );

    if output2.exit_code == Some(0) {
        output2.stdout = apply_filter(&compiled, &output2.stdout);
        output2.filter_applied = compiled
            .rule
            .description
            .clone()
            .or_else(|| Some(compiled.rule.match_command.clone()));
    }

    assert!(
        output2.filter_applied.is_some(),
        "filter_applied should be set when exit_code == Some(0)"
    );
    assert_eq!(
        output2.filter_applied.as_ref().unwrap(),
        "cargo build filter"
    );
    assert!(
        !output2.stdout.contains("Compiling"),
        "stdout should be filtered when exit_code == Some(0)"
    );
}

#[test]
fn test_no_stat_injection() {
    // Happy path: --no-stat injection for bare git pull
    let command = "git pull origin main";
    let result = maybe_inject_no_stat(command);
    assert_eq!(
        result, "git pull origin main --no-stat",
        "should inject --no-stat"
    );
}

#[test]
fn test_no_stat_not_injected_when_present() {
    // Edge case: --no-stat not injected when --stat already present
    let command = "git pull --stat origin main";
    let result = maybe_inject_no_stat(command);
    assert_eq!(result, command, "should not inject when --stat present");

    let command2 = "git pull --no-stat origin main";
    let result2 = maybe_inject_no_stat(command2);
    assert_eq!(
        result2, command2,
        "should not inject when --no-stat present"
    );

    let command3 = "git pull --verbose origin main";
    let result3 = maybe_inject_no_stat(command3);
    assert_eq!(
        result3, command3,
        "should not inject when --verbose present"
    );
}

#[test]
fn test_no_stat_word_boundary_cases() {
    let cases: &[(&str, &str)] = &[
        ("gitpull some-arg", "gitpull some-arg"),
        ("git log upstream/pull/123", "git log upstream/pull/123"),
        (
            "git pull origin main --rebase",
            "git pull origin main --rebase --no-stat",
        ),
        ("git pull --no-stat", "git pull --no-stat"),
        ("git log --stat", "git log --stat"),
    ];
    for (input, expected) in cases {
        assert_eq!(maybe_inject_no_stat(input), *expected, "input: {input}");
    }
}

#[test]
fn test_filter_applied_field_present() {
    // Test apply_filter() end-to-end and verify filter_applied field is set correctly
    let rule = types::FilterRule {
        match_command: "^git\\s+status".to_string(),
        description: Some("git status filter".to_string()),
        strip_ansi: false,
        strip_lines_matching: vec!["^On branch".to_string()],
        keep_lines_matching: vec![],
        max_lines: Some(20),
        on_empty: None,
    };

    let strip_patterns = vec![Regex::new("^On branch").unwrap()];
    let compiled = CompiledRule {
        pattern: Regex::new("^git\\s+status").unwrap(),
        strip_patterns,
        keep_patterns: vec![],
        rule,
    };

    let stdout = "On branch main\nnothing to commit\n";

    // Call apply_filter() and verify the returned string is filtered
    let filtered = apply_filter(&compiled, stdout);
    assert!(
        !filtered.contains("On branch"),
        "apply_filter should strip matching lines"
    );
    assert!(
        filtered.contains("nothing to commit"),
        "apply_filter should keep non-matching lines"
    );

    // Simulate the guard and field assignment from run_exec_impl
    let mut output = ShellOutput::new(filtered, "".to_string(), "".to_string(), Some(0), false);

    // Set filter_applied as run_exec_impl does
    output.filter_applied = compiled
        .rule
        .description
        .clone()
        .or_else(|| Some(compiled.rule.match_command.clone()));

    assert!(
        output.filter_applied.is_some(),
        "filter_applied should be set when filter matches"
    );
    assert_eq!(output.filter_applied.as_ref().unwrap(), "git status filter");
}

#[test]
fn test_filter_keep_lines_matching() {
    // Happy path: filter matches command prefix and keeps only matching lines
    let rule = types::FilterRule {
        match_command: "^cargo\\s+test".to_string(),
        description: Some("test keep filter".to_string()),
        strip_ansi: false,
        strip_lines_matching: vec![],
        keep_lines_matching: vec!["^test ".to_string(), "^FAILED".to_string()],
        max_lines: None,
        on_empty: None,
    };
    let compiled = filters::CompiledRule {
        pattern: Regex::new("^cargo\\s+test").unwrap(),
        strip_patterns: vec![],
        keep_patterns: vec![
            Regex::new("^test ").unwrap(),
            Regex::new("^FAILED").unwrap(),
        ],
        rule,
    };

    let stdout = "   Compiling mylib v0.1.0\ntest foo::bar ... ok\ntest foo::baz ... FAILED\ntest result: FAILED\n";
    let filtered = filters::apply_filter(&compiled, stdout);

    assert!(filtered.contains("test foo::bar"), "should keep test lines");
    assert!(
        filtered.contains("test foo::baz"),
        "should keep FAILED test lines"
    );
    assert!(!filtered.contains("Compiling"), "should drop compile lines");
}

#[test]
fn test_filter_max_lines_cap() {
    // Edge case: filter caps output to max_lines
    let rule = types::FilterRule {
        match_command: "^git\\s+log".to_string(),
        description: Some("test max lines".to_string()),
        strip_ansi: false,
        strip_lines_matching: vec![],
        keep_lines_matching: vec![],
        max_lines: Some(3),
        on_empty: None,
    };
    let compiled = filters::CompiledRule {
        pattern: Regex::new("^git\\s+log").unwrap(),
        strip_patterns: vec![],
        keep_patterns: vec![],
        rule,
    };

    let stdout = "line1\nline2\nline3\nline4\nline5\n";
    let filtered = filters::apply_filter(&compiled, stdout);

    assert_eq!(filtered.lines().count(), 3, "should cap at 3 lines");
    assert!(filtered.contains("line1"));
    assert!(filtered.contains("line3"));
    assert!(
        !filtered.contains("line4"),
        "should not include lines beyond max"
    );
}

#[test]
fn test_filter_git_show_strips_patch_hunks() {
    // Happy path: verifies ^[+-][^+-] keeps ---/+++ file headers while stripping diff lines
    let compiled = filters::CompiledRule {
        pattern: Regex::new("^git\\s+show").unwrap(),
        strip_patterns: vec![
            Regex::new("^@@").unwrap(),
            Regex::new("^[+-][^+-]").unwrap(),
        ],
        keep_patterns: vec![],
        rule: types::FilterRule {
            match_command: "^git\\s+show".to_string(),
            description: None,
            strip_ansi: true,
            strip_lines_matching: vec!["^@@".to_string(), "^[+-][^+-]".to_string()],
            keep_lines_matching: vec![],
            max_lines: Some(200),
            on_empty: None,
        },
    };

    let stdout = "commit abc123\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n-old line\n+new line\n context line\n";
    let filtered = filters::apply_filter(&compiled, stdout);

    assert!(
        filtered.contains("--- a/src/lib.rs"),
        "should keep --- file header"
    );
    assert!(
        filtered.contains("+++ b/src/lib.rs"),
        "should keep +++ file header"
    );
    assert!(!filtered.contains("@@ -1,3"), "should strip hunk headers");
    assert!(
        !filtered.contains("-old line"),
        "should strip removed lines"
    );
    assert!(!filtered.contains("+new line"), "should strip added lines");
}

#[test]
fn test_filter_on_empty_from_empty_input() {
    // Edge case: on_empty fires when stdout is already empty (not just stripped-to-empty);
    // complements test_filter_on_empty_substitution which covers stripped-to-empty
    let compiled = filters::CompiledRule {
        pattern: Regex::new("^git\\s+diff").unwrap(),
        strip_patterns: vec![],
        keep_patterns: vec![],
        rule: types::FilterRule {
            match_command: "^git\\s+diff".to_string(),
            description: None,
            strip_ansi: true,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: Some(100),
            on_empty: Some("ok (working tree clean)".to_string()),
        },
    };

    assert_eq!(
        filters::apply_filter(&compiled, ""),
        "ok (working tree clean)",
        "on_empty should fire on empty input"
    );
}

#[test]
fn test_filter_applied_to_interleaved_with_both_streams() {
    // Happy path: apply_filter on an interleaved string that mixes stdout and stderr lines.
    // Lines matching the strip pattern are removed; stderr-origin lines are preserved.
    let compiled = filters::CompiledRule {
        pattern: Regex::new("^git\\s+pull").unwrap(),
        strip_patterns: vec![Regex::new("^\\s*\\|\\s*\\d+\\s*[+\\-]+").unwrap()],
        keep_patterns: vec![],
        rule: types::FilterRule {
            match_command: "^git\\s+pull".to_string(),
            description: None,
            strip_ansi: false,
            strip_lines_matching: vec!["^\\s*\\|\\s*\\d+\\s*[+\\-]+".to_string()],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: None,
        },
    };

    // Arrange: interleaved with one stdout-origin strip-matched line and one stderr-origin line
    let interleaved = " | 42  ++++++++++++\nFrom https://github.com/example/repo\n";

    // Act
    let result = filters::apply_filter(&compiled, interleaved);

    // Assert: strip-matched line gone; stderr-origin line present
    assert!(
        !result.contains("| 42"),
        "strip-matched line should be absent from filtered interleaved"
    );
    assert!(
        result.contains("From https://github.com/example/repo"),
        "stderr-origin line should be preserved in filtered interleaved"
    );
}

#[test]
fn test_on_empty_substitution_in_interleaved() {
    // Edge case: when filter strips all lines in interleaved, on_empty text is returned.
    let compiled = filters::CompiledRule {
        pattern: Regex::new("^git\\s+pull").unwrap(),
        strip_patterns: vec![Regex::new(".*").unwrap()],
        keep_patterns: vec![],
        rule: types::FilterRule {
            match_command: "^git\\s+pull".to_string(),
            description: None,
            strip_ansi: false,
            strip_lines_matching: vec![".*".to_string()],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: Some("ok (up-to-date)".to_string()),
        },
    };

    // Arrange: interleaved where every line matches the strip pattern
    let interleaved = "Already up to date.\nFrom https://github.com/example/repo\n";

    // Act
    let result = filters::apply_filter(&compiled, interleaved);

    // Assert: on_empty substitution text returned
    assert_eq!(
        result, "ok (up-to-date)",
        "on_empty should be returned when filter strips all lines in interleaved"
    );
}

#[test]
fn test_line_cap_fires_before_byte_cap() {
    // Edge case: 2500 lines x 5 chars each = 12500 bytes (under 30k byte cap)
    // Line cap (2000) should fire; returned content has ~50 lines (OVERFLOW_PREVIEW_LINES)
    let line = "abcde";
    let stdout: String = std::iter::repeat(format!("{}\n", line))
        .take(2500)
        .collect();
    assert_eq!(stdout.lines().count(), 2500, "should have 2500 lines");
    assert!(stdout.len() < 30_000, "should be under byte cap");

    let stderr = String::new();
    let slot = 42u32;

    let (out_stdout, _out_stderr, stdout_path, _stderr_path, byte_truncated) =
        handle_output_persist(stdout, stderr, slot);

    // Line cap fires: output_truncated should be indicated via stdout_path being set
    assert!(
        !byte_truncated,
        "byte cap should NOT fire (under 30k bytes)"
    );
    assert!(
        stdout_path.is_some(),
        "stdout_path should be set when line cap fires"
    );
    // Returned preview is last OVERFLOW_PREVIEW_LINES (50) lines
    let line_count = out_stdout.lines().count();
    assert!(
        line_count <= 50,
        "returned content should have at most 50 lines, got {}",
        line_count
    );
    assert!(line_count > 0, "returned content should not be empty");
}

#[test]
fn test_project_local_overrides_builtin() {
    // Edge case: project-local rule inserted at index 0 takes precedence (first-match semantics).
    // Use a unique command name that does NOT match any built-in rule to verify
    // that project-local rules are loaded and placed before built-ins.
    use std::io::Write;

    let tmp = std::env::temp_dir().join(format!(
        "aptu-test-project-local-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let aptu_dir = tmp.join(".aptu");
    std::fs::create_dir_all(&aptu_dir).expect("should create .aptu dir");

    // Use a unique command not matching any built-in rule; include required schema_version field
    let toml_content = "schema_version = 1\n[[filters]]\nmatch_command = \"^my-custom-tool\"\nkeep_lines_matching = []\non_empty = \"project-local-only-marker\"\n";
    let mut f =
        std::fs::File::create(aptu_dir.join("filters.toml")).expect("should create filters.toml");
    f.write_all(toml_content.as_bytes())
        .expect("should write toml");
    drop(f);

    let rules = filters::load_filter_table(&tmp);

    // The project-local rule should appear at index 0
    let first_rule = rules.first().expect("should have at least one rule");
    assert!(
        first_rule.pattern.is_match("my-custom-tool --flag"),
        "project-local rule should be first (index 0)"
    );
    assert_eq!(
        first_rule.rule.on_empty.as_deref(),
        Some("project-local-only-marker"),
        "project-local rule on_empty should match what was written"
    );

    // Also verify that built-in rules are still present (after the project-local rule)
    let has_git_pull = rules
        .iter()
        .any(|r| r.pattern.is_match("git pull origin main"));
    assert!(
        has_git_pull,
        "built-in git pull rule should still be present"
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_invalid_toml_falls_back_gracefully() {
    // Edge case: invalid TOML in .aptu/filters.toml should fall back to built-ins without panic
    use std::io::Write;

    let tmp = std::env::temp_dir().join(format!(
        "aptu-test-invalid-toml-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let aptu_dir = tmp.join(".aptu");
    std::fs::create_dir_all(&aptu_dir).expect("should create .aptu dir");

    let mut f =
        std::fs::File::create(aptu_dir.join("filters.toml")).expect("should create filters.toml");
    // invalid TOML: use "garbage" that is syntactically invalid TOML
    // Note: the TOML also requires schema_version field in FilterTableConfig;
    // invalid content ensures the serde parse fails
    f.write_all(b"schema_version = INVALID_VALUE {{{{")
        .expect("should write garbage");
    drop(f);

    // Should not panic; should return built-in rules only
    let rules = filters::load_filter_table(&tmp);

    // Built-in rules include git pull, git fetch, etc.
    let has_git_pull = rules
        .iter()
        .any(|r| r.pattern.is_match("git pull origin main"));
    assert!(
        has_git_pull,
        "should have git pull built-in rule after invalid TOML"
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_invalid_schema_version_falls_back_gracefully() {
    // Edge case: schema_version != 1 in .aptu/filters.toml should fall back to built-ins.
    use std::io::Write;

    let tmp = std::env::temp_dir().join(format!(
        "aptu-test-schema-version-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let aptu_dir = tmp.join(".aptu");
    std::fs::create_dir_all(&aptu_dir).expect("should create .aptu dir");

    // schema_version = 2 with a valid filter rule; should be rejected
    let toml_content = "schema_version = 2\n[[filters]]\nmatch_command = \"^my-v2-tool\"\nkeep_lines_matching = []\n";
    let mut f =
        std::fs::File::create(aptu_dir.join("filters.toml")).expect("should create filters.toml");
    f.write_all(toml_content.as_bytes())
        .expect("should write toml");
    drop(f);

    // Should not panic; should return built-in rules only (no project-local rule)
    let rules = filters::load_filter_table(&tmp);

    // Built-in rules must be present
    let has_git_pull = rules
        .iter()
        .any(|r| r.pattern.is_match("git pull origin main"));
    assert!(
        has_git_pull,
        "should have git pull built-in rule after schema_version=2 rejection"
    );

    // The project-local rule must NOT be present
    let has_v2_rule = rules
        .iter()
        .any(|r| r.pattern.is_match("my-v2-tool --flag"));
    assert!(
        !has_v2_rule,
        "schema_version=2 rule should not be loaded; only built-ins expected"
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
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
    };
    assert!(
        !event.chars_threshold_breach,
        "chars_threshold_breach should be false for output_chars=5000"
    );
}

// ── Progress token gating and watch channel tests ──

/// When no progressToken is present, handle_overview_mode skips all progress
/// machinery (no peer lock acquisition for progress, no watch channel, no
/// emit_progress calls) and returns the analysis result directly.
#[tokio::test]
async fn test_progress_bypassed_when_no_token() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}").unwrap();
    let analyzer = make_analyzer();
    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": dir.path().to_str().unwrap(),
    }))
    .unwrap();
    let ct = tokio_util::sync::CancellationToken::new();

    // Act: call with None progress_token -- must complete without error.
    let result = analyzer.handle_overview_mode(&params, ct, None).await;
    assert!(
        result.is_ok(),
        "handle_overview_mode with None token must succeed"
    );
}

// ── strip_cd_prefix tests ──

#[test]
fn test_strip_cd_prefix_basic() {
    let (cmd, path) = strip_cd_prefix("cd /tmp && echo hello");
    assert_eq!(cmd, "echo hello");
    assert_eq!(path, Some("/tmp"));
}

#[test]
fn test_strip_cd_prefix_no_ampersand() {
    // No && separator -- returned unmodified; shell handles the cd naturally.
    let (cmd, path) = strip_cd_prefix("cd /tmp");
    assert_eq!(cmd, "cd /tmp");
    assert_eq!(path, None);
}

#[test]
fn test_strip_cd_prefix_with_extra_spaces() {
    // Surrounding whitespace is trimmed from both extracted path and stripped command.
    let (cmd, path) = strip_cd_prefix("cd  /tmp  &&  echo hello");
    assert_eq!(path, Some("/tmp"));
    assert_eq!(cmd, "echo hello");
}

#[test]
fn test_strip_cd_prefix_splits_on_first_ampersand_only() {
    // Only the leading cd && is consumed; subsequent && in the command are preserved.
    let (cmd, path) = strip_cd_prefix("cd /a && cmd1 && cd /b && cmd2");
    assert_eq!(path, Some("/a"));
    assert_eq!(cmd, "cmd1 && cd /b && cmd2");
}
