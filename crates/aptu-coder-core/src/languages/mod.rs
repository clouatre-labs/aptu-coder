// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Language-specific handlers and query definitions for tree-sitter parsing.
//!
//! Provides query strings and extraction handlers for supported languages.
//! Language support is controlled by Cargo `lang-*` features (by default all
//! available language handlers are enabled): Rust, Go, Java, JavaScript, Python,
//! TypeScript, TSX, Fortran, C/C++, and C#.

#[cfg(feature = "lang-cpp")]
pub mod cpp;
#[cfg(feature = "lang-csharp")]
pub mod csharp;
#[cfg(feature = "lang-fortran")]
pub mod fortran;
#[cfg(feature = "lang-go")]
pub mod go;
#[cfg(feature = "lang-html")]
pub mod html;
#[cfg(feature = "lang-java")]
pub mod java;
#[cfg(feature = "lang-javascript")]
pub mod javascript;
#[cfg(feature = "lang-kotlin")]
pub mod kotlin;
#[cfg(feature = "lang-markdown")]
pub mod markdown;
#[cfg(feature = "lang-python")]
pub mod python;
#[cfg(feature = "lang-rust")]
pub mod rust;
#[cfg(any(feature = "lang-typescript", feature = "lang-tsx"))]
pub mod typescript;

use tree_sitter::{Language, Node};

/// Extract the source text for a node with a bounds check.
///
/// Returns `None` if the node's byte range falls outside `source`.
#[must_use]
pub fn get_node_text(node: &Node, source: &str) -> Option<String> {
    let end = node.end_byte();
    if end <= source.len() {
        Some(source[node.start_byte()..end].to_string())
    } else {
        None
    }
}

/// Handler to extract function name from a node.
pub type ExtractFunctionNameHandler = fn(&Node, &str, &str) -> Option<String>;

/// Handler to find method name for a receiver type.
pub type FindMethodForReceiverHandler = fn(&Node, &str, Option<usize>) -> Option<String>;

/// Handler to find receiver type for a method.
pub type FindReceiverTypeHandler = fn(&Node, &str) -> Option<String>;

/// Handler to extract inheritance information from a class node.
pub type ExtractInheritanceHandler = fn(&Node, &str) -> Vec<String>;

/// Information about a supported language for code analysis.
pub struct LanguageInfo {
    pub name: &'static str,
    pub language: Language,
    pub element_query: &'static str,
    pub call_query: &'static str,
    pub reference_query: Option<&'static str>,
    pub import_query: Option<&'static str>,
    pub impl_query: Option<&'static str>,
    pub impl_trait_query: Option<&'static str>,
    pub defuse_query: Option<&'static str>,
    pub extract_function_name: Option<ExtractFunctionNameHandler>,
    pub find_method_for_receiver: Option<FindMethodForReceiverHandler>,
    pub find_receiver_type: Option<FindReceiverTypeHandler>,
    pub extract_inheritance: Option<ExtractInheritanceHandler>,
}

