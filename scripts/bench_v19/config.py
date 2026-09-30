"""Shared v19 wait-timeout configuration.

Single owner of the v19 SESSION_WAIT_TIMEOUT_S override (A7b carry-over
consolidation): the 300 s default, the SESSION_WAIT_TIMEOUT_S env
resolution, and the one-shot assignment on the imported v18 module
global. bench_v18/runner.py is never edited (frozen doctrine); the v18
module global remains the effective runtime knob consumed by
meter_and_close and all tests.
"""

from __future__ import annotations

import os

from bench_v18 import runner as v18

DEFAULT_WAIT_TIMEOUT_S = 300


def resolve_wait_timeout_s() -> int:
    """Return the env-configured wait timeout, defaulting to 300 s.

    Mirrors the prior inline int(os.environ.get(...)) behavior: a
    non-integer env value raises ValueError (fail closed).
    """
    return int(os.environ.get("SESSION_WAIT_TIMEOUT_S", str(DEFAULT_WAIT_TIMEOUT_S)))


def apply_wait_timeout() -> int:
    """Assign the resolved timeout onto the v18 module global once.

    Must be invoked at import time by every v19 entrypoint so the
    smoke driver and the pilot ladder cannot silently run at the v18
    120 s default. Returns the applied value for convenience.
    """
    timeout_s = resolve_wait_timeout_s()
    v18.SESSION_WAIT_TIMEOUT_S = timeout_s
    return timeout_s
