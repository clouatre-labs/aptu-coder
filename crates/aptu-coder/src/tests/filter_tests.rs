use regex::Regex;

use crate::ShellOutput;
use crate::filters;
use crate::filters::{CompiledRule, apply_filter, maybe_inject_no_stat};
use crate::tools::exec_command::handle_output_persist;
use aptu_coder_core::types;

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
