# Security Policy

This project has earned the [OpenSSF Best Practices silver badge](https://www.bestpractices.dev/projects/12275/silver).

## Reporting

Please report vulnerabilities privately via [GitHub's private vulnerability reporting](https://github.com/clouatre-labs/aptu-coder/security/advisories/new).

Do not open public issues for sensitive matters.

## Branch Protection

The `main` branch is protected by GitHub rulesets with the following rules:

- **Required Status Checks**: All CI checks must pass before merging
- **Signed Commits**: All commits must be signed (GPG or S/MIME)
- **No Force Push**: History cannot be rewritten on main
- **No Deletion**: The main branch cannot be deleted

## Reporter credit

Reporters who disclose vulnerabilities responsibly will be credited in the release notes for the fixing release, unless they request anonymity.

## Response SLA

| Severity | Acknowledgement | Remediation target |
|---|---|---|
| Critical / High | Within 72 hours | Within 14 days |
| Medium / Low | Within 72 hours | Next regular release cycle |

## Verifying release signatures

Release artifacts are signed with [cosign](https://docs.sigstore.dev/cosign/overview/) using keyless signing. A `.bundle` file is published alongside each release tarball on the [releases page](https://github.com/clouatre-labs/aptu-coder/releases).

To verify a release artifact:

```bash
cosign verify-blob \
  --bundle aptu-coder-<version>-<target>.tar.gz.bundle \
  --certificate-identity-regexp 'https://github.com/clouatre-labs/aptu-coder/.github/workflows/release.yml' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  aptu-coder-<version>-<target>.tar.gz
```

Replace `<version>` with the release version and `<target>` with the target triple (e.g., `x86_64-unknown-linux-musl`).

## Supply Chain Integrity

- New Cargo.lock dependencies must be at least 7 days old at the time of addition (enforced by CI; bypass with `SKIP_PACKAGE_AGE_CHECK=true` for urgent security patches).
- OpenSSF Scorecard runs weekly and uploads results to the GitHub Security tab as SARIF.
