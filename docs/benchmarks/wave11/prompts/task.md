## Task: TypeScript JSX (TSX) Language Support Re-wiring

You are re-wiring TypeScript JSX (tsx) language support in the aptu-coder repository. The tsx feature
has been partially stripped from the codebase, and your task is to restore it correctly.

Repository: clouatre-labs/aptu-coder
Working directory: REPO_PATH_PLACEHOLDER

The task requires modifying two files to re-add tsx support:

1. **crates/aptu-coder-core/src/languages/mod.rs** (lines 112-127 and 240-241)
   - Add tsx arm to `get_language_info()` function (after typescript arm)
   - Add tsx arm to `get_ts_language()` function (after typescript arm)

2. **crates/aptu-coder-core/src/lang.rs** (lines 56-57 and 90-91)
   - Add tsx extension mapping to `EXTENSION_MAP`
   - Add "tsx" to `supported_languages()` function

## Exact Targets

**mod.rs - get_language_info() arm (insert after typescript arm, before go arm):**
```rust
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
```

**mod.rs - get_ts_language() arm (insert after typescript arm, before go arm):**
```rust
        #[cfg(feature = "lang-tsx")]
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
```

**lang.rs - EXTENSION_MAP entry (insert after typescript entries):**
```rust
    #[cfg(feature = "lang-tsx")]
    ("tsx", "tsx"),
```

**lang.rs - supported_languages() entry (insert after typescript entries):**
```rust
        #[cfg(feature = "lang-tsx")]
        "tsx",
```

## Three Genuine Traps

Be aware of these common mistakes that will cause verification to fail:

1. **Namespace mismatch:** Use `typescript::` prefix for queries and functions (not `tsx::`).
   The queries come from the typescript module, not a separate tsx module.

2. **Shared pub mod:** The `pub mod typescript;` declaration in mod.rs is shared between
   typescript and tsx features. Do NOT add a separate `pub mod tsx;` -- only add the
   feature-gated arms in the match statements.

3. **LANGUAGE_TSX suffix:** Use `tree_sitter_typescript::LANGUAGE_TSX` (not LANGUAGE_TYPESCRIPT).
   This is the correct constant name for the JSX variant.

**Do not run `cargo test` or any build commands.** The benchmark infrastructure will verify
the re-wiring externally after you complete your implementation.

Output must be valid JSON matching this exact schema:

```json
{
  "run_id": "RUN_ID_PLACEHOLDER",
  "condition": "CONDITION_PLACEHOLDER",
  "files_modified": [
    {"path": "crates/aptu-coder-core/src/languages/mod.rs", "changes_description": "Added tsx arms to get_language_info and get_ts_language"},
    {"path": "crates/aptu-coder-core/src/lang.rs", "changes_description": "Added tsx extension mapping and supported language entry"}
  ],
  "tsx_wiring_complete": true,
  "mod_rs_get_language_info_arm_added": true,
  "mod_rs_get_ts_language_arm_added": true,
  "lang_rs_extension_map_added": true,
  "lang_rs_supported_languages_added": true,
  "tool_calls_total": 0
}
```
