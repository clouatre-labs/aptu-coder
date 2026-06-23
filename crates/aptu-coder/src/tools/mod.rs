//! Extraction pattern for milestone 17 (lib.rs decomposition).
//!
//! This module will contain extracted tool handler logic as free functions,
//! keeping the `#[tool(...)]`-decorated methods in `lib.rs` as thin shims.
//! The pattern ensures a smooth, incremental decomposition with no single
//! large-bang refactor.
//!
//! Pattern rules
//! -------------
//!
//! (a) **Extracted handlers are free functions, not methods.** Each handler
//!     is a plain `pub(crate) fn` in this module, not an `impl CodeAnalyzer`
//!     method. This breaks the coupling to `&self` and makes the function
//!     testable in isolation.
//!
//! (b) **Explicit parameters instead of `&self`.** State that the handler
//!     needs (e.g., `config: &Config`, `state: &HandlerState`) is passed
//!     explicitly as function parameters. The shim in `lib.rs` extracts
//!     values from `&self` before calling the extracted function.
//!
//! (c) **`#[tool(...)]`-decorated method and `#[instrument(...)]` decorator
//!     remain in `lib.rs` as thin shims.** The `#[tool(..)]` attribute
//!     macro and the outer `#[instrument(..)]` stay on the small stub in
//!     `lib.rs`. The stub validates parameters, extracts state, and
//!     delegates to the free function here.
//!
//! (d) **The extracted free function also carries `#[instrument(skip(...))]`
//!     on its own signature.** This preserves distributed tracing context
//!     after the call leaves the `lib.rs` shim. Use `skip` for large
//!     internal types whose fields are not useful trace attributes.
//!
//! (e) **`edit_failure_counts` stays in `CodeAnalyzer` and is passed by
//!     reference to extracted edit handlers.** The concurrent failure-tracking
//!     map is not moved into `tools/`; it remains an `Arc<Mutex<...>>` field
//!     on `CodeAnalyzer`. Extracted edit handlers receive `&edit_failure_counts`
//!     as a parameter.
//!
//! (f) **`#[tool_router]` and `#[tool_handler]` impl blocks remain in
//!     `lib.rs` permanently.** The `#[tool_router]` impl on `CodeAnalyzer`
//!     and the `#[tool_handler]` impl for `ServerHandler` must not be
//!     moved into this module. They are the framework glue that ties all
//!     tools together and must live in the crate root.
