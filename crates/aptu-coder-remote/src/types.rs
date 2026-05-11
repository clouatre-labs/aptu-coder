// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Parameters for `remote_tree` tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoteTreeParams {
    /// URL of the repository (e.g. `https://github.com/owner/repo` or
    /// `https://gitlab.com/owner/repo`).
    pub url: String,
    /// Subdirectory path inside the repository to list. Defaults to the root.
    pub path: Option<String>,
    /// Branch, tag, or commit SHA. Defaults to the repository default branch.
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    /// Maximum tree depth to recurse (1 = top-level only; default: 2).
    pub depth: Option<u32>,
}

/// A single entry returned from a remote repository tree listing.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoteTreeEntry {
    /// Path of the entry relative to the requested root.
    pub path: String,
    /// Entry type: `"blob"` (file) or `"tree"` (directory).
    #[serde(rename = "type")]
    pub entry_type: String,
}

/// Output returned by `remote_tree`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoteTreeOutput {
    /// Compact human-readable summary (matches `analyze_directory` style).
    pub formatted: String,
    /// Total number of files found.
    pub total_files: u64,
    /// Count of files per file extension (e.g. `{"rs": 42, "toml": 3}`).
    pub extension_counts: HashMap<String, u64>,
    /// Individual tree entries (files and directories).
    pub entries: Vec<RemoteTreeEntry>,
}

/// Parameters for `remote_file` tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoteFileParams {
    /// URL of the repository (e.g. `https://github.com/owner/repo`).
    pub url: String,
    /// Path to the file inside the repository.
    pub path: String,
    /// Branch, tag, or commit SHA. Defaults to the repository default branch.
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    /// Optional line range in `START-END` format (1-indexed, inclusive).
    /// Example: `"10-50"` returns lines 10 through 50.
    pub line_range: Option<String>,
}

/// Output returned by `remote_file`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoteFileOutput {
    /// File content (potentially sliced if `line_range` was provided).
    pub content: String,
    /// Size in bytes of the full file (before any line slicing).
    pub size_bytes: usize,
    /// The resolved git ref (branch / tag / commit SHA).
    pub resolved_ref: String,
    /// The path of the file as returned by the remote API.
    pub path: String,
}
