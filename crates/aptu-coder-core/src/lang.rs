// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Language detection by file extension.
//!
//! Maps file extensions to supported language identifiers.

const EXTENSION_MAP: &[(&str, &str)] = &[
    #[cfg(feature = "lang-cpp")]
    ("c", "c"),
    #[cfg(feature = "lang-cpp")]
    ("cc", "cpp"),
    #[cfg(feature = "lang-javascript")]
    ("cjs", "javascript"),
    #[cfg(feature = "lang-cpp")]
    ("cpp", "cpp"),
    #[cfg(feature = "lang-cpp")]
    ("cxx", "cpp"),
    #[cfg(feature = "lang-fortran")]
    ("f", "fortran"),
    #[cfg(feature = "lang-fortran")]
    ("f03", "fortran"),
    #[cfg(feature = "lang-fortran")]
    ("f08", "fortran"),
    #[cfg(feature = "lang-fortran")]
    ("f77", "fortran"),
    #[cfg(feature = "lang-fortran")]
    ("f90", "fortran"),
    #[cfg(feature = "lang-fortran")]
    ("f95", "fortran"),
    #[cfg(feature = "lang-fortran")]
    ("for", "fortran"),
    #[cfg(feature = "lang-fortran")]
    ("ftn", "fortran"),
    #[cfg(feature = "lang-cpp")]
    ("h", "cpp"),
    #[cfg(feature = "lang-csharp")]
    ("cs", "csharp"),
    #[cfg(feature = "lang-cpp")]
    ("hpp", "cpp"),
    #[cfg(feature = "lang-cpp")]
    ("hxx", "cpp"),
    #[cfg(feature = "lang-javascript")]
    ("js", "javascript"),
    #[cfg(feature = "lang-javascript")]
    ("mjs", "javascript"),
    #[cfg(feature = "lang-go")]
    ("go", "go"),
    #[cfg(feature = "lang-java")]
    ("java", "java"),
    #[cfg(feature = "lang-kotlin")]
    ("kt", "kotlin"),
    #[cfg(feature = "lang-kotlin")]
    ("kts", "kotlin"),
    #[cfg(feature = "lang-python")]
    ("py", "python"),
    #[cfg(feature = "lang-rust")]
    ("rs", "rust"),
    #[cfg(feature = "lang-typescript")]
    ("ts", "typescript"),
    #[cfg(feature = "lang-tsx")]
    ("tsx", "tsx"),
    #[cfg(feature = "lang-html")]
    ("html", "html"),
    #[cfg(feature = "lang-html")]
    ("htm", "html"),
    #[cfg(feature = "lang-markdown")]
    ("md", "markdown"),
    #[cfg(feature = "lang-markdown")]
    ("mdx", "markdown"),
    ("astro", "astro"),
    ("css", "css"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("json", "json"),
    ("toml", "toml"),
];

/// Returns the language identifier for the given file extension, or `None` if unsupported.
///
/// The lookup is case-insensitive. Supported extensions include `rs`, `py`, `go`, `java`,
/// `ts`, `tsx`, `js`, `mjs`, `cjs`, `c`, `cc`, `cpp`, `cxx`, `h`, `hpp`, `hxx`, `cs`,
/// Fortran variants `f`, `f77`, `f90`, `f95`, `f03`, `f08`, `for`, `ftn`,
/// HTML variants `html`, `htm`, and Markdown variants `md`, `mdx`.
#[must_use]
pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    EXTENSION_MAP
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, lang)| *lang)
}

/// Returns all file extensions supported by the compiled feature set.
///
/// Each entry corresponds to one row in `EXTENSION_MAP`. The list is used to
/// build human-readable error messages without duplicating the extension list.
#[must_use]
pub fn supported_extensions() -> Vec<&'static str> {
    EXTENSION_MAP.iter().map(|(ext, _)| *ext).collect()
}

