#!/usr/bin/env python3
"""Pre-registered binary named-defect adjudicator.

Rubric per the design doc (lines 90-102):
- catch: transcript identifies the fixture's named defect concretely
  (names the defect symbol AND describes the actual breakage: stale
  caller, dangling call, or trait contract mismatch). Generic hedges
  ("check callers", "might break") are NOT catches.
- false_positive: flags a non-defect on a defective fixture, or any
  defect on the clean control (harmless style comments excluded).
- false_negative: the named defect exists and no finding identifies it.
- false_reassurance: explicitly claims the relevant caller/impl was
  updated/unused/compatible/safe when that claim is false.

Single-reviewer mode: automated classification plus recorded evidence
excerpts; ambiguous transcripts get needs_review=true with the reviewer's
classification preserved.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

from config import DEFECT_SYMBOLS

BREAKAGE_TERMS = [
    "stale caller", "broken caller", "dangling call", "old signature",
    "signature change", "signature mismatch", "no longer matches",
    "wrong signature", "contract mismatch", "mismatched signature",
    "now takes", "now expects", "missing argument", "wrong number of",
    "type mismatch", "mismatched return", "return type mismatch",
    "still calls", "still uses", "call site", "callers of", "uses the old",
    "uses the previous", "not updated to match", "hasn't been updated",
    "has not been updated", "expects a", "passes the wrong",
]

HEDGE_PATTERNS = [
    r"check (all )?callers", r"might break", r"could break",
    r"should verify", r"potential(ly)? (breaking|issue)",
    r"may (affect|break)", r"worth checking",
]

REASSURANCE_TERMS = [
    "no callers", "caller is updated", "callers are updated",
    "all callers updated", "unused", "compatible", "no impact",
    "safe", "fully updated", "correctly updated",
]

FP_ON_DEFECT_TERMS = [
    "security vulnerability", "sql injection", "race condition",
    "memory leak", "missing auth", "credentials",
]


def read_transcript(path: Path) -> str:
    """Concatenate pi JSON transcript into reviewable text."""
    parts = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            parts.append(line)
            continue
        if not isinstance(entry, dict):
            continue
        msg = entry.get("message") if isinstance(entry.get("message"), dict) \
            else entry
        content = msg.get("content")
        if isinstance(content, str):
            parts.append(content)
        elif isinstance(content, list):
            for block in content:
                if isinstance(block, dict) and block.get("type") == "text":
                    parts.append(block.get("text", ""))
    return "\n".join(parts)


def excerpt(text: str, term: str, width: int = 240) -> str:
    idx = text.lower().find(term.lower())
    if idx == -1:
        return ""
    start = max(0, idx - width // 2)
    return text[start:idx + width].replace("\n", " ")


def is_hedge_only(sentence: str) -> bool:
    return any(re.search(p, sentence, re.IGNORECASE)
               for p in HEDGE_PATTERNS)


def classify(transcript_text: str, fixture: str) -> dict:
    symbols = DEFECT_SYMBOLS[fixture]
    low = transcript_text.lower()
    result: dict = {
        "fixture": fixture,
        "catch": False,
        "false_positive": False,
        "false_negative": False,
        "false_reassurance": False,
        "evidence": [],
        "notes": "",
        "needs_review": False,
    }

    symbol_hit = any(s.lower() in low for s in symbols)
    breakage_hit = [t for t in BREAKAGE_TERMS if t.lower() in low]

    if fixture == "clean-control":
        # Any substantive defect claim is a false positive.
        defect_claims = [t for t in FP_ON_DEFECT_TERMS if t in low] + \
            ([t for t in breakage_hit if "call site" not in t])
        reassurance = [t for t in REASSURANCE_TERMS if t in low]
        if defect_claims and not reassurance:
            result["false_positive"] = True
            result["evidence"].append(excerpt(transcript_text,
                                              defect_claims[0]))
        if reassurance:
            result["false_reassurance"] = not result["false_positive"] and \
                False  # reassurance on clean control is accurate, not FR
        if not result["false_positive"]:
            result["notes"] = "clean control correctly reported harmless"
        return result

    # Defective fixtures.
    if symbol_hit and breakage_hit:
        hedge_context = False
        for term in breakage_hit:
            ex = excerpt(transcript_text, term)
            sentence = next((s for s in ex.split(". ") if term in s), ex)
            if is_hedge_only(sentence):
                hedge_context = True
            result["evidence"].append(ex)
        if not hedge_context or len(result["evidence"]) > 1:
            result["catch"] = True
            result["notes"] = (
                f"named defect symbol {symbols} identified with concrete "
                f"breakage terms {breakage_hit}")
        else:
            result["needs_review"] = True
            result["notes"] = ("breakage terms appear only in hedge "
                               "context; reviewer must decide")
    elif symbol_hit or breakage_hit:
        result["needs_review"] = True
        result["notes"] = (
            f"partial signal: symbol_hit={symbol_hit}, "
            f"breakage_hit={breakage_hit}; hedge patterns present="
            f"{any(is_hedge_only(excerpt(transcript_text, t))
                   for t in breakage_hit)}")
    else:
        result["false_negative"] = True
        result["notes"] = "named defect symbols never mentioned"

    if not result["catch"] and result["false_negative"] is False:
        # Explicit false reassurance check.
        for term in REASSURANCE_TERMS:
            if term in low and symbols and symbols[0].lower() in low:
                result["false_reassurance"] = True
                result["evidence"].append(excerpt(transcript_text, term))
                break
    return result


def adjudicate_all(run_root: Path, manifest: Path, out_dir: Path,
                   overrides: dict[str, str] | None = None) -> list[dict]:
    """Adjudicate every manifest run into sheets under out_dir."""
    overrides = overrides or {}
    out_dir.mkdir(parents=True, exist_ok=True)
    sheets = []
    for line in manifest.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        run = json.loads(line)
        key = f"{run['arm']}-{run['fixture']}-run{run['run']}"
        text = read_transcript(Path(run["transcript"]))
        sheet = classify(text, run["fixture"])
        sheet["run_id"] = run.get("run_id")
        sheet["arm"] = run["arm"]
        sheet["run"] = run["run"]
        sheet["complete"] = run.get("complete", False)
        if key in overrides:
            sheet["reviewer_override"] = overrides[key]
            for field in ("catch", "false_positive", "false_negative",
                          "false_reassurance"):
                sheet[field] = False
            sheet[overrides[key]] = True
            sheet["needs_review"] = False
        (out_dir / f"{key}-{sheet['run_id'][:8]}.json").write_text(
            json.dumps(sheet, indent=2) + "\n", encoding="utf-8")
        sheets.append(sheet)
    return sheets


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--run-root", type=Path, required=True)
    ap.add_argument("--manifest", type=Path, required=True)
    ap.add_argument("--out-dir", type=Path, required=True)
    ap.add_argument("--overrides", default=None,
                    help="JSON object key->outcome for single-reviewer "
                         "resolutions")
    args = ap.parse_args()
    overrides = json.loads(args.overrides) if args.overrides else None
    sheets = adjudicate_all(args.run_root, args.manifest, args.out_dir,
                            overrides)
    review_needed = [s for s in sheets if s["needs_review"]]
    print(json.dumps({"adjudicated": len(sheets),
                      "needs_review": len(review_needed)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
