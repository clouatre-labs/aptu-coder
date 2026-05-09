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
];

/// Returns the language identifier for the given file extension, or `None` if unsupported.
///
/// The lookup is case-insensitive. Supported extensions include `rs`, `py`, `go`, `java`,
/// `ts`, `tsx`, `js`, `mjs`, `cjs`, `c`, `cc`, `cpp`, `cxx`, `h`, `hpp`, `hxx`, `cs`,
/// and Fortran variants `f`, `f77`, `f90`, `f95`, `f03`, `f08`, `for`, `ftn`.
#[must_use]
pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    EXTENSION_MAP
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, lang)| *lang)
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