/// Returns a static slice of all supported language names based on compiled features.
///
/// The returned slice contains language identifiers like `"rust"`, `"python"`, `"go"`, etc.,
/// depending on which language features are enabled at compile time.
#[must_use]
pub fn supported_languages() -> &'static [&'static str] {
    &[
        #[cfg(feature = "lang-rust")]
        "rust",
        #[cfg(feature = "lang-go")]
        "go",
        #[cfg(feature = "lang-java")]
        "java",
        #[cfg(feature = "lang-kotlin")]
        "kotlin",
        #[cfg(feature = "lang-python")]
        "python",
        #[cfg(feature = "lang-typescript")]
        "typescript",
        #[cfg(feature = "lang-tsx")]
        "tsx",
        #[cfg(feature = "lang-javascript")]
        "javascript",
        #[cfg(feature = "lang-fortran")]
        "fortran",
        #[cfg(feature = "lang-cpp")]
        "c",
        #[cfg(feature = "lang-cpp")]
        "cpp",
        #[cfg(feature = "lang-csharp")]
        "csharp",
        #[cfg(feature = "lang-html")]
        "html",
        #[cfg(feature = "lang-markdown")]
        "markdown",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_for_extension_happy_path() {
        #[cfg(feature = "lang-rust")]
        assert_eq!(language_for_extension("rs"), Some("rust"));
        #[cfg(feature = "lang-python")]
        assert_eq!(language_for_extension("py"), Some("python"));
        #[cfg(feature = "lang-go")]
        assert_eq!(language_for_extension("go"), Some("go"));
        #[cfg(feature = "lang-java")]
        assert_eq!(language_for_extension("java"), Some("java"));
        #[cfg(feature = "lang-typescript")]
        assert_eq!(language_for_extension("ts"), Some("typescript"));
        #[cfg(feature = "lang-tsx")]
        assert_eq!(language_for_extension("tsx"), Some("tsx"));
        #[cfg(feature = "lang-fortran")]
        assert_eq!(language_for_extension("f90"), Some("fortran"));
        #[cfg(feature = "lang-fortran")]
        assert_eq!(language_for_extension("for"), Some("fortran"));
        #[cfg(feature = "lang-fortran")]
        assert_eq!(language_for_extension("ftn"), Some("fortran"));
        #[cfg(feature = "lang-cpp")]
        assert_eq!(language_for_extension("c"), Some("c"));
        #[cfg(feature = "lang-cpp")]
        assert_eq!(language_for_extension("cpp"), Some("cpp"));
        #[cfg(feature = "lang-cpp")]
        assert_eq!(language_for_extension("h"), Some("cpp"));
        #[cfg(feature = "lang-cpp")]
        assert_eq!(language_for_extension("hpp"), Some("cpp"));
        #[cfg(feature = "lang-cpp")]
        assert_eq!(language_for_extension("cc"), Some("cpp"));
        #[cfg(feature = "lang-kotlin")]
        assert_eq!(language_for_extension("kt"), Some("kotlin"));
        #[cfg(feature = "lang-kotlin")]
        assert_eq!(language_for_extension("kts"), Some("kotlin"));
    }

    /// Asserts every extension in `EXTENSION_MAP` appears as an alternation in
    /// `SUPPORTED_FILE_EXT_PATTERN`, preventing drift when a new language is added.
    /// The check is a substring match: the pattern has the form `...(ext1|ext2|...)...`
    /// so each extension must appear as `ext|` or `ext)`.
    #[test]
    fn test_supported_file_ext_pattern_covers_all_extension_map_entries() {
        #[cfg(feature = "schemars")]
        for (ext, _lang) in EXTENSION_MAP {
            let in_alternation = crate::schema_helpers::SUPPORTED_FILE_EXT_PATTERN
                .contains(&format!("{ext}|"))
                || crate::schema_helpers::SUPPORTED_FILE_EXT_PATTERN.contains(&format!("{ext})"));
            assert!(
                in_alternation,
                "SUPPORTED_FILE_EXT_PATTERN is missing extension '{ext}' from EXTENSION_MAP; \
                 add it to schema_helpers.rs"
            );
        }
    }

    #[test]
    fn test_language_for_extension_edge_case() {
        assert_eq!(language_for_extension("unknown"), None);
        assert_eq!(language_for_extension(""), None);
        #[cfg(feature = "lang-rust")]
        assert_eq!(language_for_extension("RS"), Some("rust"));
        // Uppercase Fortran extensions resolved via eq_ignore_ascii_case
        #[cfg(feature = "lang-fortran")]
        assert_eq!(language_for_extension("F90"), Some("fortran"));
        #[cfg(feature = "lang-fortran")]
        assert_eq!(language_for_extension("FOR"), Some("fortran"));
    }
}
