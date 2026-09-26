#!/usr/bin/env python3
"""Snapshot fetch and verify for the v19 benchmark.

fetch (network) and verify (pure-local, fail-closed) are separable. The
manifest below is frozen from day one: pinned commits and expected tarball
SHA256 digests are never re-pinned, and verify_snapshot trusts only this
checked-in manifest -- never caller-supplied JSON.

SSRF hardening: fetching uses an OpenerDirector whose redirect handler
rejects every redirect EXCEPT a single hop to a pinned archive host
(`codeload.github.com`, the only target GitHub's own tarball endpoint
302s to). A fully-open redirect chain could otherwise land on an
internal or cloud-metadata endpoint; the pinned single-hop allowlist
closes that hole while keeping the frozen GitHub tarball URLs valid.
"""

from __future__ import annotations

import hashlib
import json
import urllib.error
import urllib.request
from pathlib import Path

FETCH_TIMEOUT_S = 60
CHUNK_SIZE = 1 << 20

SNAPSHOT_MANIFEST = {
    "django": {
        "repo": "django/django",
        "commit": "dd6f6b1531984823e3dc56740dfa93f3ceb09357",
        "tarball_sha256": (
            "9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e"
        ),
    },
    "rust-clippy": {
        "repo": "rust-lang/rust-clippy",
        "commit": "f0c668aa82a404de25d4f331210e018b3eb8feed",
        "tarball_sha256": (
            "5c5a5653d02c8b99194d194b2619c596da755922336da00757404839cbd00744"
        ),
    },
}

ARCHIVE_HOST = "codeload.github.com"
ALLOWED_HOST = "github.com"
ALLOWED_REPO_PREFIXES = tuple(
    f"https://{ALLOWED_HOST}/{entry['repo']}/archive/"
    for entry in SNAPSHOT_MANIFEST.values()
)


def snapshot_sha256(entry: dict) -> str:
    """Stable manifest key: SHA256 over 'repo@commit' (not file bytes)."""
    return hashlib.sha256(f"{entry['repo']}@{entry['commit']}".encode()).hexdigest()


def tarball_url(entry: dict) -> str:
    return f"https://{ALLOWED_HOST}/{entry['repo']}/archive/{entry['commit']}.tar.gz"


def validate_tarball_url(url: str) -> None:
    """Fail closed unless url is an allowlisted frozen tarball URL."""
    if not url.startswith(ALLOWED_REPO_PREFIXES):
        raise RuntimeError(f"refusing non-allowlisted tarball URL: {url}")


class _PinnedRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Allow only one hop, from the allowlisted host to the pinned
    archive host; anything else is SSRF (no arbitrary post-allowlist
    hops)."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if req.host == ALLOWED_HOST and newurl.startswith(
            f"https://{ARCHIVE_HOST}/"
        ):
            return urllib.request.Request(
                newurl, headers={k: v for k, v in req.headers.items() if k.lower() != "host"},
                origin_req_host=req.origin_req_host,
                unverifiable=True,
            )
        raise urllib.error.HTTPError(
            req.full_url, code, "redirect blocked (not the pinned archive hop)", headers, fp
        )


def _opener() -> urllib.request.OpenerDirector:
    return urllib.request.build_opener(_PinnedRedirectHandler())


def fetch_snapshot(name: str, dest_dir: Path) -> Path:
    """Download one snapshot tarball (allowlisted URL, no redirects, timeout)."""
    entry = SNAPSHOT_MANIFEST[name]
    url = tarball_url(entry)
    validate_tarball_url(url)
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest = dest_dir / f"{name}-{entry['commit']}.tar.gz"
    with (
        _opener().open(url, timeout=FETCH_TIMEOUT_S) as resp,
        dest.open("wb") as out,
    ):
        while chunk := resp.read(CHUNK_SIZE):
            out.write(chunk)
    return dest


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(CHUNK_SIZE), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_snapshot(name: str, dest_dir: Path) -> None:
    """Fail-closed local verification against the frozen manifest digest."""
    entry = SNAPSHOT_MANIFEST[name]
    expected = entry["tarball_sha256"]
    dest = dest_dir / f"{name}-{entry['commit']}.tar.gz"
    if not dest.is_file():
        raise RuntimeError(f"snapshot {name}: {dest} not found")
    actual = _file_sha256(dest)
    if actual != expected:
        raise RuntimeError(f"snapshot {name}: sha256 mismatch ({actual} != {expected})")


def write_manifest(dest: Path) -> None:
    """Write the frozen manifest (repo/commit/key plus expected digest)."""
    payload = {
        name: {
            "repo": e["repo"],
            "commit": e["commit"],
            "key": snapshot_sha256(e),
            "tarball_sha256": e["tarball_sha256"],
        }
        for name, e in SNAPSHOT_MANIFEST.items()
    }
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--fetch",
        action="store_true",
        help="download snapshots (network); default is verify-only",
    )
    ap.add_argument("--dest-dir", type=Path, required=True)
    args = ap.parse_args()
    for name in SNAPSHOT_MANIFEST:
        if args.fetch:
            print(f"fetched {fetch_snapshot(name, args.dest_dir)}")
        verify_snapshot(name, args.dest_dir)
        print(f"verified {name}")


if __name__ == "__main__":
    main()
