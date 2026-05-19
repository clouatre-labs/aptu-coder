// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

//! Async helpers for fetching GitLab and GitHub repository trees and files without cloning.
//!
//! This crate provides two main functions:
//! - [`fetch_tree`]: Explore a remote repository's directory structure
//! - [`fetch_file`]: Fetch a single file from a remote repository with optional line range slicing
//!
//! Both functions auto-detect the platform (GitHub or GitLab) from the repository URL and
//! require authentication tokens to be set in environment variables:
//! - `GITHUB_TOKEN` for GitHub repositories
//! - `GITLAB_TOKEN` for GitLab repositories
//!
//! For more information, see the [README](https://github.com/clouatre-labs/aptu-coder/blob/main/crates/aptu-coder-remote/README.md).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod types;

use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;
use tracing::debug;

use crate::types::{RemoteFileOutput, RemoteTreeEntry, RemoteTreeOutput};

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Supported remote hosting platforms.
///
/// Platform is auto-detected from the repository URL. GitHub and GitLab use different
/// authentication tokens and APIs.
#[derive(Debug, Clone)]
pub enum Platform {
    /// GitLab instance (may be self-hosted, but currently only `gitlab.com` is auto-detected).
    ///
    /// Requires `GITLAB_TOKEN` environment variable to be set.
    GitLab { host: String },
    /// GitHub (`github.com`).
    ///
    /// Requires `GITHUB_TOKEN` environment variable to be set.
    GitHub,
}

/// Errors produced by the remote helpers.
#[derive(Debug, Error)]
pub enum RemoteError {
    /// The URL host is not a supported platform.
    #[error("unsupported host: {0} – only gitlab.com and github.com are supported")]
    UnsupportedHost(String),
    /// The supplied URL could not be parsed.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// The `GITLAB_TOKEN` environment variable is not set.
    #[error("GITLAB_TOKEN environment variable is not set")]
    MissingGitLabToken,
    /// The `GITHUB_TOKEN` environment variable is not set.
    #[error("GITHUB_TOKEN environment variable is not set")]
    MissingGitHubToken,
    /// The requested resource was not found (404).
    #[error("resource not found: {0}")]
    NotFound(String),
    /// An API error occurred.
    #[error("API error: {0}")]
    Api(String),
    /// The supplied line range is invalid.
    #[error("invalid line range: {0}")]
    InvalidLineRange(String),
}

/// Parse a URL and return the platform, owner, and repository name.
///
/// # Errors
/// Returns [`RemoteError::InvalidUrl`] if the URL cannot be parsed or has fewer
/// than 2 path segments, and [`RemoteError::UnsupportedHost`] if the host is
/// not `gitlab.com` or `github.com`.
pub fn detect_platform(url: &str) -> Result<(Platform, String, String), RemoteError> {
    let parsed = url::Url::parse(url).map_err(|e| RemoteError::InvalidUrl(e.to_string()))?;

    if parsed.scheme() != "https" {
        return Err(RemoteError::InvalidUrl(format!(
            "only https:// URLs are supported, got: {}://",
            parsed.scheme()
        )));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| RemoteError::InvalidUrl("no host in URL".to_string()))?
        .to_lowercase();

    // Extract owner / repo from path segments, ignoring empty segments
    let segments: Vec<&str> = parsed
        .path_segments()
        .ok_or_else(|| RemoteError::InvalidUrl("no path segments".to_string()))?
        .filter(|s| !s.is_empty())
        .collect();

    if segments.len() < 2 {
        return Err(RemoteError::InvalidUrl(
            "URL must contain owner/repo path".to_string(),
        ));
    }

    let platform = match host.as_str() {
        "gitlab.com" => Platform::GitLab { host: host.clone() },
        "github.com" => {
            // GitHub does not support nested namespaces; reject URLs with more than 2 segments
            if segments.len() > 2 {
                return Err(RemoteError::InvalidUrl(
                    "GitHub URLs must contain exactly owner/repo path (no extra segments)"
                        .to_string(),
                ));
            }
            Platform::GitHub
        }
        other => return Err(RemoteError::UnsupportedHost(other.to_string())),
    };

    // Extract owner and repo based on platform
    let owner = segments[0].to_string();
    let repo = match platform {
        Platform::GitLab { .. } => {
            // For GitLab, join all segments after the first (owner) to support nested namespaces
            segments[1..].join("/")
        }
        Platform::GitHub => {
            // For GitHub, use only the second segment (no nested namespaces)
            segments[1].to_string()
        }
    }
    .trim_end_matches(".git")
    .to_string();

    Ok((platform, owner, repo))
}

