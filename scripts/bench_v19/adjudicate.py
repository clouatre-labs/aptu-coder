"""Anchor-adjudication protocol client for the v19 benchmark.

Implements the FROZEN question protocol from issue #1686 verbatim:
one `choice` question per cited anchor, no arm labels in state, five
anchor-role criteria. Jev is a *verification* oracle only -- the AST
oracle remains the sole ground truth for the oracle *set*; Jev only
classifies the *role of cited evidence*. The code-side decision rule
maps the judge's role answer onto a verdict; no judge/API call happens
in this module (the live call runs through the MCP judge tool at
calibration time).

Changes to this protocol after calibration are a manifest-recorded
amendment, not silent edits.
"""

from __future__ import annotations

# Model pin: temperature-free API; changes require a new calibration run.
JEV_MODEL = "jev-1.13.0"

# Verdicts produced by the frozen code-side decision rule.
TRUE_POSITIVE = "true-positive"
ADJUDICATION_DISAGREEMENT = "adjudication-disagreement"
PRECISION_PENALTY = "precision-penalty"

# The five anchor-role criteria, in the frozen protocol order.
ANCHOR_ROLE_CRITERIA = {
    "call_of_target": (
        "the cited line contains a call whose callee is the target symbol"
    ),
    "definition_of_target": "the cited line defines the target symbol itself",
    "textual_mention_only": (
        "the symbol appears only in a comment, docstring, or string literal"
    ),
    "other_symbol_same_name": "a call to a different symbol that happens to share the name",
    "unrelated": "no relationship to the target symbol",
}

ANCHOR_ROLE_INSTRUCTIONS = (
    "Classify what the cited context shows relative to the target symbol."
)


def build_adjudication_request(anchor_record: dict) -> dict:
    """Build the {state, questions} judge payload for one cited anchor.

    Payload shape is the frozen protocol from #1686, verbatim: state
    carries the target symbol, scope, cited anchor (file + line) and the
    verbatim cited context; questions carry exactly one `choice` question
    named `anchor_role` with the five criteria. No arm labels, no human
    labels, no oracle set membership.
    """
    cited_anchor = anchor_record["cited_anchor"]
    return {
        "state": {
            "target_symbol": anchor_record["target_symbol"],
            "scope": anchor_record["scope"],
            "cited_anchor": {
                "file": cited_anchor["file"],
                "line": cited_anchor["line"],
            },
            "cited_context": anchor_record["cited_context"],
        },
        "questions": {
            "anchor_role": {
                "type": "choice",
                "instructions": ANCHOR_ROLE_INSTRUCTIONS,
                "criteria": dict(ANCHOR_ROLE_CRITERIA),
            }
        },
    }


def apply_decision_rule(
    anchor_record: dict,
    judge_answer: dict,
    oracle_set: set,
) -> dict:
    """Apply the frozen code-side rule to a judge answer for one anchor.

    Rule (frozen, #1686):
    - `call_of_target` with anchor in the oracle set -> true-positive
    - `call_of_target` not in the oracle set -> adjudication-disagreement
      (flag for human review, never silently scored)
    - every other role -> precision-penalty per the existing F1 layer

    Returns a dict with the verdict, the judge's role, and the anchor
    identity so results can be joined back to labels.
    """
    role = judge_answer["anchor_role"]
    if role not in ANCHOR_ROLE_CRITERIA:
        raise ValueError("unknown anchor role: {!r}".format(role))
    cited_anchor = anchor_record["cited_anchor"]
    in_oracle = (cited_anchor["file"], cited_anchor["line"]) in oracle_set
    if role == "call_of_target":
        verdict = TRUE_POSITIVE if in_oracle else ADJUDICATION_DISAGREEMENT
    else:
        verdict = PRECISION_PENALTY
    return {
        "verdict": verdict,
        "role": role,
        "file": cited_anchor["file"],
        "line": cited_anchor["line"],
    }
