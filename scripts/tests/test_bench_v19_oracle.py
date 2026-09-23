"""Offline tests for the bench_v19 ripgrep oracle (rg required on PATH)."""

from __future__ import annotations

import pytest
from bench_v19 import oracle


@pytest.fixture()
def repo(tmp_path):
    """Mini snapshot tree: definition, real caller, comment mention, doc."""
    (tmp_path / "lib.py").write_text("def target_symbol(x):\n    return x\n")
    (tmp_path / "caller.py").write_text("print(target_symbol(1))\n")
    (tmp_path / "mentions.py").write_text("# target_symbol in a comment\n")
    (tmp_path / "docs.md").write_text("See target_symbol here.\n")
    return tmp_path


def test_hop1_excludes_definition_and_comment_and_doc_mentions(repo):
    assert oracle.callers_oracle(repo, "target_symbol", 1) == ["caller.py"]


def test_metachar_and_dash_symbols_do_not_corrupt_matches(repo):
    (repo / "uses_dash.py").write_text("x = dash_symbol_value\n")
    (repo / "dash_use.py").write_text("import lib  # uses -dash-symbol\n")
    # Metacharacters match literally (fixed-string), not as a pattern.
    assert oracle.callers_oracle(repo, "foo(", 1) == []
    assert oracle.callers_oracle(repo, "-dash", 1) == ["dash_use.py"]


def test_rejects_invalid_symbols(repo):
    for bad in ["", "  ", "a\nb", "a\tb"]:
        with pytest.raises(ValueError, match="symbol"):
            oracle.callers_oracle(repo, bad, 1)


def test_build_callers_oracle_hop_depths(repo):
    entries = oracle.build_callers_oracle(repo, "target_symbol")
    assert [e["hop_depth"] for e in entries] == [1, 2, 3]
    assert entries[0]["expected_files"] == ["caller.py"]