// ---------------------------------------------------------------------------
// Line range helpers
// ---------------------------------------------------------------------------

/// Parse a line range string of the form `"START-END"` (1-indexed, inclusive).
///
/// # Errors
/// Returns [`RemoteError::InvalidLineRange`] for malformed input or out-of-order
/// bounds.
pub fn parse_line_range(s: &str) -> Result<(usize, usize), RemoteError> {
    let parts: Vec<&str> = s.splitn(2, '-').collect();
    if parts.len() != 2 {
        return Err(RemoteError::InvalidLineRange(format!(
            "expected START-END format, got: {s}"
        )));
    }
    let start: usize = parts[0].parse().map_err(|_| {
        RemoteError::InvalidLineRange(format!("start is not a number: {}", parts[0]))
    })?;
    let end: usize = parts[1]
        .parse()
        .map_err(|_| RemoteError::InvalidLineRange(format!("end is not a number: {}", parts[1])))?;
    if start == 0 {
        return Err(RemoteError::InvalidLineRange(
            "line numbers are 1-indexed; start must be >= 1".to_string(),
        ));
    }
    if end < start {
        return Err(RemoteError::InvalidLineRange(format!(
            "end ({end}) must be >= start ({start})"
        )));
    }
    Ok((start, end))
}

/// Slice `content` to the given 1-indexed inclusive line range.
///
/// If `end` is beyond the last line the function returns whatever lines are
/// available.
pub fn slice_lines(content: &str, start: usize, end: usize) -> String {
    content
        .lines()
        .skip(start.saturating_sub(1))
        .take(end - start + 1)
        .collect::<Vec<&str>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Internal: extension counting + formatting
// ---------------------------------------------------------------------------

pub(crate) fn build_tree_output(entries: Vec<RemoteTreeEntry>) -> RemoteTreeOutput {
    let mut extension_counts: HashMap<String, u64> = HashMap::new();
    let mut total_files: u64 = 0;

    for entry in &entries {
        if entry.entry_type == "blob" {
            total_files += 1;
            if let Some(ext) = std::path::Path::new(&entry.path)
                .extension()
                .and_then(|e| e.to_str())
            {
                *extension_counts.entry(ext.to_string()).or_insert(0) += 1;
            }
        }
    }

    let mut ext_lines: Vec<String> = extension_counts
        .iter()
        .map(|(k, v)| format!("  .{k}: {v}"))
        .collect();
    ext_lines.sort();

    let formatted = format!(
        "total files: {}\n{}\nentries: {}",
        total_files,
        ext_lines.join("\n"),
        entries.len()
    );

    RemoteTreeOutput {
        formatted,
        total_files,
        extension_counts,
        entries,
    }
}

// ---------------------------------------------------------------------------
// GitLab helpers (using the `gitlab` crate)
// ---------------------------------------------------------------------------

/// A minimal deserialization struct for GitLab tree entries.
#[derive(Deserialize)]
struct GitLabTreeItem {
    #[serde(rename = "type")]
    item_type: String,
    path: String,
}

/// Fetch one level of a GitLab tree (non-recursive).
/// Helper function for BFS traversal in gitlab_fetch_tree.
async fn gitlab_fetch_one_level(
    client: &gitlab::AsyncGitlab,
    project: &str,
    path: &str,
    git_ref: Option<&str>,
) -> Result<Vec<GitLabTreeItem>, RemoteError> {
    use gitlab::api::projects::repository::Tree;
    use gitlab::api::{self, AsyncQuery as _};

    let mut builder = Tree::builder();
    builder.project(project);
    if !path.is_empty() {
        builder.path(path);
    }
    if let Some(r) = git_ref {
        builder.ref_(r);
    }
    builder.recursive(false);

    let endpoint = builder
        .build()
        .map_err(|e| RemoteError::Api(e.to_string()))?;

    api::paged(endpoint, gitlab::api::Pagination::All)
        .query_async(client)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("404") || msg.contains("Not Found") {
                RemoteError::NotFound(msg)
            } else {
                RemoteError::Api(msg)
            }
        })
}

