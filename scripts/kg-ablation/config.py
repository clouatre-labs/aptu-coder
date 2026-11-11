"""Pre-registered constants for the KG pull-ablation benchmark.

All values mirror docs/audit/2026-08-29-kg-pull-ablation-benchmark-design.md
except the provider/model, which is a DECLARED DEVIATION: the design
pre-registered mistral-small-2603, but no Mistral credential exists in this
environment (only ZAI_API_KEY), so sessions run with zai/glm-5.3-flash.
"""

from __future__ import annotations

import itertools

ARMS = ("no-graph", "optional-graph", "forced-graph")
FIXTURES = ("broken-caller", "dead-code-path", "wrong-trait-impl", "clean-control")
RUNS_PER_CELL = 2

# DECLARED DEVIATION from the pre-registered mistral-small-2603.
PROVIDER = "zai"
MODEL = "glm-5.3-flash"

# Pre-registered 24-run ceiling: 24 x USD 0.0023 + 25% operational headroom.
SPEND_CAP_USD = 0.069
PER_RUN_KILL_USD = 0.004

# Pinning (recorded per session in the run manifest).
PINNED_REPO_COMMIT = "bec6b10"

# MCP surface manipulation targets.
HIDDEN_TOOL = "analyze_symbol"
GRAPH_URI_PREFIX = "aptu-coder://graph/"
REJECTED_METHODS = ("resources/list", "resources/templates/list")

# Fixture -> throwaway branch and PR title (draft PRs, never merged).
FIXTURE_BRANCHES = {
    "broken-caller": "bench-fixture-broken-caller",
    "dead-code-path": "bench-fixture-dead-code-path",
    "wrong-trait-impl": "bench-fixture-wrong-trait-impl",
    "clean-control": "bench-fixture-clean-control",
}

# Defect signatures used by the adjudicator's named-defect rubric: the
# concrete symbol names whose staleness/mismatch constitutes the named
# defect, per fixture.
DEFECT_SYMBOLS = {
    "broken-caller": ["validate_path_relative_to"],
    "dead-code-path": ["decode_offset"],
    "wrong-trait-impl": ["get_ts_language"],
    "clean-control": [],
}

# Session timeout and pi invocation pins.
SESSION_TIMEOUT_S = 300
PI_COMMON_FLAGS = ["--no-skills", "--no-context-files"]


def session_matrix() -> list[tuple[str, str, int]]:
    """All 24 (arm, fixture, run) cells in canonical order."""
    return list(
        itertools.product(ARMS, FIXTURES, range(1, RUNS_PER_CELL + 1))
    )


ARM_PROMPT_SUFFIX = {
    "no-graph": "",
    "optional-graph": (
        " Graph context tools and resources may be available on request; "
        "use them only if you judge them useful."
    ),
    "forced-graph": (
        " Before reviewing, you MUST call the analyze_symbol tool on the "
        "primary changed symbol and read the graph resources relevant to "
        "the change (blast radius / bidirectional edges), then incorporate "
        "all returned pages into your analysis."
    ),
}


def review_prompt(fixture_pr: int, arm: str) -> str:
    """Pinned review prompt for one cell (arm suffix differs by arm)."""
    base = (
        f"You are reviewing draft PR #{fixture_pr} in the current "
        "repository. Fetch its diff with `gh pr diff "
        f"{fixture_pr}` and review the changed code. Identify any "
        "concrete defects: broken callers, dangling calls, contract "
        "mismatches, or other real breakage introduced by the diff. "
        "For each finding, state the file, the symbol, and the exact "
        "problem. If you find no defects, say so explicitly."
        + ARM_PROMPT_SUFFIX[arm]
    )
    return base
