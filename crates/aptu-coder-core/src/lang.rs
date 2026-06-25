// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Language detection by file extension.
//!
//! Maps file extensions to supported language identifiers.
//!
//! All languages are unconditionally compiled in.  The historical `lang-*` Cargo feature
//! gates have been removed; every registered extension is always available at runtime.
//! This simplifies the build matrix and guarantees that callers never get a feature-gate
//! error for a language that "should" be supported.

const EXTENSION_MAP: &[(&str, &str)] = &[
    ("c", "c"),
    ("cc", "cpp"),
    ("cjs", "javascript"),
    ("cpp", "cpp"),
    ("cxx", "cpp"),
    ("f", "fortran"),
    ("f03", "fortran"),
    ("f08", "fortran"),
    ("f77", "fortran"),
    ("f90", "fortran"),
    ("f95", "fortran"),
    ("for", "fortran"),
    ("ftn", "fortran"),
    ("h", "cpp"),
    ("cs", "csharp"),
    ("hpp", "cpp"),
    ("hxx", "cpp"),
    ("js", "javascript"),
    ("mjs", "javascript"),
    ("go", "go"),
    ("java", "java"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("py", "python"),
    ("rs", "rust"),
    ("ts", "typescript"),
    ("tsx", "tsx"),
    ("html", "html"),
    ("htm", "html"),
    ("md", "markdown"),
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

/// Returns a static slice of all supported language names.
///
/// All languages are unconditionally compiled in; the historical `lang-*` feature
/// gates have been removed.
#[must_use]
pub fn supported_languages() -> &'static [&'static str] {
    &[
        "rust",
        "go",
        "java",
        "kotlin",
        "python",
        "typescript",
        "tsx",
        "javascript",
        "fortran",
        "c",
        "cpp",
        "csharp",
        "html",
        "markdown",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_for_extension_happy_path() {
        assert_eq!(language_for_extension("rs"), Some("rust"));
        assert_eq!(language_for_extension("py"), Some("python"));
        assert_eq!(language_for_extension("go"), Some("go"));
        assert_eq!(language_for_extension("java"), Some("java"));
        assert_eq!(language_for_extension("ts"), Some("typescript"));
        assert_eq!(language_for_extension("tsx"), Some("tsx"));
        assert_eq!(language_for_extension("f90"), Some("fortran"));
        assert_eq!(language_for_extension("for"), Some("fortran"));
        assert_eq!(language_for_extension("ftn"), Some("fortran"));
        assert_eq!(language_for_extension("c"), Some("c"));
        assert_eq!(language_for_extension("cpp"), Some("cpp"));
        assert_eq!(language_for_extension("h"), Some("cpp"));
        assert_eq!(language_for_extension("hpp"), Some("cpp"));
        assert_eq!(language_for_extension("cc"), Some("cpp"));
        assert_eq!(language_for_extension("kt"), Some("kotlin"));
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
        assert_eq!(language_for_extension("RS"), Some("rust"));
        // Uppercase Fortran extensions resolved via eq_ignore_ascii_case
        assert_eq!(language_for_extension("F90"), Some("fortran"));
        assert_eq!(language_for_extension("FOR"), Some("fortran"));
    }
}