pub(crate) async fn gitlab_fetch_tree(
    host: &str,
    token: &str,
    project: &str, // "owner/repo"
    path: Option<&str>,
    git_ref: Option<&str>,
    depth: u32,
) -> Result<Vec<RemoteTreeEntry>, RemoteError> {
    use gitlab::GitlabBuilder;
    use std::collections::{HashSet, VecDeque};

    // depth=0 returns empty list
    if depth == 0 {
        return Ok(Vec::new());
    }

    let client = GitlabBuilder::new(host, token)
        .build_async()
        .await
        .map_err(|e| RemoteError::Api(e.to_string()))?;

    let mut entries: Vec<RemoteTreeEntry> = Vec::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    // `seen` tracks directory paths that have been queued for traversal.
    // GitLab and GitHub both return canonical relative paths (no trailing
    // slashes, no duplicates within a single level response), so a simple
    // string HashSet is sufficient to prevent re-queuing a directory that
    // appeared as a subdirectory of multiple already-visited parents.
    let mut seen: HashSet<String> = HashSet::new();

    // Start with the root path
    let root_path = path.unwrap_or("").to_string();
    queue.push_back((root_path.clone(), 0));
    seen.insert(root_path.clone());

    while let Some((current_path, current_depth)) = queue.pop_front() {
        // Fetch one level
        let items = gitlab_fetch_one_level(&client, project, &current_path, git_ref).await?;

        for item in items {
            entries.push(RemoteTreeEntry {
                path: item.path.clone(),
                entry_type: item.item_type.clone(),
            });

            // If we haven't reached max depth and this is a directory, queue it
            if current_depth + 1 < depth
                && item.item_type == "tree"
                && seen.insert(item.path.clone())
            {
                queue.push_back((item.path, current_depth + 1));
            }
        }
    }

    Ok(entries)
}

/// A minimal deserialization struct for GitLab file content response.
#[derive(Deserialize)]
struct GitLabFileContent {
    content: String,
    encoding: String,
    file_path: String,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    #[allow(dead_code)]
    size: Option<u64>,
}

pub(crate) async fn gitlab_fetch_file(
    host: &str,
    token: &str,
    project: &str,
    path: &str,
    git_ref: Option<&str>,
) -> Result<RemoteFileOutput, RemoteError> {
    use gitlab::GitlabBuilder;
    use gitlab::api::{AsyncQuery as _, projects::repository::files::File};

    let client = GitlabBuilder::new(host, token)
        .build_async()
        .await
        .map_err(|e| RemoteError::Api(e.to_string()))?;

    let ref_str = git_ref.unwrap_or("HEAD");

    let endpoint = File::builder()
        .project(project)
        .file_path(path)
        .ref_(ref_str)
        .build()
        .map_err(|e| RemoteError::Api(e.to_string()))?;

    let item: GitLabFileContent = endpoint.query_async(&client).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("404") || msg.contains("Not Found") {
            RemoteError::NotFound(msg)
        } else {
            RemoteError::Api(msg)
        }
    })?;

    // Decode content (GitLab returns base64-encoded content)
    let raw_content = if item.encoding == "base64" {
        use base64::Engine as _;
        let cleaned: String = item
            .content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let bytes = base64::prelude::BASE64_STANDARD
            .decode(cleaned.as_bytes())
            .map_err(|e| RemoteError::Api(format!("base64 decode error: {e}")))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        item.content
    };

    let size_bytes = raw_content.len();
    let resolved_ref = item.git_ref.unwrap_or_else(|| ref_str.to_string());

    Ok(RemoteFileOutput {
        content: raw_content,
        size_bytes,
        resolved_ref,
        path: item.file_path,
    })
}

// ---------------------------------------------------------------------------
// GitHub helpers (using the `octocrab` crate)
// ---------------------------------------------------------------------------

/// Build an `Octocrab` instance. When `base_url` is `Some`, the base URI is
/// overridden (used in tests to point at a wiremock server). The builder is
/// fully consumed here -- no non-`Send` state leaks into the caller's async
/// future.
fn build_octocrab(token: &str, base_url: Option<&str>) -> Result<octocrab::Octocrab, RemoteError> {
    let builder = octocrab::OctocrabBuilder::new().personal_token(token);
    let builder = if let Some(url) = base_url {
        builder
            .base_uri(url)
            .map_err(|e| RemoteError::Api(e.to_string()))?
    } else {
        builder
    };
    builder.build().map_err(|e| RemoteError::Api(e.to_string()))
}

