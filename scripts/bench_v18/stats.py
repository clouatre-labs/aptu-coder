#!/usr/bin/env python3
"""Exact paired statistics for the v18 analysis step.

scipy is allowed here only; a pure-Python fallback exists for the sign
test so the module works on a bare stdlib interpreter.
"""

from __future__ import annotations

import random
from math import comb

try:  # scipy is optional; fallbacks below keep this stdlib-safe
    from scipy import stats as _scipy_stats
except ImportError:  # pragma: no cover - exercised implicitly on CI images
    _scipy_stats = None


def sign_test(deltas: list[float]) -> tuple[float, float]:
    """Exact two-sided sign test over non-ties. Returns (wins, p)."""
    wins = sum(1 for d in deltas if d > 0)
    n = sum(1 for d in deltas if d != 0)
    if n == 0:
        return 0, 1.0
    tail = sum(comb(n, k) for k in range(0, min(wins, n - wins) + 1))
    p = min(1.0, 2.0 * tail / 2 ** n)
    if _scipy_stats is not None:
        p = float(_scipy_stats.binomtest(wins, n, 0.5).pvalue)
    return wins, p


def wilcoxon_signed_rank(deltas: list[float]) -> tuple[float, float]:
    """Wilcoxon signed-rank over non-zero deltas (scipy; None if absent)."""
    nonzero = [d for d in deltas if d != 0]
    if not nonzero or _scipy_stats is None:
        return 0.0, float("nan")
    stat, p = _scipy_stats.wilcoxon(nonzero)
    return float(stat), float(p)


def rank_biserial(deltas: list[float]) -> float:
    """Rank-biserial correlation for paired non-zero deltas."""
    nonzero = sorted((d for d in deltas if d != 0), key=abs)
    n = len(nonzero)
    if n == 0:
        return 0.0
    pos = sum(rank + 1 for rank, d in enumerate(nonzero) if d > 0)
    return 2.0 * pos / (n * (n + 1)) - 1.0


def bootstrap_ci(
    values: list[float],
    stat=float,
    iters: int = 10_000,
    alpha: float = 0.05,
    seed: int = 1604,
) -> tuple[float, float]:
    """Percentile bootstrap CI for `stat(values)`."""
    if not values:
        return float("nan"), float("nan")
    rng = random.Random(seed)
    n = len(values)
    samples = sorted(
        stat([values[rng.randrange(n)] for _ in range(n)])
        for _ in range(iters)
    )
    lo = samples[int((alpha / 2) * iters)]
    hi = samples[min(iters - 1, int((1 - alpha / 2) * iters))]
    return lo, hi
