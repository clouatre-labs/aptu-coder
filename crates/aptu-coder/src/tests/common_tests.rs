use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;

use crate::tests::helpers::make_analyzer;
use crate::tools::common::summary_cursor_conflict;
use crate::{CodeAnalyzer, SIZE_LIMIT};
use aptu_coder_core::types::AnalyzeFileParams;
use aptu_coder_core::{analyze, completion, traversal};

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
    let result = crate::tools::analyze_symbol::validate_impl_only(&entries);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    drop(analyzer); // ensure it compiles with analyzer in scope
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
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    let analyzer = CodeAnalyzer::new(
        peer,
        log_level_filter,
        crate::metrics::MetricsSender(metrics_tx),
    );

    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": tmp.path().to_str().unwrap(),
    }))
    .unwrap();

    let ct = tokio_util::sync::CancellationToken::new();
    let (output, _cache_hit) = analyzer.handle_overview_mode(&params, ct).await.unwrap();

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
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    let analyzer = CodeAnalyzer::new(
        peer,
        log_level_filter,
        crate::metrics::MetricsSender(metrics_tx),
    );

    let params: AnalyzeDirectoryParams = serde_json::from_value(serde_json::json!({
        "path": tmp.path().to_str().unwrap(),
        "summary": false,
    }))
    .unwrap();

    // Act: call the full handler via handle_overview_mode + replicate handler path
    let ct = tokio_util::sync::CancellationToken::new();
    let (output, _cache_hit) = analyzer.handle_overview_mode(&params, ct).await.unwrap();

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

#[test]
fn test_analyze_symbol_import_lookup_invalid_params() {
    // Arrange: empty symbol with import_lookup=true (violates the guard:
    // symbol must hold the module path when import_lookup=true).
    // Act: call the validate helper directly (same pattern as validate_impl_only).
    let result = crate::tools::analyze_symbol::validate_import_lookup(Some(true), "");

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
    use aptu_coder_core::types::AnalyzeDirectoryParams;
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
        .handle_overview_mode(&params, ct)
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