/// Map an octocrab error to `RemoteError`. Checks the `status_code` on the
/// inner `GitHubError` when the variant is `GitHub`; falls back to checking
/// the `Display` string for "404" / "Not Found".
fn map_octocrab_err(e: octocrab::Error) -> RemoteError {
    if let octocrab::Error::GitHub { ref source, .. } = e
        && source.status_code.as_u16() == 404
    {
        return RemoteError::NotFound(source.to_string());
    }
    let msg = e.to_string();
    if msg.contains("404") || msg.contains("Not Found") {
        RemoteError::NotFound(msg)
    } else {
        RemoteError::Api(msg)
    }
}

pub(crate) async fn github_fetch_tree(
    token: &str,
    owner: &str,
    repo: &str,
    path: Option<&str>,
    git_ref: Option<&str>,
    depth: u32,
    base_url: Option<&str>,
) -> Result<Vec<RemoteTreeEntry>, RemoteError> {
    use std::collections::{HashSet, VecDeque};

    // depth=0 returns empty list
    if depth == 0 {
        return Ok(Vec::new());
    }

    let octo = build_octocrab(token, base_url)?;

    let mut entries: Vec<RemoteTreeEntry> = Vec::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    // `seen` tracks directory paths that have been queued for traversal.
    // GitLab and GitHub both return canonical relative paths (no trailing
    // slashes, no duplicates within a single level response), so a simple
    // string HashSet is sufficient to prevent re-queuing a directory that
    // appeared as a subdirectory of multiple already-visited parents.
    let mut seen: HashSet<String> = HashSet::new();

    // Start with the root path
    let root_path = path.unwrap_or("").to_string();
    queue.push_back((root_path.clone(), 0));
    seen.insert(root_path.clone());

    while let Some((current_path, current_depth)) = queue.pop_front() {
        let repo_handler = octo.repos(owner, repo);
        let builder = repo_handler.get_content().path(&current_path);
        let builder = if let Some(r) = git_ref {
            builder.r#ref(r)
        } else {
            builder
        };

        match builder.send().await {
            Ok(mut content_items) => {
                let items = content_items.take_items();

                for c in items {
                    entries.push(RemoteTreeEntry {
                        path: c.path.clone(),
                        entry_type: if c.r#type == "dir" {
                            "tree".to_string()
                        } else {
                            "blob".to_string()
                        },
                    });

                    // If we haven't reached max depth and this is a directory, queue it
                    if current_depth + 1 < depth && c.r#type == "dir" && seen.insert(c.path.clone())
                    {
                        queue.push_back((c.path, current_depth + 1));
                    }
                }
            }
            Err(e) => {
                debug!("github_fetch_tree: error fetching {current_path}: {e}");
                return Err(map_octocrab_err(e));
            }
        }
    }

    Ok(entries)
}

