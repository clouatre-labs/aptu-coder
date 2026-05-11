# aptu-coder-remote

Remote repository exploration tools for GitLab and GitHub, implemented as MCP tools.

Part of the [aptu-coder](https://github.com/clouatre-labs/aptu-coder) project.

## Tools

### `remote_tree`

List directory structure and file counts for a remote repository without cloning it.
Platform is auto-detected from the URL host (`gitlab.com` uses the `gitlab` crate,
`github.com` uses `octocrab`).

**Parameters:**
- `url` (required): Full repository URL, e.g. `https://gitlab.com/owner/repo`
- `path` (optional): Subdirectory path. Defaults to root.
- `ref` (optional): Branch, tag, or commit SHA. Defaults to the default branch.
- `depth` (optional): Directory traversal depth, 1-5. Default 2.

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

## License

MIT OR Apache-2.0
