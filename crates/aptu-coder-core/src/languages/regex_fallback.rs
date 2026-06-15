// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Regex-based semantic extraction for formats without tree-sitter grammars.
//!
//! Covers CSS, YAML, JSON, TOML, and Astro. Each function returns a
//! [`SemanticAnalysis`] with `functions` populated as a best-effort symbol
//! list (selectors, keys, section headers, or frontmatter exports). All
//! errors are handled internally; callers always receive a valid (possibly
//! empty) result.

use crate::parser::SemanticExtractor;
use crate::types::{FunctionInfo, SemanticAnalysis};
use regex::Regex;
use std::sync::LazyLock;

// --- compiled patterns (compiled once at startup) ---

static CSS_SELECTOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[.#][\w-]+[\s,:{]").expect("valid CSS selector pattern"));

static YAML_TOP_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\w[\w-]*): ").expect("valid YAML top-level key pattern"));

static JSON_FIRST_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s{0,2}"(\w+)":"#).expect("valid JSON first-level key pattern")
});

static TOML_SECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[([^\]]+)\]").expect("valid TOML section header pattern"));

// --- extraction functions ---

/// Extract CSS class/ID selectors as function entries.
pub fn extract_css(source: &str) -> SemanticAnalysis {
    let mut functions = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if CSS_SELECTOR.is_match(trimmed) {
            let name = trimmed
                .trim_end_matches(|c: char| c == '{' || c == ',' || c == ':' || c.is_whitespace())
                .to_string();
            if !name.is_empty() {
                let line_no = idx + 1;
                functions.push(FunctionInfo {
                    name,
                    line: line_no,
                    end_line: line_no,
                    parameters: Vec::new(),
                    return_type: None,
                });
            }
        }
    }
    SemanticAnalysis {
        functions,
        ..Default::default()
    }
}

/// Extract YAML top-level keys as function entries.
pub fn extract_yaml(source: &str) -> SemanticAnalysis {
    let mut functions = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        if let Some(caps) = YAML_TOP_KEY.captures(line) {
            let name = caps[1].to_string();
            let line_no = idx + 1;
            functions.push(FunctionInfo {
                name,
                line: line_no,
                end_line: line_no,
                parameters: Vec::new(),
                return_type: None,
            });
        }
    }
    SemanticAnalysis {
        functions,
        ..Default::default()
    }
}

/// Extract JSON first-level string keys as function entries.
pub fn extract_json(source: &str) -> SemanticAnalysis {
    let mut functions = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        if let Some(caps) = JSON_FIRST_KEY.captures(line) {
            let name = caps[1].to_string();
            let line_no = idx + 1;
            functions.push(FunctionInfo {
                name,
                line: line_no,
                end_line: line_no,
                parameters: Vec::new(),
                return_type: None,
            });
        }
    }
    SemanticAnalysis {
        functions,
        ..Default::default()
    }
}

/// Extract TOML section headers as function entries.
pub fn extract_toml(source: &str) -> SemanticAnalysis {
    let mut functions = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        if let Some(caps) = TOML_SECTION.captures(line) {
            let name = caps[1].to_string();
            let line_no = idx + 1;
            functions.push(FunctionInfo {
                name,
                line: line_no,
                end_line: line_no,
                parameters: Vec::new(),
                return_type: None,
            });
        }
    }
    SemanticAnalysis {
        functions,
        ..Default::default()
    }
}

/// Extract Astro frontmatter imports/exports via the TypeScript extractor.
///
/// Splits on lines starting with `---`, extracts the block between the first
/// and second delimiter, then delegates to [`SemanticExtractor::extract`] with
/// `language = "typescript"`. Returns [`Default::default`] when no frontmatter
/// is found or extraction fails.
pub fn extract_astro(source: &str) -> SemanticAnalysis {
    let block = extract_frontmatter(source);
    let Some(frontmatter) = block else {
        return SemanticAnalysis::default();
    };
    SemanticExtractor::extract(&frontmatter, "typescript", None, None).unwrap_or_default()
}

fn extract_frontmatter(source: &str) -> Option<String> {
    let mut delimiters = source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.starts_with("---"));
    let (first, _) = delimiters.next()?;
    let (second, _) = delimiters.next()?;
    let block: Vec<&str> = source
        .lines()
        .skip(first + 1)
        .take(second - first - 1)
        .collect();
    Some(block.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regex_fallback_css_basic() {
        // Arrange
        let source = ".container {\n  color: red;\n}\n#header {\n  font-size: 16px;\n}\n";
        // Act
        let result = extract_css(source);
        // Assert
        let names: Vec<&str> = result.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&".container"),
            "expected .container in {names:?}"
        );
        assert!(names.contains(&"#header"), "expected #header in {names:?}");
    }

    #[test]
    fn test_regex_fallback_yaml_basic() {
        // Arrange
        let source = "name: my-project\nversion: 1.0\n  nested: value\n";
        // Act
        let result = extract_yaml(source);
        // Assert
        let names: Vec<&str> = result.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"name"), "expected name in {names:?}");
        assert!(names.contains(&"version"), "expected version in {names:?}");
        // nested key has leading spaces so must NOT appear
        assert!(
            !names.contains(&"nested"),
            "nested must not appear in {names:?}"
        );
    }

    #[test]
    fn test_regex_fallback_json_basic() {
        // Arrange
        let source = "{\n  \"name\": \"project\",\n  \"version\": \"1.0\"\n}\n";
        // Act
        let result = extract_json(source);
        // Assert
        let names: Vec<&str> = result.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"name"), "expected name in {names:?}");
        assert!(names.contains(&"version"), "expected version in {names:?}");
    }

    #[test]
    fn test_regex_fallback_toml_basic() {
        // Arrange
        let source = "[package]\nname = \"my-crate\"\n\n[dependencies]\nregex = \"1\"\n";
        // Act
        let result = extract_toml(source);
        // Assert
        let names: Vec<&str> = result.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"package"), "expected package in {names:?}");
        assert!(
            names.contains(&"dependencies"),
            "expected dependencies in {names:?}"
        );
    }

    #[cfg(feature = "lang-typescript")]
    #[test]
    fn test_regex_fallback_astro_basic() {
        // Arrange: Astro file with TypeScript frontmatter
        let source =
            "---\nimport Foo from './Foo.astro';\nconst title = 'Hello';\n---\n<h1>{title}</h1>\n";
        // Act
        let result = extract_astro(source);
        // Assert: TypeScript extractor should find the import
        assert!(
            !result.imports.is_empty() || !result.functions.is_empty(),
            "expected imports or functions from frontmatter; got empty result"
        );
    }

    #[test]
    fn test_regex_fallback_astro_no_frontmatter() {
        // Arrange: Astro file without --- delimiters
        let source = "<h1>Hello World</h1>\n<p>No frontmatter here.</p>\n";
        // Act
        let result = extract_astro(source);
        // Assert: returns empty without panic
        assert!(result.functions.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_regex_fallback_empty_file() {
        // Arrange: empty source for each format
        assert!(extract_css("").functions.is_empty());
        assert!(extract_yaml("").functions.is_empty());
        assert!(extract_json("").functions.is_empty());
        assert!(extract_toml("").functions.is_empty());
        assert!(extract_astro("").functions.is_empty());
    }
}
