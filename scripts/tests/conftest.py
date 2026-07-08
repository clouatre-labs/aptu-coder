"""pytest configuration for scripts/tests.

Adds the scripts/ directory to sys.path so that test modules can locate
mcp-metrics.py via importlib without manual path manipulation in each file.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))
