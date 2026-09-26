"""Offline tests for the v19 anchor-adjudication protocol (#1686).

Pure-offline: no network, no judge/API calls. Fidelity of the request
shape to the frozen protocol, the code-side decision rule for all five
roles plus the disagreement case, and calibration agreement/gate math.
"""

from __future__ import annotations

import pytest
from bench_v19.adjudicate import (
    ANCHOR_ROLE_CRITERIA,
    JEV_MODEL,
    ADJUDICATION_DISAGREEMENT,
    PRECISION_PENALTY,
    TRUE_POSITIVE,
    apply_decision_rule,
    build_adjudication_request,
)
from bench_v19.calibrate import (
    FREEZE_OVERALL_MIN,
    FREEZE_PER_ROLE_MIN,
    compute_agreement,
    emit_requests,
    freeze_gate,
    score_results,
)

ANCHOR = {
    "target_symbol": "parse_config",
    "scope": "aptu-coder/crates/aptu-coder-core",
    "cited_anchor": {"file": "src/config.rs", "line": 42},
    "cited_context": "let cfg = parse_config(&raw)?;",
}

ORACLE = {("src/config.rs", 42), ("src/other.rs", 7)}


def anchor_for(role_context):
    record = dict(ANCHOR)
    record["cited_context"] = role_context
    return record


# --- frozen protocol shape -------------------------------------------------


def test_protocol_constants_frozen():
    assert JEV_MODEL == "jev-1.13.0"
    assert list(ANCHOR_ROLE_CRITERIA) == [
        "call_of_target",
        "definition_of_target",
        "textual_mention_only",
        "other_symbol_same_name",
        "unrelated",
    ]


def test_request_state_shape_matches_frozen_protocol():
    request = build_adjudication_request(ANCHOR)
    assert set(request) == {"state", "questions"}
    state = request["state"]
    assert set(state) == {"target_symbol", "scope", "cited_anchor", "cited_context"}
    assert state["target_symbol"] == ANCHOR["target_symbol"]
    assert state["scope"] == ANCHOR["scope"]
    assert state["cited_anchor"] == {"file": ANCHOR["cited_anchor"]["file"], "line": 42}
    assert state["cited_context"] == ANCHOR["cited_context"]


def test_request_questions_shape_matches_frozen_protocol():
    questions = build_adjudication_request(ANCHOR)["questions"]
    assert list(questions) == ["anchor_role"]
    question = questions["anchor_role"]
    assert question["type"] == "choice"
    assert question["instructions"] == (
        "Classify what the cited context shows relative to the target symbol."
    )
    assert question["criteria"] == ANCHOR_ROLE_CRITERIA
    for description in question["criteria"].values():
        assert isinstance(description, str) and description


def test_request_carries_no_labels_and_is_deterministic():
    first = build_adjudication_request(ANCHOR)
    second = build_adjudication_request(dict(ANCHOR))
    assert first == second
    serialized = repr(first)
    assert "human_label" not in serialized
    assert "oracle" not in serialized
    assert JEV_MODEL not in serialized  # pin lives in the harness, not state


def test_emit_requests_payload_pins_model(tmp_path):
    labels = [{"anchor_record": ANCHOR, "human_label": "call_of_target"}]
    out = tmp_path / "requests.json"
    count = emit_requests(labels, str(out))
    assert count == 1
    payload = __import__("json").loads(out.read_text())
    assert payload["model"] == JEV_MODEL
    assert payload["requests"] == [build_adjudication_request(ANCHOR)]


# --- frozen code-side decision rule ----------------------------------------


@pytest.mark.parametrize(
    ("role", "in_oracle", "expected"),
    [
        ("call_of_target", True, TRUE_POSITIVE),
        ("call_of_target", False, ADJUDICATION_DISAGREEMENT),
        ("definition_of_target", True, PRECISION_PENALTY),
        ("textual_mention_only", True, PRECISION_PENALTY),
        ("other_symbol_same_name", True, PRECISION_PENALTY),
        ("unrelated", True, PRECISION_PENALTY),
    ],
)
def test_decision_rule_all_roles(role, in_oracle, expected):
    oracle = ORACLE if in_oracle else set()
    result = apply_decision_rule(ANCHOR, {"anchor_role": role}, oracle)
    assert result["verdict"] == expected
    assert result["role"] == role
    assert result["file"] == "src/config.rs"
    assert result["line"] == 42


def test_decision_rule_rejects_unknown_role():
    with pytest.raises(ValueError):
        apply_decision_rule(ANCHOR, {"anchor_role": "invented_role"}, ORACLE)


# --- calibration agreement and freeze gate ---------------------------------


def labels_for(roles):
    return [
        {"anchor_record": anchor_for("ctx-{}".format(i)), "human_label": role}
        for i, role in enumerate(roles)
    ]


def answers_for(roles):
    return [{"anchor_role": role} for role in roles]


def test_agreement_perfect():
    roles = ["call_of_target", "definition_of_target", "unrelated"]
    report = compute_agreement(labels_for(roles), answers_for(roles))
    assert report["overall_agreement"] == 1.0
    assert report["per_role_agreement"]["call_of_target"] == 1.0
    assert report["per_role_counts"]["call_of_target"] == 1
    assert report["per_role_counts"]["textual_mention_only"] == 0


def test_agreement_mixed():
    labels = labels_for(["call_of_target", "call_of_target", "unrelated"])
    answers = answers_for(["call_of_target", "unrelated", "unrelated"])
    report = compute_agreement(labels, answers)
    assert report["overall_agreement"] == pytest.approx(2 / 3)
    assert report["per_role_agreement"]["call_of_target"] == pytest.approx(0.5)
    assert report["per_role_agreement"]["unrelated"] == 1.0


def test_agreement_length_mismatch_raises():
    with pytest.raises(ValueError):
        compute_agreement(labels_for(["call_of_target"]), [])


def test_freeze_gate_constants():
    assert FREEZE_OVERALL_MIN == 0.9
    assert FREEZE_PER_ROLE_MIN == 0.75


def test_freeze_gate_pass_and_fail():
    roles = list(ANCHOR_ROLE_CRITERIA)
    passing = score_results(labels_for(roles), answers_for(roles))
    assert passing["freeze_gate_passed"] is True

    labels = labels_for(["call_of_target", "call_of_target", "call_of_target",
                         "call_of_target", "unrelated"])
    answers = answers_for(["call_of_target", "call_of_target", "unrelated",
                           "unrelated", "unrelated"])
    failing = score_results(labels, answers)
    assert failing["overall_agreement"] == pytest.approx(0.6)
    assert failing["freeze_gate_passed"] is False


def test_freeze_gate_role_floor():
    # Overall >= 0.9 but one role drops below the 0.75 floor -> gate fails.
    roles = ["call_of_target"] * 18 + ["definition_of_target"] * 5
    labels = labels_for(roles)
    answers = answers_for(["call_of_target"] * 18 + ["definition_of_target"] * 3
                          + ["unrelated"] * 2)
    report = score_results(labels, answers)
    assert report["overall_agreement"] == pytest.approx(21 / 23)
    assert report["overall_agreement"] > 0.9
    assert report["per_role_agreement"]["definition_of_target"] == pytest.approx(0.6)
    assert report["freeze_gate_passed"] is False
