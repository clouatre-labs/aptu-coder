"""Pre-registered constants for the KG pull-ablation benchmark.

All values mirror docs/audit/2026-08-29-kg-pull-ablation-benchmark-design.md
except the provider/model, which is a DECLARED DEVIATION: the design
pre-registered mistral-small-2603, but no Mistral credential exists in this
environment (only ZAI_API_KEY), so sessions run with zai/glm-5.3-flash.
"""

from __future__ import annotations

import itertools
from pathlib import Path

ARMS = ("no-graph", "optional-graph", "forced-graph")
FIXTURES = ("family1-contract-drift", "family2-reference-resolution",
            "family3-pattern-mode", "clean-control")
RUNS_PER_CELL = 2

# v20 fixture packs live in-repo (self-contained crates with
# ground-truth.json), replacing the ephemeral bench-fixture-* branches
# of #1812-#1815.
FIXTURES_DIR = Path(__file__).resolve().parent / "fixtures"

# DECLARED DEVIATION from the pre-registered mistral-small-2603.
PROVIDER = "zai"
MODEL = "glm-5.3-flash"

# Pre-registered v20 ceiling, re-registered from OBSERVED per-session
# costs in docs/audit/2026-11-30-kg-pull-ablation-results.md (USD
# 0.0173-0.0221/session with zai/glm-5.3-flash; the prior USD 0.069 cap
# came from a stale USD 0.0023 estimate and was blown ~8x on one cell).
# 24 runs x USD 0.0221 (observed max) = USD 0.5304, plus ~20%
# operational headroom, rounded to USD 0.64. Per-run kill raised above
# the observed per-session max (USD 0.0221) so a normal session is not
# killed mid-flight: 24 x 0.03 would itself breach the cap, so the
# cumulative cap remains the binding stop rule.
SPEND_CAP_USD = 0.64
PER_RUN_KILL_USD = 0.03

# Pinning (recorded per session in the run manifest).
PINNED_REPO_COMMIT = "bec6b10"

# MCP surface manipulation targets.
HIDDEN_TOOL = "analyze_symbol"
GRAPH_URI_PREFIX = "aptu-coder://graph/"
REJECTED_METHODS = ("resources/list", "resources/templates/list")

# Fixture -> throwaway branch and PR title (draft PRs, never merged).
# Retained for v20 pack provenance records in the run manifest.
FIXTURE_BRANCHES = {
    "family1-contract-drift": "bench-fixture-family1-contract-drift",
    "family2-reference-resolution": "bench-fixture-family2-reference-resolution",
    "family3-pattern-mode": "bench-fixture-family3-pattern-mode",
    "clean-control": "bench-fixture-clean-control",
}

# Defect signatures used by the adjudicator's named-defect rubric: the
# concrete symbol names whose staleness/mismatch constitutes the named
# defect, per fixture. v20 fixture packs (scripts/kg-ablation/fixtures/)
# carry full ground truth in each pack's ground-truth.json; only
# family1 has a named defect. Families 2 and 3 are reference-resolution
# and call-shape pattern targets scored against their reference/pattern
# ground truth, not against the named-defect rubric.
DEFECT_SYMBOLS = {
    "family1-contract-drift": ["normalize_window"],
    "family2-reference-resolution": [],
    "family3-pattern-mode": [],
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
