// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Filter rules for exec_command output post-processing.

use aptu_coder_core::types;
use regex::Regex;
use std::fs;
use std::path::Path;
use tracing::warn;

/// TOML configuration for filter rules.
#[derive(serde::Deserialize)]
pub(crate) struct FilterTableConfig {
    #[allow(dead_code)]
    schema_version: u32,
    filters: Vec<types::FilterRule>,
}

/// Compiled filter rule with pre-compiled regex patterns.
pub(crate) struct CompiledRule {
    pub(crate) pattern: Regex,
    pub(crate) strip_patterns: Vec<Regex>,
    pub(crate) keep_patterns: Vec<Regex>,
    pub(crate) rule: types::FilterRule,
}

/// Build the set of built-in filter rules for common git and cargo commands.
pub(crate) fn build_builtin_filter_rules() -> Vec<types::FilterRule> {
    vec![
        // git pull: strip diff-stat noise (data-confirmed: 96k-108k char cluster)
        types::FilterRule {
            match_command: "^git\\s+pull".to_string(),
            description: Some(
                "git pull: strip diff-stat noise (data-confirmed: 96k-108k char cluster)"
                    .to_string(),
            ),
            strip_ansi: false,
            strip_lines_matching: vec![
                "^\\s*\\|\\s*\\d+\\s*[+-]+".to_string(),
                "^\\s+create mode".to_string(),
                "^\\s+delete mode".to_string(),
                "^\\s+rename ".to_string(),
                "^\\s+mode change".to_string(),
            ],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: Some("ok (up-to-date)".to_string()),
        },
        // git fetch: emit compact ref summary (data-confirmed)
        types::FilterRule {
            match_command: "^git\\s+fetch".to_string(),
            description: Some("git fetch: emit compact ref summary (data-confirmed)".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec!["^From ".to_string(), "^\\s+[a-f0-9]+\\.\\.".to_string()],
            keep_lines_matching: vec![],
            max_lines: Some(10),
            on_empty: Some("ok fetched".to_string()),
        },
        // git push: strip verbose remote lines (data-confirmed)
        types::FilterRule {
            match_command: "^git\\s+push".to_string(),
            description: Some("git push: strip verbose remote lines (data-confirmed)".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec![
                "^remote:\\s+$".to_string(),
                "^remote: Resolving".to_string(),
                "^remote: Compressing".to_string(),
                "^remote: Counting".to_string(),
                "^To ".to_string(),
            ],
            keep_lines_matching: vec![],
            max_lines: Some(10),
            on_empty: Some("ok pushed".to_string()),
        },
        // git log: cap at 20 commit entries (data-confirmed: 957k char cluster)
        types::FilterRule {
            match_command: "^git\\s+log".to_string(),
            description: Some(
                "git log: cap at 20 commit entries (data-confirmed: 957k char cluster)".to_string(),
            ),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: Some(20),
            on_empty: None,
        },
        // git status: cap at 20 lines (data-confirmed)
        types::FilterRule {
            match_command: "^git\\s+status".to_string(),
            description: Some("git status: cap at 20 lines (data-confirmed)".to_string()),
            strip_ansi: false,
            strip_lines_matching: vec![],
            keep_lines_matching: vec![],
            max_lines: Some(20),
            on_empty: None,
        },
        // cargo build: strip compile noise, keep errors/warnings (preventive)
        types::FilterRule {
            match_command: "^cargo\\s+build".to_string(),
            description: Some(
                "cargo build: strip compile noise, keep errors/warnings (preventive)".to_string(),
            ),
            strip_ansi: false,
            strip_lines_matching: vec![
                "^\\s*Compiling ".to_string(),
                "^\\s*Checking ".to_string(),
                "^\\s*Downloading ".to_string(),
                "^\\s*Updating ".to_string(),
                "^\\s*Fresh ".to_string(),
            ],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: Some("ok (build clean)".to_string()),
        },
        // cargo test: keep test results and errors, strip compile noise (preventive)
        types::FilterRule {
            match_command: "^cargo\\s+test".to_string(),
            description: Some(
                "cargo test: keep test results and errors, strip compile noise (preventive)"
                    .to_string(),
            ),
            strip_ansi: false,
            strip_lines_matching: vec![
                "^\\s*Compiling ".to_string(),
                "^\\s*Checking ".to_string(),
                "^\\s*Fresh ".to_string(),
            ],
            keep_lines_matching: vec![],
            max_lines: None,
            on_empty: None,
        },
    ]
}

/// Load filter rules: built-in + project-local from .aptu/filters.toml.
/// On TOML parse error, logs warning and returns built-in rules only.
pub(crate) fn load_filter_table(cwd: &Path) -> Vec<CompiledRule> {
    let mut compiled_rules = Vec::new();

    // Start with built-in rules
    let builtin_rules = build_builtin_filter_rules();
    for rule in builtin_rules {
        if let Ok(pattern) = Regex::new(&rule.match_command) {
            let strip_patterns = rule
                .strip_lines_matching
                .iter()
                .filter_map(|p| Regex::new(p).ok())
                .collect();
            let keep_patterns = rule
                .keep_lines_matching
                .iter()
                .filter_map(|p| Regex::new(p).ok())
                .collect();
            compiled_rules.push(CompiledRule {
                pattern,
                strip_patterns,
                keep_patterns,
                rule,
            });
        }
    }

    // Try to load project-local rules from .aptu/filters.toml
    let filters_path = cwd.join(".aptu").join("filters.toml");
    if let Ok(content) = fs::read_to_string(&filters_path) {
        match toml::from_str::<FilterTableConfig>(&content) {
            Ok(config) => {
                for rule in config.filters {
                    if let Ok(pattern) = Regex::new(&rule.match_command) {
                        let strip_patterns = rule
                            .strip_lines_matching
                            .iter()
                            .filter_map(|p| Regex::new(p).ok())
                            .collect();
                        let keep_patterns = rule
                            .keep_lines_matching
                            .iter()
                            .filter_map(|p| Regex::new(p).ok())
                            .collect();
                        compiled_rules.insert(
                            0,
                            CompiledRule {
                                pattern,
                                strip_patterns,
                                keep_patterns,
                                rule,
                            },
                        );
                    } else {
                        warn!(
                            "failed to compile regex pattern in .aptu/filters.toml: {}",
                            rule.match_command
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "aptu/filters.toml parse error: {}; using built-in table only",
                    e
                );
            }
        }
    }

    compiled_rules
}

/// Inject --no-stat flag for git pull if not already present.
pub(crate) fn maybe_inject_no_stat(command: &str) -> String {
    if command.starts_with("git")
        && command.contains("pull")
        && !command.contains("--stat")
        && !command.contains("--no-stat")
        && !command.contains("--verbose")
    {
        return format!("{} --no-stat", command);
    }
    command.to_string()
}

/// Apply filter rule to stdout: strip/keep/cap lines, substitute on_empty if needed.
pub(crate) fn apply_filter(compiled_rule: &CompiledRule, stdout: &str) -> String {
    let mut lines: Vec<&str> = stdout.lines().collect();

    // Strip lines matching any strip pattern
    if !compiled_rule.strip_patterns.is_empty() {
        lines.retain(|line| {
            !compiled_rule
                .strip_patterns
                .iter()
                .any(|p| p.is_match(line))
        });
    }

    // Keep only lines matching any keep pattern (if keep_patterns is non-empty)
    if !compiled_rule.keep_patterns.is_empty() {
        lines.retain(|line| compiled_rule.keep_patterns.iter().any(|p| p.is_match(line)));
    }

    // Cap to max_lines
    if let Some(max) = compiled_rule.rule.max_lines {
        lines.truncate(max);
    }

    // If result is empty and on_empty is set, return on_empty
    if lines.is_empty()
        && let Some(on_empty) = &compiled_rule.rule.on_empty
    {
        return on_empty.clone();
    }

    lines.join("\n")
}
