// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! HTML language stub.
//!
//! HTML is recognised by extension (`html`, `htm`) but has no tree-sitter grammar
//! dependency in this release. Extraction returns zero functions and imports.
//! This module reserves the feature slot for a future `tree-sitter-html` upgrade.

/// Tree-sitter element query for HTML (empty -- no grammar compiled in).
pub const ELEMENT_QUERY: &str = "";

/// Tree-sitter call query for HTML (empty -- no grammar compiled in).
pub const CALL_QUERY: &str = "";
