// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! Output formatting for analysis results across different modes.
//!
//! Formats semantic analysis, call graphs, and directory structures into human-readable text.
//! Handles multiline wrapping, pagination, and summary generation.

pub mod emit;
pub mod pagination;
pub mod render;
pub mod summary;

// Re-export all public API items for backward compatibility.
// Downstream files import via `aptu_coder_core::formatter::*` or specific paths.

pub use self::pagination::{
    format_file_details_paginated, format_focused_paginated, format_structure_paginated,
};
pub use self::render::{
    FormatterError, format_file_details, format_file_details_summary, format_module_info,
};
pub use self::summary::{format_focused_summary, format_structure, format_summary};

// pub(crate) re-exports for internal cross-crate use.
// Used by formatter_defuse.rs: snippet_one_line, strip_base_path.
// Used by analyze.rs: format_focused_internal, format_focused_summary_internal.
pub(crate) use self::emit::{snippet_one_line, strip_base_path};
pub(crate) use self::summary::{format_focused_internal, format_focused_summary_internal};
