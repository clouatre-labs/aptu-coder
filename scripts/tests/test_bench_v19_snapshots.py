"""Offline tests for bench_v19 snapshots (verify path; no network)."""

from __future__ import annotations

import hashlib
import urllib.request

import pytest
from bench_v19 import snapshots


def _fake_tarball(tmp_path, name):
    """Local tarball fixture whose digest matches nothing in the manifest."""
    entry = dict(snapshots.SNAPSHOT_MANIFEST[name])
    dest_dir = tmp_path / "snapshots"
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest = dest_dir / f"{name}-{entry['commit']}.tar.gz"
    dest.write_bytes(f"payload-{name}".encode())
    entry["tarball_sha256"] = hashlib.sha256(dest.read_bytes()).hexdigest()
    return dest_dir, entry


@pytest.mark.parametrize("name", sorted(snapshots.SNAPSHOT_MANIFEST))
def test_verify_succeeds_against_matching_local_tree(tmp_path, name, monkeypatch):
    dest_dir, entry = _fake_tarball(tmp_path, name)
    monkeypatch.setitem(snapshots.SNAPSHOT_MANIFEST, name, entry)
    snapshots.verify_snapshot(name, dest_dir)


@pytest.mark.parametrize("name", sorted(snapshots.SNAPSHOT_MANIFEST))
def test_verify_fails_closed_on_mismatch_or_missing(tmp_path, name):
    dest_dir, _ = _fake_tarball(tmp_path, name)
    # Manifest digest is authoritative; caller-supplied digests are ignored.
    with pytest.raises(RuntimeError, match="sha256 mismatch"):
        snapshots.verify_snapshot(name, dest_dir)
    with pytest.raises(RuntimeError, match="not found"):
        snapshots.verify_snapshot(name, tmp_path / "empty")


def test_manifest_pins_frozen_commits_and_tarball_digests():
    assert snapshots.SNAPSHOT_MANIFEST["django"] == {
        "repo": "django/django",
        "commit": "dd6f6b1531984823e3dc56740dfa93f3ceb09357",
        "tarball_sha256": (
            "9dc904f5a45be0eea03268642001e4054a7f83faacf546a69fc1a69b092d1e5e"
        ),
    }
    assert snapshots.SNAPSHOT_MANIFEST["rust-clippy"]["commit"] == (
        "f0c668aa82a404de25d4f331210e018b3eb8feed"
    )


def test_tarball_url_allowlist_rejects_foreign_urls():
    for entry in snapshots.SNAPSHOT_MANIFEST.values():
        snapshots.validate_tarball_url(snapshots.tarball_url(entry))
    for bad in [
        "https://evil.example/django/django/archive/x.tar.gz",
        "http://github.com/django/django/archive/x.tar.gz",
        "https://github.com/attacker/repo/archive/x.tar.gz",
    ]:
        with pytest.raises(RuntimeError, match="allowlist"):
            snapshots.validate_tarball_url(bad)


def test_fetch_snapshot_streams_chunks_with_timeout(tmp_path, monkeypatch):
    class FakeResponse:
        def __init__(self):
            self._chunks = [b"aaa", b"bbb", b""]

        def read(self, size=-1):
            return self._chunks.pop(0)

        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

    seen = {}

    def fake_urlopen(url, timeout=None):
        seen.update(url=url, timeout=timeout)
        snapshots.validate_tarball_url(url)
        return FakeResponse()

    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)
    dest = snapshots.fetch_snapshot("django", tmp_path / "out")
    assert dest.read_bytes() == b"aaabbb"
    assert seen["url"] == snapshots.tarball_url(snapshots.SNAPSHOT_MANIFEST["django"])
    assert seen["timeout"] == snapshots.FETCH_TIMEOUT_S
