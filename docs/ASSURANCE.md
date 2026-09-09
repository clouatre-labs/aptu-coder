# Assurance Case

This document provides the security assurance case for aptu-coder.

## What the tool does

aptu-coder is a static analysis server that parses source files using tree-sitter and returns structured code metadata (functions, classes, imports, call graphs) over the MCP protocol. **Analyzed code is never executed.** The server performs read-only operations on local files.

## Trust boundaries

| Boundary | Direction | Description |
|---|---|---|
| Local file system | Inbound | Source files provided by the MCP client via `path` arguments |
| stdio (MCP protocol) | Bidirectional | JSON-RPC requests from the client; JSON responses from the server |
| Streamable HTTP listener | Inbound | When `--port` or `APTU_CODER_PORT` is set, the server binds to `127.0.0.1:PORT` and serves MCP requests over HTTP |
| OTLP exporter | Outbound | When `OTEL_EXPORTER_OTLP_ENDPOINT` is set, the server exports traces, logs, and metrics to the configured collector endpoint via OTLP/HTTP |

In the default stdio transport mode, the server makes no outbound network calls and holds no credentials. "No persistent state" is scoped to no database/session store: an always-on JSONL metrics writer (`MetricsWriter`, spawned unconditionally in `crates/aptu-coder/src/main.rs`) appends metrics files under `$XDG_DATA_HOME/aptu-coder/`, and `analyze_symbol`'s L2 disk cache (`crates/aptu-coder-core/src/analyze_focused.rs`, keyed by canonical path + git HEAD SHA) is enabled by default under the same XDG data directory, opt-out only via `APTU_CODER_DISK_CACHE_DISABLED=1`. See [ARCHITECTURE.md](ARCHITECTURE.md) for the full data flow.

**Outbound network calls:** In the default stdio transport, the server makes no outbound network calls. When `OTEL_EXPORTER_OTLP_ENDPOINT` is set, the OpenTelemetry exporter sends traces, logs, and metrics to the configured collector endpoint via OTLP/HTTP.

**Network listener:** In the default stdio transport, there is no network listener. When `--port` or `APTU_CODER_PORT` is set, the server binds to `127.0.0.1:PORT` and serves MCP requests over the Streamable HTTP transport.

## Attack surface

The only meaningful attack surface is **malformed or adversarially crafted source files** fed to tree-sitter. tree-sitter performs error recovery by design and never panics or executes the parsed content. The server does not eval, exec, or interpret any analyzed file.

There is no database, no deserialization of untrusted network data, and no privilege escalation path. In stdio mode, there is no network listener. When Streamable HTTP is enabled (`--port` or `APTU_CODER_PORT`), the listener binds to `127.0.0.1` only (loopback, not `0.0.0.0`).

## Common weaknesses countered

| Weakness | Status |
|---|---|
| SQL injection | Not applicable -- no database |
| Shell injection | Not applicable -- no shell exec in analysis tools; `exec_command` is intentionally separate and runs only when explicitly called |
| Deserialization of untrusted data | Not applicable -- no network input; only local files parsed by tree-sitter |
| Network I/O vulnerabilities | Not applicable in stdio mode (default). When Streamable HTTP is enabled, the listener is bound to `127.0.0.1` only. When OTLP export is enabled, outbound HTTP is limited to the configured collector endpoint. |
| Credential exposure | Not applicable -- no credentials or secrets handled |

## Supply chain hardening

Existing mechanisms (not duplicated here):

- `cargo deny` in CI: audits transitive dependencies for known advisories, license compliance, and dependency ban/duplicate-version rules (`cargo deny check advisories licenses bans`)
- Renovate: automated dependency update pull requests
- SLSA provenance: build provenance attestations published alongside each release
- cosign: release artifacts are signed with keyless signing; see [SECURITY.md](../SECURITY.md) for verification instructions
- GPG-signed commits: all commits to main are GPG-signed
- Secret scanning and workflow auditing: `.github/workflows/security.yml` runs a single "Security Result" job on every PR and push to main -- TruffleHog (`trufflesecurity/trufflehog` v3.97.4, `--only-verified`) for committed-secret scanning, and zizmor (`zizmorcore/zizmor-action` v0.6.3, `min-severity: medium`, SARIF upload) for GitHub Actions workflow auditing. The zizmor step is path-gated to run only when workflow files change. `.github/workflows/scheduled-security-audit.yml` re-runs zizmor weekly (`cron: '0 2 * * 1'`) independent of any diff, since CVE and Actions-advisory data changes without code commits.

## Site hardening

The project website and repository are hosted on GitHub (https://github.com/clouatre-labs/aptu-coder). GitHub enforces the following hardening headers by default, verified with `curl -sI`:

- `strict-transport-security: max-age=31536000; includeSubdomains; preload`
- `x-frame-options: deny`
- `x-content-type-options: nosniff`
- `content-security-policy: default-src 'none'` (comprehensive policy)

The project distribution channels (crates.io, Homebrew tap) are third-party platforms with established security postures; the project has no control over their headers.

## Security review

- **Review date:** 2026-03-29
- **Scope:** Full codebase, trust boundaries, attack surface, and supply chain (as documented in this file)
- **Conclusion:** No exploitable vulnerabilities identified; residual risks documented above
- **Reviewer:** Project maintainer (self-review; acceptable for solo projects under OpenSSF criteria)
- **Note:** This review predates material changes to the CI security posture made between 2026-03-29 and 2026-09-09 -- the secret scanner switched from gitleaks to TruffleHog, secrets and zizmor scanning were consolidated into one Security Result job, the `cargo deny` check gained the `bans` rule, and path-gating was added for the zizmor step and semver-checks (see commits 2271ee7, 4d674f7, f1d52ca, bd56109). Those changes have not been covered by a re-review; treat this review as scoped to the state of the codebase on 2026-03-29 and re-run or extend it to cover the delta before relying on it for anything past that date.