pub(crate) async fn github_fetch_file(
    token: &str,
    owner: &str,
    repo: &str,
    path: &str,
    git_ref: Option<&str>,
    base_url: Option<&str>,
) -> Result<RemoteFileOutput, RemoteError> {
    let octo = build_octocrab(token, base_url)?;

    let repo_handler = octo.repos(owner, repo);
    let builder = repo_handler.get_content().path(path);
    let builder = if let Some(r) = git_ref {
        builder.r#ref(r)
    } else {
        builder
    };

    let mut content_items = builder.send().await.map_err(map_octocrab_err)?;

    let items = content_items.take_items();
    let item = items
        .into_iter()
        .next()
        .ok_or_else(|| RemoteError::NotFound(format!("no content found for path: {path}")))?;

    let raw_content = item
        .decoded_content()
        .ok_or_else(|| RemoteError::Api("failed to decode file content".to_string()))?;

    let size_bytes = raw_content.len();
    let resolved_ref = git_ref.unwrap_or("HEAD").to_string();

    Ok(RemoteFileOutput {
        content: raw_content,
        size_bytes,
        resolved_ref,
        path: item.path.clone(),
    })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fetch the directory tree of a remote repository.
///
/// `url` must be a `https://gitlab.com/...` or `https://github.com/...` URL.
/// `GITLAB_TOKEN` / `GITHUB_TOKEN` must be set in the environment.
/// Fetch the directory structure of a remote repository without cloning.
///
/// # Arguments
///
/// * `url` - Full repository URL (e.g., `https://github.com/owner/repo` or `https://gitlab.com/owner/repo`)
/// * `path` - Optional subdirectory path within the repository (defaults to root)
/// * `git_ref` - Optional branch, tag, or commit SHA (defaults to the repository's default branch)
/// * `depth` - Directory traversal depth (1-5; default 2)
///
/// # Returns
///
/// A [`RemoteTreeOutput`] containing the directory structure and file counts.
///
/// # Errors
///
/// Returns [`RemoteError`] if:
/// - The URL is invalid or not a GitHub/GitLab URL
/// - The required authentication token (`GITHUB_TOKEN` or `GITLAB_TOKEN`) is not set
/// - The repository or path does not exist
/// - The API request fails
///
/// # Example
///
/// ```no_run
/// use aptu_coder_remote::fetch_tree;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // Requires GITHUB_TOKEN environment variable
///     let output = fetch_tree(
///         "https://github.com/clouatre-labs/aptu-coder",
///         Some("crates"),
///         None,
///         2,
///     ).await?;
///     println!("{}", output.formatted);
///     Ok(())
/// }
/// ```
pub async fn fetch_tree(
    url: &str,
    path: Option<&str>,
    git_ref: Option<&str>,
    depth: u32,
) -> Result<RemoteTreeOutput, RemoteError> {
    let (platform, owner, repo) = detect_platform(url)?;

    match platform {
        Platform::GitLab { host } => {
            let token =
                std::env::var("GITLAB_TOKEN").map_err(|_| RemoteError::MissingGitLabToken)?;
            let project = format!("{owner}/{repo}");
            let entries = gitlab_fetch_tree(&host, &token, &project, path, git_ref, depth).await?;
            Ok(build_tree_output(entries))
        }
        Platform::GitHub => {
            let token =
                std::env::var("GITHUB_TOKEN").map_err(|_| RemoteError::MissingGitHubToken)?;
            let entries =
                github_fetch_tree(&token, &owner, &repo, path, git_ref, depth, None).await?;
            Ok(build_tree_output(entries))
        }
    }
}

/// Fetch the content of a single file from a remote repository.
///
/// # Arguments
///
/// * `url` - Full repository URL (e.g., `https://github.com/owner/repo` or `https://gitlab.com/owner/repo`)
/// * `path` - File path within the repository (e.g., `src/main.rs`)
/// * `git_ref` - Optional branch, tag, or commit SHA (defaults to the repository's default branch)
/// * `line_range` - Optional line range in `START-END` format (e.g., `10-50`). Both bounds are inclusive and 1-indexed.
///
/// # Returns
///
/// A [`RemoteFileOutput`] containing the file content and metadata.
///
/// # Errors
///
/// Returns [`RemoteError`] if:
/// - The URL is invalid or not a GitHub/GitLab URL
/// - The required authentication token (`GITHUB_TOKEN` or `GITLAB_TOKEN`) is not set
/// - The repository, file, or ref does not exist
/// - The line range format is invalid
/// - The API request fails
///
/// # Example
///
/// ```no_run
/// use aptu_coder_remote::fetch_file;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // Requires GITHUB_TOKEN environment variable
///     let output = fetch_file(
///         "https://github.com/clouatre-labs/aptu-coder",
///         "README.md",
///         None,
///         Some("1-50"),
///     ).await?;
///     println!("{}", output.content);
///     Ok(())
/// }
/// ```
pub async fn fetch_file(
    url: &str,
    path: &str,
    git_ref: Option<&str>,
    line_range: Option<&str>,
) -> Result<RemoteFileOutput, RemoteError> {
    let (platform, owner, repo) = detect_platform(url)?;

    let mut output = match platform {
        Platform::GitLab { host } => {
            let token =
                std::env::var("GITLAB_TOKEN").map_err(|_| RemoteError::MissingGitLabToken)?;
            let project = format!("{owner}/{repo}");
            gitlab_fetch_file(&host, &token, &project, path, git_ref).await?
        }
        Platform::GitHub => {
            let token =
                std::env::var("GITHUB_TOKEN").map_err(|_| RemoteError::MissingGitHubToken)?;
            github_fetch_file(&token, &owner, &repo, path, git_ref, None).await?
        }
    };

    if let Some(range) = line_range {
        let (start, end) = parse_line_range(range)?;
        output.content = slice_lines(&output.content, start, end);
    }

    Ok(output)
}

#[cfg(test)]
mod tests;
