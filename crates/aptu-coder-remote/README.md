<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 aptu-coder contributors -->

# aptu-coder-remote

Remote repository exploration tools for GitLab and GitHub, implemented as MCP tools.

Part of the [aptu-coder](https://github.com/clouatre-labs/aptu-coder) project.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
aptu-coder-remote = "*"
```

## Tools

### `remote_tree`

List directory structure and file counts for a remote repository without cloning it.
Platform is auto-detected from the URL host (`gitlab.com` uses the `gitlab` crate,
`github.com` uses `octocrab`).

**Parameters:**
- `url` (required): Full repository URL, e.g. `https://gitlab.com/owner/repo`
- `path` (optional): Subdirectory path. Defaults to root.
- `ref` (optional): Branch, tag, or commit SHA. Defaults to the default branch.
- `depth` (optional): Directory traversal depth. Default 2.

**Output:** Compact summary matching `analyze_directory summary=true` format.

### `remote_file`

Fetch raw file content from a remote repository at a given ref, with optional line range
to keep context cost low.

**Parameters:**
- `url` (required): Full repository URL
- `path` (required): File path within the repository, e.g. `src/main.rs`
- `ref` (optional): Branch, tag, or commit SHA. Defaults to the default branch.
- `line_range` (optional): Line slice in `START-END` format, e.g. `10-50`.

## Authentication

Tokens are read from environment variables at call time, never stored:

- `GITLAB_TOKEN` for GitLab repositories
- `GITHUB_TOKEN` for GitHub repositories

## Usage Examples

### Fetch a repository tree

```rust,no_run
use aptu_coder_remote::fetch_tree;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Requires GITHUB_TOKEN environment variable
    let output = fetch_tree(
        "https://github.com/clouatre-labs/aptu-coder",
        Some("crates"),
        None,
        2,
    ).await?;
    
    println!("{}", output.formatted);
    Ok(())
}
```

### Fetch a file with line range

```rust,no_run
use aptu_coder_remote::fetch_file;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Requires GITHUB_TOKEN environment variable
    let output = fetch_file(
        "https://github.com/clouatre-labs/aptu-coder",
        "README.md",
        None,
        Some("1-50"),
    ).await?;
    
    println!("{}", output.content);
    Ok(())
}
```

## API Reference

For detailed API documentation, see [docs.rs](https://docs.rs/aptu-coder-remote/).

## License

Apache-2.0
