"""Offline tests for the bench_v19 tree-sitter oracle (rg cross-check arm)."""

from __future__ import annotations

import pytest
from bench_v19 import oracle


@pytest.fixture()
def py_repo(tmp_path):
    """Python fixture: definition, real caller, comment, docstring mention."""
    (tmp_path / "lib.py").write_text("def target_symbol(x):\n    return x\n")
    (tmp_path / "caller.py").write_text(
        '"""Uses target_symbol, but only in a docstring."""\n'
        "from lib import target_symbol\n\n"
        "print(target_symbol(1))\n"
    )
    (tmp_path / "mentions.py").write_text("# target_symbol in a comment\n")
    (tmp_path / "strings.py").write_text('note = "target_symbol in a string"\n')
    (tmp_path / "docs.md").write_text("See target_symbol here.\n")
    return tmp_path


@pytest.fixture()
def rs_repo(tmp_path):
    """Rust fixture: definition, real caller, comment mention."""
    (tmp_path / "lib.rs").write_text("pub fn target_symbol(x: u32) -> u32 { x }\n")
    (tmp_path / "caller.rs").write_text(
        "// target_symbol mentioned in a comment\npub fn main() { target_symbol(1); }\n"
    )
    return tmp_path


def test_python_call_edges_exclude_comments_docstrings_strings_defs(py_repo):
    assert oracle.callers_oracle(py_repo, "target_symbol", 1) == ["caller.py"]
    edges = oracle.build_call_edges(py_repo)
    assert ("caller.py", "target_symbol") in edges
    assert ("mentions.py", "target_symbol") not in edges
    assert ("strings.py", "target_symbol") not in edges


def test_rust_call_edges_exclude_comments_and_definitions(rs_repo):
    assert oracle.callers_oracle(rs_repo, "target_symbol", 1) == ["caller.rs"]
    index = oracle.build_definition_index(rs_repo)
    assert index["target_symbol"] == ["lib.rs"]


def test_hop2_bfs_over_three_file_call_chain(tmp_path):
    # a.py defines sym; b.py calls sym; c.py calls a function defined in b.py.
    (tmp_path / "a.py").write_text("def sym():\n    return 1\n")
    (tmp_path / "b.py").write_text("def middle():\n    return sym()\n")
    (tmp_path / "c.py").write_text("from b import middle\n\nmiddle()\n")
    assert oracle.callers_oracle(tmp_path, "sym", 1) == ["b.py"]
    assert oracle.callers_oracle(tmp_path, "sym", 2) == ["b.py", "c.py"]
    assert oracle.callers_oracle(tmp_path, "sym", 3) == ["b.py", "c.py"]


def test_hop_depth_validation_and_symbol_validation(tmp_path):
    for bad_hop in (0, 4):
        with pytest.raises(ValueError, match="hop depth"):
            oracle.callers_oracle(tmp_path, "sym", bad_hop)
    for bad in ["", "  ", "a\nb", "a\tb"]:
        with pytest.raises(ValueError, match="symbol"):
            oracle.callers_oracle(tmp_path, bad, 1)


def test_crosscheck_agreement_and_literal_rg_arm(tmp_path):
    # Clean repo: rg's literal file set equals the tree-sitter caller set.
    (tmp_path / "lib.py").write_text("def target_symbol(x):\n    return x\n")
    (tmp_path / "caller.py").write_text("target_symbol(1)\n")
    assert oracle.crosscheck_agrees(tmp_path, "target_symbol", ["caller.py"])
    # Any divergence (empty expected set) drops agreement.
    assert not oracle.crosscheck_agrees(tmp_path, "target_symbol", [])
    # Literal-safe: metacharacters never corrupt the match.
    assert oracle.rg_caller_files(tmp_path, "foo(") == set()


def test_disagreeing_crosscheck_drops_task(py_repo):
    # rg also matches the comment/string/docstring files -> hop-1
    # disagreement -> the task generator must drop every Track A task.
    entries = oracle.build_callers_oracle(py_repo, "target_symbol")
    assert not entries[0]["crosscheck_agrees"]
    from bench_v19.generate_tasks import generate_tasks

    # The hop-1 disagreement verdict drops every Track A task.
    assert generate_tasks(entries) == []


def test_build_callers_oracle_entries_and_lookup(tmp_path):
    (tmp_path / "a.py").write_text("def sym():\n    return 1\n")
    (tmp_path / "b.py").write_text("sym()\n")
    entries = oracle.build_callers_oracle(tmp_path, "sym")
    assert [e["hop_depth"] for e in entries] == [1, 2, 3]
    assert entries[0]["expected_files"] == ["b.py"]
    assert entries[0]["crosscheck_agrees"]
    lookup = oracle.build_lookup_oracle(tmp_path, "sym")
    assert lookup["expected_files"] == ["a.py"]
    assert lookup["track"] == "C"