/// Get language information by language name.
#[allow(clippy::too_many_lines)] // exhaustive match over all supported languages; splitting harms readability
pub fn get_language_info(lang_name: &str) -> Option<LanguageInfo> {
    match lang_name {
        #[cfg(feature = "lang-rust")]
        "rust" => Some(LanguageInfo {
            name: "rust",
            language: tree_sitter_rust::LANGUAGE.into(),
            element_query: rust::ELEMENT_QUERY,
            call_query: rust::CALL_QUERY,
            reference_query: Some(rust::REFERENCE_QUERY),
            import_query: Some(rust::IMPORT_QUERY),
            impl_query: Some(rust::IMPL_QUERY),
            impl_trait_query: Some(rust::IMPL_TRAIT_QUERY),
            defuse_query: Some(rust::DEFUSE_QUERY),
            extract_function_name: Some(rust::extract_function_name),
            find_method_for_receiver: Some(rust::find_method_for_receiver),
            find_receiver_type: Some(rust::find_receiver_type),
            extract_inheritance: Some(rust::extract_inheritance),
        }),
        #[cfg(feature = "lang-python")]
        "python" => Some(LanguageInfo {
            name: "python",
            language: tree_sitter_python::LANGUAGE.into(),
            element_query: python::ELEMENT_QUERY,
            call_query: python::CALL_QUERY,
            reference_query: Some(python::REFERENCE_QUERY),
            import_query: Some(python::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(python::DEFUSE_QUERY),
            extract_function_name: None,
            find_method_for_receiver: None,
            find_receiver_type: None,
            extract_inheritance: Some(python::extract_inheritance),
        }),
        #[cfg(feature = "lang-typescript")]
        "typescript" => Some(LanguageInfo {
            name: "typescript",
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            element_query: typescript::ELEMENT_QUERY,
            call_query: typescript::CALL_QUERY,
            reference_query: Some(typescript::REFERENCE_QUERY),
            import_query: Some(typescript::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(typescript::DEFUSE_QUERY),
            extract_function_name: None,
            find_method_for_receiver: None,
            find_receiver_type: None,
            extract_inheritance: Some(typescript::extract_inheritance),
        }),
        #[cfg(feature = "lang-tsx")]
        "tsx" => Some(LanguageInfo {
            name: "tsx",
            language: tree_sitter_typescript::LANGUAGE_TSX.into(),
            element_query: typescript::ELEMENT_QUERY,
            call_query: typescript::CALL_QUERY,
            reference_query: Some(typescript::REFERENCE_QUERY),
            import_query: Some(typescript::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(typescript::DEFUSE_QUERY),
            extract_function_name: None,
            find_method_for_receiver: None,
            find_receiver_type: None,
            extract_inheritance: Some(typescript::extract_inheritance),
        }),
        #[cfg(feature = "lang-go")]
        "go" => Some(LanguageInfo {
            name: "go",
            language: tree_sitter_go::LANGUAGE.into(),
            element_query: go::ELEMENT_QUERY,
            call_query: go::CALL_QUERY,
            reference_query: Some(go::REFERENCE_QUERY),
            import_query: Some(go::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(go::DEFUSE_QUERY),
            extract_function_name: Some(go::extract_function_name),
            find_method_for_receiver: Some(go::find_method_for_receiver),
            find_receiver_type: Some(go::find_receiver_type),
            extract_inheritance: Some(go::extract_inheritance),
        }),
        #[cfg(feature = "lang-cpp")]
        "c" | "cpp" => Some(LanguageInfo {
            name: if lang_name == "c" { "c" } else { "cpp" },
            language: tree_sitter_cpp::LANGUAGE.into(),
            element_query: cpp::ELEMENT_QUERY,
            call_query: cpp::CALL_QUERY,
            reference_query: Some(cpp::REFERENCE_QUERY),
            import_query: Some(cpp::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(cpp::DEFUSE_QUERY),
            extract_function_name: Some(cpp::extract_function_name),
            find_method_for_receiver: Some(cpp::find_method_for_receiver),
            find_receiver_type: None,
            extract_inheritance: Some(cpp::extract_inheritance),
        }),
        #[cfg(feature = "lang-java")]
        "java" => Some(LanguageInfo {
            name: "java",
            language: tree_sitter_java::LANGUAGE.into(),
            element_query: java::ELEMENT_QUERY,
            call_query: java::CALL_QUERY,
            reference_query: Some(java::REFERENCE_QUERY),
            import_query: Some(java::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(java::DEFUSE_QUERY),
            extract_function_name: Some(java::extract_function_name),
            find_method_for_receiver: Some(java::find_method_for_receiver),
            find_receiver_type: Some(java::find_receiver_type),
            extract_inheritance: Some(java::extract_inheritance),
        }),
        #[cfg(feature = "lang-kotlin")]
        "kotlin" => Some(LanguageInfo {
            name: "kotlin",
            language: tree_sitter_kotlin_ng::LANGUAGE.into(),
            element_query: kotlin::ELEMENT_QUERY,
            call_query: kotlin::CALL_QUERY,
            reference_query: Some(kotlin::REFERENCE_QUERY),
            import_query: Some(kotlin::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(kotlin::DEFUSE_QUERY),
            extract_function_name: Some(kotlin::extract_function_name),
            find_method_for_receiver: Some(kotlin::find_method_for_receiver),
            find_receiver_type: Some(kotlin::find_receiver_type),
            extract_inheritance: Some(kotlin::extract_inheritance),
        }),
        #[cfg(feature = "lang-fortran")]
        "fortran" => Some(LanguageInfo {
            name: "fortran",
            language: tree_sitter_fortran::LANGUAGE.into(),
            element_query: fortran::ELEMENT_QUERY,
            call_query: fortran::CALL_QUERY,
            reference_query: Some(fortran::REFERENCE_QUERY),
            import_query: Some(fortran::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: None,
            extract_function_name: Some(fortran::extract_function_name),
            find_method_for_receiver: Some(fortran::find_method_for_receiver),
            find_receiver_type: Some(fortran::find_receiver_type),
            extract_inheritance: Some(fortran::extract_inheritance),
        }),
        #[cfg(feature = "lang-csharp")]
        "csharp" => Some(LanguageInfo {
            name: "csharp",
            language: tree_sitter_c_sharp::LANGUAGE.into(),
            element_query: csharp::ELEMENT_QUERY,
            call_query: csharp::CALL_QUERY,
            reference_query: Some(csharp::REFERENCE_QUERY),
            import_query: Some(csharp::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(csharp::DEFUSE_QUERY),
            extract_function_name: Some(csharp::extract_function_name),
            find_method_for_receiver: Some(csharp::find_method_for_receiver),
            find_receiver_type: Some(csharp::find_receiver_type),
            extract_inheritance: Some(csharp::extract_inheritance),
        }),
        #[cfg(feature = "lang-javascript")]
        "javascript" => Some(LanguageInfo {
            name: "javascript",
            language: tree_sitter_javascript::LANGUAGE.into(),
            element_query: javascript::ELEMENT_QUERY,
            call_query: javascript::CALL_QUERY,
            reference_query: None,
            import_query: Some(javascript::IMPORT_QUERY),
            impl_query: None,
            impl_trait_query: None,
            defuse_query: Some(javascript::DEFUSE_QUERY),
            extract_function_name: Some(javascript::extract_function_name),
            find_method_for_receiver: Some(javascript::find_method_for_receiver),
            find_receiver_type: Some(javascript::find_receiver_type),
            extract_inheritance: Some(javascript::extract_inheritance),
        }),
        // HTML is a reserved feature stub. `tree-sitter-html` 0.23.x is incompatible with the
        // tree-sitter 0.26 API used by this crate; full HTML support is blocked on the
        // tree-sitter-html ^0.25 release. Until then, analysis of `.html`/`.htm` files returns
        // `None` here, which causes `analyze_file` to emit an INVALID_PARAMS error with the
        // message "unsupported language: html". This is intentional: the extension is registered
        // so that the file-type is recognised and a clear error surfaces rather than silently
        // skipping the file. Track https://github.com/tree-sitter/tree-sitter-html for the
        // ^0.25 release.
        #[cfg(feature = "lang-html")]
        "html" => None,
        #[cfg(feature = "lang-markdown")]
        "markdown" => Some(LanguageInfo {
            name: "markdown",
            language: tree_sitter_md::LANGUAGE.into(),
            element_query: markdown::ELEMENT_QUERY,
            call_query: markdown::CALL_QUERY,
            reference_query: None,
            import_query: None,
            impl_query: None,
            impl_trait_query: None,
            defuse_query: None,
            extract_function_name: None,
            find_method_for_receiver: None,
            find_receiver_type: None,
            extract_inheritance: None,
        }),
        _ => None,
    }
}

/// Get the tree-sitter Language object for a given language name.
///
/// Returns `None` if the language is not supported or not compiled in.
#[must_use]
pub fn get_ts_language(lang_name: &str) -> Option<Language> {
    match lang_name {
        #[cfg(feature = "lang-rust")]
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        #[cfg(feature = "lang-python")]
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        #[cfg(feature = "lang-typescript")]
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        #[cfg(feature = "lang-tsx")]
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        #[cfg(feature = "lang-go")]
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        #[cfg(feature = "lang-cpp")]
        "c" | "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        #[cfg(feature = "lang-java")]
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        #[cfg(feature = "lang-kotlin")]
        "kotlin" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        #[cfg(feature = "lang-fortran")]
        "fortran" => Some(tree_sitter_fortran::LANGUAGE.into()),
        #[cfg(feature = "lang-csharp")]
        "csharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        #[cfg(feature = "lang-javascript")]
        "javascript" => Some(tree_sitter_javascript::LANGUAGE.into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_language_info_known() {
        // Happy path: known languages return Some
        assert!(
            get_language_info("rust").is_some(),
            "expected Some for 'rust'"
        );
        assert!(get_language_info("go").is_some(), "expected Some for 'go'");
        assert!(
            get_language_info("python").is_some(),
            "expected Some for 'python'"
        );
    }

    #[test]
    fn test_get_language_info_unknown() {
        // Edge case: unknown language returns None
        assert!(
            get_language_info("cobol").is_none(),
            "expected None for 'cobol'"
        );
    }

    #[test]
    fn test_get_ts_language_known() {
        // Happy path: known language returns Some
        assert!(
            get_ts_language("rust").is_some(),
            "expected Some for 'rust'"
        );
    }

    #[test]
    fn test_get_ts_language_unknown() {
        // Edge case: unknown language returns None
        assert!(
            get_ts_language("cobol").is_none(),
            "expected None for 'cobol'"
        );
    }
}
