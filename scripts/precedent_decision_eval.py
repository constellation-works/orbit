"""Opt-in precedent-decision evaluation contract (decision-assistance-eval/4).

Stdlib only. No network and no runtime activation. Dispatched from
evaluate-decision-assistance.py only for `precedent-schema` or a document
whose format_version is this contract. `/1`–`/3` never reach this module.

Openings are recorded under `.decision-assistance-eval/precedent-windows/`.
They must not be written into `holdouts/`: the `/1`–`/2` reader rejects any
other format_version found there.
"""

import argparse
import hashlib
import json
import math
import os
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

FORMAT = "decision-assistance-eval/4"
QUESTION_SET = "precedent-decision/1"
CONSUMER = "precedent_decision"
RECORDED = "recorded_human_decision"
ABSTAIN = "insufficient_precedent"
READOUT_VERSION = "precedent-decision-readout/1"
POLICY_TEXT = "precedent-decision/1 synthetic-seed policy\n"
DISPOSITIONS = ("suggested", "uncertain", "refused", "unavailable", "invalid", "stale")
CLOSED_STATUSES = frozenset({"superseded", "expired"})
INELIGIBLE_ACTORS = frozenset({"model", "precedent", "automation"})
ELIGIBLE_HUMAN_ACTORS = frozenset({"human", "human_confirmed"})
TARGET_KEYS = ("threshold", "min_samples", "min_accuracy", "min_coverage", "max_ece")
SAMPLE_FLOOR = 30
ACCURACY_FLOOR = 0.95
COVERAGE_FLOOR = 0.30
ECE_CAP = 0.05
ECE_BINS = 10
PRECEDENT_WINDOWS = Path(".decision-assistance-eval") / "precedent-windows"
SEED_REGISTERED = "2026-03-01T00:00:00Z"
SEED_WINDOW_START = "2026-04-01T00:00:00Z"
SEED_WINDOW_END = "2026-05-01T00:00:00Z"
SEED_CATEGORY = "ws_orbit:agent_request"
SEED_OTHER = "ws_orbit:final_recovery"
EXCLUSIONS = (
    "leaked",
    "superseded_or_expired",
    "unsealed",
    "no_answered_at",
    "unavailable",
    "outside_window",
)


def _digest(text):
    return "sha256:" + hashlib.sha256(text.encode()).hexdigest()


def _is_digest(value):
    return (
        isinstance(value, str)
        and value.startswith("sha256:")
        and len(value) == 71
        and all(character in "0123456789abcdef" for character in value[7:])
    )


def _ts(value):
    """Parse an ISO-8601 instant. Naive timestamps are rejected."""
    if not isinstance(value, str):
        return None
    try:
        timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return timestamp if timestamp.tzinfo is not None else None


def _finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _text(value):
    return value if isinstance(value, str) and value.strip() and value == value.strip() else None


def _emit(obj, code=0):
    print(json.dumps(obj, indent=2, sort_keys=True))
    return code


def _dedupe(errors):
    unique = []
    for error in errors:
        if error not in unique:
            unique.append(error)
    return unique


def _flag_value(argv, flag):
    prefix = flag + "="
    for index, arg in enumerate(argv):
        if arg == flag and index + 1 < len(argv):
            return argv[index + 1]
        if arg.startswith(prefix):
            return arg[len(prefix):]
    return None


def _format_requested(argv):
    if FORMAT in argv:
        return True
    prefix = "--format-version="
    return any(arg.startswith(prefix) and arg[len(prefix):] == FORMAT for arg in argv)


def _load(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def _owns(argv):
    """True only for the additive command or an explicit /4 document."""
    if not argv:
        return False
    command = argv[0]
    if command == "precedent-schema":
        return True
    if command == "seed":
        return _format_requested(argv)
    if command not in ("validate", "score", "check-split", "check-targets"):
        return False
    flag = "--targets" if command == "check-targets" else "--dataset"
    path = _flag_value(argv, flag)
    if not isinstance(path, str) or not path or path.startswith("-"):
        return False
    try:
        document = _load(path)
    except (OSError, ValueError, RecursionError, UnicodeError):
        return False
    return isinstance(document, dict) and document.get("format_version") == FORMAT


def _actor_tokens(label):
    tokens = []
    if not isinstance(label, dict):
        return tokens
    for key in ("answered_by", "decided_by_kind", "actor"):
        value = label.get(key)
        if isinstance(value, str):
            tokens.append(value)
    writer = label.get("writer")
    if isinstance(writer, dict):
        for key in ("actor", "answered_by", "kind"):
            value = writer.get(key)
            if isinstance(value, str):
                tokens.append(value)
    return tokens


def _ineligible_actor_message(label):
    if not isinstance(label, dict):
        return None
    if label.get("automation") is True:
        return "automation actor labels are not holdout-eligible"
    for token in _actor_tokens(label):
        if token in INELIGIBLE_ACTORS:
            return f"{token} actor labels are not holdout-eligible"
    return None


def wilson(successes, total):
    """Wilson score interval at 95%, matching the delivery-quality helper."""
    if not total:
        return {"value": None, "count": successes, "n": 0, "interval_95": None, "method": "Wilson"}
    z = 1.959963984540054
    proportion = successes / total
    denominator = 1 + z * z / total
    center = (proportion + z * z / (2 * total)) / denominator
    half = z * math.sqrt(proportion * (1 - proportion) / total + z * z / (4 * total * total)) / denominator
    return {
        "value": proportion,
        "count": successes,
        "n": total,
        "interval_95": [max(0.0, center - half), min(1.0, center + half)],
        "method": "Wilson",
    }


def expected_calibration_error(pairs):
    """ECE over 10 equal-width bins. The last bin includes probability 1."""
    if not pairs:
        return {"value": None, "bins": ECE_BINS, "n": 0, "method": "equal-width", "reliability": []}
    totals = [0] * ECE_BINS
    confidence_sum = [0.0] * ECE_BINS
    accuracy_sum = [0.0] * ECE_BINS
    for probability, correct in pairs:
        index = min(ECE_BINS - 1, int(probability * ECE_BINS))
        totals[index] += 1
        confidence_sum[index] += probability
        accuracy_sum[index] += 1.0 if correct else 0.0
    count = len(pairs)
    error = 0.0
    table = []
    for index in range(ECE_BINS):
        if not totals[index]:
            continue
        confidence = confidence_sum[index] / totals[index]
        accuracy = accuracy_sum[index] / totals[index]
        error += (totals[index] / count) * abs(accuracy - confidence)
        table.append({
            "bin": index,
            "lower": index / ECE_BINS,
            "upper": (index + 1) / ECE_BINS,
            "n": totals[index],
            "confidence": confidence,
            "accuracy": accuracy,
        })
    return {"value": error, "bins": ECE_BINS, "n": count, "method": "equal-width", "reliability": table}


def brier_score(pairs):
    """Brier score of decision_choice: mean (probability - correct)^2."""
    if not pairs:
        return {"value": None, "n": 0}
    total = sum((probability - (1.0 if correct else 0.0)) ** 2 for probability, correct in pairs)
    return {"value": total / len(pairs), "n": len(pairs)}


def precedent_schema():
    return {
        "format_version": FORMAT,
        "consumer": CONSUMER,
        "question_set": QUESTION_SET,
        "choice": "decision_choice",
        "abstention": ABSTAIN,
        "label_provenance": RECORDED,
        "holdout_eligible_writers": (
            "human or human_confirmed, automation false; "
            "model, precedent and automation actors are refused as holdout labels"
        ),
        "splits": "prospective time window [window_start, window_end)",
        "per_category_target_keys": list(TARGET_KEYS),
        "target_floors": {
            "min_samples": SAMPLE_FLOOR,
            "min_accuracy": ACCURACY_FLOOR,
            "min_coverage": COVERAGE_FLOOR,
            "max_ece": ECE_CAP,
            "note": "a registration may tighten these floors and may not loosen them",
        },
        "metrics": [
            "coverage",
            "accuracy",
            "ece",
            "brier",
            "abstention",
            "conflict_rate",
            "conflict_accuracy",
            "citation_agreement",
        ],
        "ece_bins": ECE_BINS,
        "accuracy_interval": "wilson95",
        "readout_version": READOUT_VERSION,
    }


def check_precedent_targets(doc, holdout_opened_at=None):
    """Require every per-category key and a registration strictly before the window."""
    if not isinstance(doc, dict):
        return ["targets document must be a JSON object"]
    errors = []
    if doc.get("format_version") != FORMAT:
        errors.append(f"targets format_version must be {FORMAT}")
    registered = _ts(doc.get("registered_at", ""))
    start = _ts(doc.get("window_start", ""))
    end = _ts(doc.get("window_end", ""))
    if registered is None:
        errors.append("targets registered_at must be ISO-8601 with a timezone")
    if start is None:
        errors.append("targets window_start must be ISO-8601 with a timezone")
    if end is None:
        errors.append("targets window_end must be ISO-8601 with a timezone")
    if start is not None and end is not None and end <= start:
        errors.append("window_end must be after window_start")
    if registered is not None and start is not None and registered >= start:
        errors.append(
            "registration must be before the window starts (registered_at >= window_start)"
        )
    if holdout_opened_at:
        opened = _ts(holdout_opened_at)
        if opened is None:
            errors.append("holdout-opened-at must be ISO-8601 with a timezone")
        elif registered is not None and registered >= opened:
            errors.append(
                "targets must be registered before holdout access "
                "(registered_at >= holdout_opened_at)"
            )
    if not _is_digest(doc.get("policy_digest")):
        errors.append("policy_digest must be sha256:<64 lowercase hex>")
    categories = doc.get("categories")
    if not isinstance(categories, list) or not categories:
        return errors + ["categories must be a non-empty list of per-category target objects"]
    seen = set()
    for index, row in enumerate(categories):
        where = f"categories[{index}]"
        if not isinstance(row, dict):
            errors.append(f"{where} must be an object")
            continue
        name = _text(row.get("category"))
        if name is None:
            errors.append(f"{where}.category must be a non-empty string")
            where = f"{where} category"
        else:
            where = f"category {name}"
            if name in seen:
                errors.append(f"{where} is duplicated")
            seen.add(name)
        missing = [key for key in TARGET_KEYS if key not in row]
        if missing:
            errors.append(f"{where}: missing {', '.join(missing)}")
        if "threshold" in row:
            threshold = row["threshold"]
            if not _finite(threshold) or threshold <= 0 or threshold > 1:
                errors.append(f"{where}: threshold must be a finite number in (0, 1]")
        if "min_samples" in row:
            samples = row["min_samples"]
            if type(samples) is not int or samples < SAMPLE_FLOOR:
                errors.append(f"{where}: min_samples must be an integer >= {SAMPLE_FLOOR}")
        if "min_accuracy" in row:
            accuracy = row["min_accuracy"]
            if not _finite(accuracy) or accuracy + 1e-12 < ACCURACY_FLOOR or accuracy > 1:
                errors.append(f"{where}: min_accuracy must be a number in [{ACCURACY_FLOOR}, 1]")
        if "min_coverage" in row:
            coverage = row["min_coverage"]
            if not _finite(coverage) or coverage + 1e-12 < COVERAGE_FLOOR or coverage > 1:
                errors.append(f"{where}: min_coverage must be a number in [{COVERAGE_FLOOR}, 1]")
        if "max_ece" in row:
            ece = row["max_ece"]
            if not _finite(ece) or ece < 0 or ece > ECE_CAP + 1e-12:
                errors.append(f"{where}: max_ece must be a number in [0, {ECE_CAP}]")
    return errors


def _recorded_field_errors(label, where):
    errors = []
    if label.get("relabelled") is True:
        errors.append(f"{where}: recorded_human_decision is not relabelled after the fact")
    if _text(label.get("decided_by")) is None:
        errors.append(f"{where}: decided_by must name the human writer")
    if _ts(label.get("answered_at", "")) is None:
        errors.append(f"{where}: answered_at must be ISO-8601 with a timezone")
    if _text(label.get("channel")) is None:
        errors.append(f"{where}: channel must name the recorded channel")
    if _ineligible_actor_message(label) is None and not any(
        token in ELIGIBLE_HUMAN_ACTORS for token in _actor_tokens(label)
    ):
        errors.append(f"{where}: recorded_human_decision requires answered_by human or human_confirmed")
    return errors


def _cases(ds):
    raw = ds.get("cases") if isinstance(ds, dict) else None
    return raw if isinstance(raw, list) else []


def validate_precedent_dataset(ds):
    if not isinstance(ds, dict) or ds.get("format_version") != FORMAT:
        return [f"format_version must be {FORMAT}"]
    errors = []
    if _text(ds.get("dataset_id")) is None:
        errors.append("dataset_id must be a non-empty string")
    if ds.get("consumer") != CONSUMER:
        errors.append(f"consumer must be {CONSUMER}")
    if ds.get("question_set") != QUESTION_SET:
        errors.append(f"question_set must be {QUESTION_SET}")
    if not isinstance(ds.get("cases"), list):
        return errors + ["cases must be a list"]
    seen = set()
    for index, case in enumerate(ds["cases"]):
        where = f"cases[{index}]"
        if not isinstance(case, dict):
            errors.append(f"{where} must be an object")
            continue
        case_id = _text(case.get("case_id"))
        if case_id is None:
            errors.append(f"{where}.case_id must be a non-empty string")
        elif case_id in seen:
            errors.append(f"case {case_id} is duplicated")
        else:
            seen.add(case_id)
            where = f"case {case_id}"
        if case.get("consumer") != CONSUMER:
            errors.append(
                f"{where}: consumer must be {CONSUMER}; "
                "recorded_human_decision is holdout-eligible only for this consumer"
            )
        if _text(case.get("category")) is None:
            errors.append(f"{where}: category must be a non-empty string")
        if _text(case.get("topic")) is None:
            errors.append(f"{where}: topic must be a non-empty string")
        if _ts(case.get("raised_at", "")) is None:
            errors.append(f"{where}: raised_at must be ISO-8601 with a timezone")
        status = case.get("status", "answered")
        if status not in CLOSED_STATUSES and status != "answered":
            errors.append(f"{where}: status must be answered, superseded or expired")
        label = case.get("label")
        if not isinstance(label, dict):
            errors.append(f"{where}: label must be an object")
            continue
        provenance = label.get("provenance")
        if provenance not in ("synthetic", RECORDED):
            errors.append(
                f"{where}: label.provenance must be synthetic or {RECORDED}, got {provenance!r}"
            )
        if "value" not in label:
            errors.append(f"{where}: label.value is required")
        if provenance == "synthetic" and label.get("human_ground_truth") is True:
            errors.append(f"{where}: synthetic labels are not human ground truth")
        if provenance == RECORDED:
            errors.extend(_recorded_field_errors(label, where))
            actor = _ineligible_actor_message(label)
            if actor:
                errors.append(f"{where}: {actor}")
    return errors


def _citation_errors(citation, where, raised):
    errors = []
    if not isinstance(citation, dict):
        return [f"{where} must be an object"]
    if _text(citation.get("precedent_id")) is None:
        errors.append(f"{where}.precedent_id must be a non-empty string")
    for key in ("supports_choice", "same_consequence"):
        if key in citation and not isinstance(citation[key], bool):
            errors.append(f"{where}.{key} must be a boolean")
    if "decided_at" in citation:
        decided = _ts(citation.get("decided_at"))
        if decided is None:
            errors.append(f"{where}.decided_at must be ISO-8601 with a timezone")
        elif raised is not None and decided >= raised:
            errors.append(f"{where}.decided_at must be strictly before raised_at")
    return errors


def _conflict_value_errors(raw, where):
    if isinstance(raw, dict):
        if "value" not in raw:
            return [f"{where}.precedents_conflict.value is required"]
        raw = raw["value"]
    if isinstance(raw, bool) or (_finite(raw) and 0 <= raw <= 1):
        return []
    return [f"{where}.precedents_conflict must be a boolean or a finite probability in [0, 1]"]


def validate_precedent_answers(ds, answers):
    if not isinstance(answers, dict) or answers.get("format_version") != FORMAT:
        return [f"answers format_version must be {FORMAT}"]
    rows = answers.get("answers")
    if not isinstance(rows, list):
        return ["answers must be a list"]
    by_id = {}
    for case in _cases(ds):
        if isinstance(case, dict) and _text(case.get("case_id")):
            by_id[case["case_id"]] = case
    errors = []
    seen = set()
    for index, row in enumerate(rows):
        where = f"answers[{index}]"
        if not isinstance(row, dict):
            errors.append(f"{where} must be an object")
            continue
        case_id = _text(row.get("case_id"))
        if case_id is None:
            errors.append(f"{where}.case_id must be a non-empty string")
            continue
        where = f"answer {case_id}"
        if case_id in seen:
            errors.append(f"{where} is duplicated")
        seen.add(case_id)
        case = by_id.get(case_id)
        if case is None:
            errors.append(f"{where} does not match a dataset case")
        if row.get("disposition") not in DISPOSITIONS:
            errors.append(f"{where}: disposition must be one of {', '.join(DISPOSITIONS)}")
        if "sealed_at" in row and _ts(row.get("sealed_at")) is None:
            errors.append(f"{where}: sealed_at must be ISO-8601 with a timezone")
        if "precedents_conflict" in row:
            errors.extend(_conflict_value_errors(row.get("precedents_conflict"), where))
        citations = row.get("citations", [])
        if not isinstance(citations, list):
            errors.append(f"{where}: citations must be a list")
            citations = []
        raised = _ts(case.get("raised_at", "")) if isinstance(case, dict) else None
        for citation_index, citation in enumerate(citations):
            errors.extend(_citation_errors(citation, f"{where}.citations[{citation_index}]", raised))
        if row.get("disposition") != "suggested":
            continue
        choice = row.get("answers")
        choice = choice.get("decision_choice") if isinstance(choice, dict) else None
        if not isinstance(choice, dict) or not isinstance(choice.get("value"), str) or not choice["value"]:
            errors.append(f"{where}: suggested answers need decision_choice.value")
            continue
        if choice["value"] == ABSTAIN:
            if "probability" in choice and (
                not _finite(choice["probability"]) or not 0 <= choice["probability"] <= 1
            ):
                errors.append(f"{where}: probability must be finite in [0, 1]")
            continue
        probability = choice.get("probability")
        if not _finite(probability) or not 0 <= probability <= 1:
            errors.append(f"{where}: decision_choice.probability must be finite in [0, 1]")
    return errors


def _window_bounds(doc):
    start = _ts(doc.get("window_start", "")) if isinstance(doc, dict) else None
    end = _ts(doc.get("window_end", "")) if isinstance(doc, dict) else None
    if start is None or end is None or end <= start:
        return None
    return start, end


def window_label_errors(ds, start, end, *, require_recorded):
    """Holdout labels are in-window cases. Model and precedent actors are refused."""
    errors = []
    for case in _cases(ds):
        if not isinstance(case, dict):
            continue
        raised = _ts(case.get("raised_at", ""))
        if raised is None or not (start <= raised < end):
            continue
        case_id = case.get("case_id")
        where = f"holdout case {case_id}"
        label = case.get("label") if isinstance(case.get("label"), dict) else {}
        provenance = label.get("provenance")
        actor = _ineligible_actor_message(label)
        if actor:
            errors.append(f"{where}: {actor}")
        if require_recorded:
            if provenance != RECORDED:
                errors.append(
                    f"{where}: label.provenance must be {RECORDED}, got {provenance!r} "
                    "(synthetic is not a prospective holdout label)"
                )
            elif actor is None:
                errors.extend(_recorded_field_errors(label, where))
            continue
        if provenance == RECORDED and actor is None:
            errors.extend(_recorded_field_errors(label, where))
        elif provenance == "synthetic":
            if label.get("human_ground_truth") is True:
                errors.append(f"{where}: synthetic labels are not human ground truth")
        elif actor is None:
            errors.append(
                f"{where}: label.provenance {provenance!r} is not holdout-eligible "
                f"({RECORDED} or synthetic mechanics)"
            )
    return errors


def _registered_rows(doc):
    rows = {}
    for row in doc.get("categories") or []:
        if isinstance(row, dict) and _text(row.get("category")):
            rows[row["category"]] = row
    return rows


def _choice(answer):
    answers = answer.get("answers") if isinstance(answer, dict) else None
    choice = answers.get("decision_choice") if isinstance(answers, dict) else None
    if not isinstance(choice, dict):
        return None, None
    return choice.get("value"), choice.get("probability")


def _citations(answer):
    raw = answer.get("citations") if isinstance(answer, dict) else None
    return [item for item in raw if isinstance(item, dict)] if isinstance(raw, list) else []


def _supports(answer):
    return any(item.get("supports_choice") is True for item in _citations(answer))


def _conflict(answer):
    raw = answer.get("precedents_conflict") if isinstance(answer, dict) else None
    if isinstance(raw, dict):
        raw = raw.get("value")
    if isinstance(raw, bool):
        return raw
    return bool(_finite(raw) and raw >= 0.5)


def _citation_agrees(answer):
    citations = _citations(answer)
    return bool(citations) and all(item.get("same_consequence") is True for item in citations)


def _classify(case, answer, start, end):
    raised = _ts(case.get("raised_at", ""))
    if raised is None or not (start <= raised < end):
        return "outside_window"
    if case.get("status", "answered") in CLOSED_STATUSES:
        return "superseded_or_expired"
    if answer is None:
        return "unavailable"
    if _ts((case.get("label") or {}).get("answered_at", "")) is None:
        return "no_answered_at"
    sealed = _ts(answer.get("sealed_at", ""))
    answered = _ts(case["label"]["answered_at"])
    if sealed is None:
        return "unsealed"
    if sealed >= answered:
        return "leaked"
    return "scored"


def _evidence_status(provenances, dataset):
    if not provenances:
        if isinstance(dataset, dict) and dataset.get("label_provenance") == "synthetic":
            return "synthetic mechanics only; not human ground truth"
        return "no scored requests; score is not an adoption authorization"
    if provenances <= {"synthetic"}:
        return "synthetic mechanics only; not human ground truth"
    if provenances <= {RECORDED}:
        return "recorded human decisions; score is not an adoption authorization"
    return "mixed label provenance; score is not an adoption authorization"


def _empty_bucket():
    return {
        "scored": 0,
        "covered": 0,
        "correct_covered": 0,
        "abstained": 0,
        "flagged": 0,
        "flagged_correct": 0,
        "flagged_choices": 0,
        "citation_agreed": 0,
        "pairs": [],
        "excluded": Counter(),
        "provenances": set(),
    }


def score_precedent(ds, answers, targets):
    """Return (report, errors). Errors refuse the readout; metrics are not partial."""
    errors = []
    errors.extend(check_precedent_targets(targets))
    errors.extend(validate_precedent_dataset(ds))
    errors.extend(validate_precedent_answers(ds, answers))
    bounds = _window_bounds(targets) if isinstance(targets, dict) else None
    if bounds is not None and isinstance(ds, dict):
        errors.extend(window_label_errors(ds, bounds[0], bounds[1], require_recorded=False))
    if errors or bounds is None or not isinstance(targets, dict):
        return None, _dedupe(errors or ["targets window is not a prospective interval"])
    start, end = bounds
    rows = _registered_rows(targets)
    by_answer = {}
    for row in answers.get("answers") or []:
        if isinstance(row, dict) and _text(row.get("case_id")):
            by_answer[row["case_id"]] = row
    buckets = {name: _empty_bucket() for name in rows}
    for case in _cases(ds):
        if not isinstance(case, dict):
            continue
        category = case.get("category")
        kind = _classify(case, by_answer.get(case.get("case_id")), start, end)
        if kind == "outside_window":
            if category in buckets:
                buckets[category]["excluded"]["outside_window"] += 1
            continue
        if category not in buckets:
            errors.append(f"case {case.get('case_id')}: no registered target for category {category}")
            continue
        bucket = buckets[category]
        if kind != "scored":
            bucket["excluded"][kind] += 1
            continue
        answer = by_answer[case["case_id"]]
        label = case["label"]
        bucket["provenances"].add(label.get("provenance"))
        bucket["scored"] += 1
        if _conflict(answer):
            bucket["flagged"] += 1
        value, probability = _choice(answer)
        abstained = answer.get("disposition") != "suggested" or value in (None, ABSTAIN)
        if abstained:
            bucket["abstained"] += 1
            continue
        correct = value == label.get("value")
        bucket["pairs"].append((probability, correct))
        if _conflict(answer):
            bucket["flagged_choices"] += 1
            if correct:
                bucket["flagged_correct"] += 1
        if probability >= rows[category]["threshold"] and _supports(answer):
            bucket["covered"] += 1
            if correct:
                bucket["correct_covered"] += 1
                if _citation_agrees(answer):
                    bucket["citation_agreed"] += 1
    if errors:
        return None, _dedupe(errors)
    provenances = set()
    categories = {}
    for name, row in rows.items():
        bucket = buckets[name]
        provenances |= bucket["provenances"]
        scored = bucket["scored"]
        covered = bucket["covered"]
        correct = bucket["correct_covered"]
        excluded = {key: bucket["excluded"].get(key, 0) for key in EXCLUSIONS}
        in_window_gaps = sum(excluded[key] for key in EXCLUSIONS if key != "outside_window")
        categories[name] = {
            "threshold": row["threshold"],
            "raised_in_window": scored + in_window_gaps,
            "scored": scored,
            "excluded": excluded,
            "coverage": {
                "value": (covered / scored) if scored else None,
                "covered": covered,
                "scored": scored,
            },
            "accuracy": wilson(correct, covered),
            "ece": expected_calibration_error(bucket["pairs"]),
            "brier": brier_score(bucket["pairs"]),
            "abstention": {
                "value": (bucket["abstained"] / scored) if scored else None,
                "count": bucket["abstained"],
                "scored": scored,
            },
            "conflict_rate": {
                "value": (bucket["flagged"] / scored) if scored else None,
                "count": bucket["flagged"],
                "scored": scored,
            },
            "conflict_accuracy": wilson(bucket["flagged_correct"], bucket["flagged_choices"]),
            "citation_agreement": {
                "value": (bucket["citation_agreed"] / correct) if correct else None,
                "count": bucket["citation_agreed"],
                "n": correct,
            },
            "targets": {key: row[key] for key in TARGET_KEYS},
        }
    report = {
        "format_version": FORMAT,
        "readout_version": READOUT_VERSION,
        "question_set": QUESTION_SET,
        "consumer": CONSUMER,
        "dataset_id": ds.get("dataset_id") if isinstance(ds, dict) else None,
        "evidence_status": _evidence_status(provenances, ds),
        "window": {"start": targets.get("window_start"), "end": targets.get("window_end")},
        "policy_digest": targets.get("policy_digest"),
        "categories": categories,
        "validation_errors": [],
    }
    return report, []


def check_precedent_split(ds, manifest, prior_ids):
    errors = validate_precedent_dataset(ds)
    if not isinstance(manifest, dict) or manifest.get("format_version") != FORMAT:
        return errors + [f"manifest format_version must be {FORMAT}"]
    holdout_id = manifest.get("holdout_id")
    if not isinstance(holdout_id, str) or not holdout_id.strip():
        errors.append("manifest holdout_id must be a non-empty string")
    elif holdout_id in prior_ids:
        errors.append(f"holdout_id {holdout_id} already used/opened; reused holdout IDs are rejected")
    start = _ts(manifest.get("window_start", ""))
    end = _ts(manifest.get("window_end", ""))
    registered = _ts(manifest.get("registered_at", ""))
    if start is None:
        errors.append("manifest window_start must be ISO-8601 with a timezone")
    if end is None:
        errors.append("manifest window_end must be ISO-8601 with a timezone")
    if registered is None:
        errors.append("manifest registered_at must be ISO-8601 with a timezone")
    if start is not None and end is not None and end <= start:
        errors.append("window_end must be after window_start")
    if registered is not None and start is not None and registered >= start:
        errors.append(
            "registration must be before the window starts (registered_at >= window_start)"
        )
    if start is not None and end is not None and end > start:
        errors.extend(window_label_errors(ds, start, end, require_recorded=True))
    if errors or not isinstance(holdout_id, str) or not holdout_id.strip():
        return _dedupe(errors)
    try:
        opened = _open_window(holdout_id, manifest["window_start"], manifest["window_end"])
    except OSError as error:
        return [f"cannot record precedent window: {error}"]
    if not opened:
        return [f"holdout_id {holdout_id} already used/opened; reused holdout IDs are rejected"]
    return []


def _open_window(holdout_id, window_start, window_end):
    PRECEDENT_WINDOWS.mkdir(parents=True, exist_ok=True)
    path = PRECEDENT_WINDOWS / (hashlib.sha256(holdout_id.encode()).hexdigest() + ".json")
    try:
        handle = path.open("x", encoding="utf-8")
    except FileExistsError:
        return False
    record = {
        "format_version": FORMAT,
        "holdout_id": holdout_id,
        "window_start": window_start,
        "window_end": window_end,
        "opened_at": datetime.now(timezone.utc).isoformat(),
    }
    with handle:
        json.dump(record, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    return True


def _seed_specs():
    """Synthetic mechanics cases for the shipped /4 seed.

    `ws_orbit:agent_request` scores seven requests: five covered, four correct,
    one abstention, one conflict, citation agreement 3/4. One leaked prediction,
    one superseded request and one pre-window model label are not scored.
    """
    common = {
        "category": SEED_CATEGORY,
        "topic": "requeue",
        "status": "answered",
        "answered_by": "human",
        "supports": True,
        "same_consequence": True,
        "conflict": False,
    }
    return [
        {**common, "case_id": "ar-correct-a", "raised_at": "2026-04-02T00:00:00Z",
         "answered_at": "2026-04-02T02:00:00Z", "sealed_at": "2026-04-02T01:00:00Z",
         "human": "approve", "predicted": "approve", "probability": 0.95},
        {**common, "case_id": "ar-correct-b", "raised_at": "2026-04-03T00:00:00Z",
         "answered_at": "2026-04-03T02:00:00Z", "sealed_at": "2026-04-03T01:00:00Z",
         "human": "approve", "predicted": "approve", "probability": 0.92},
        {**common, "case_id": "ar-wrong", "raised_at": "2026-04-04T00:00:00Z",
         "answered_at": "2026-04-04T02:00:00Z", "sealed_at": "2026-04-04T01:00:00Z",
         "human": "dismiss", "predicted": "approve", "probability": 0.91},
        {**common, "case_id": "ar-low", "raised_at": "2026-04-05T00:00:00Z",
         "answered_at": "2026-04-05T02:00:00Z", "sealed_at": "2026-04-05T01:00:00Z",
         "human": "approve", "predicted": "approve", "probability": 0.50},
        {**common, "case_id": "ar-abstain", "raised_at": "2026-04-06T00:00:00Z",
         "answered_at": "2026-04-06T02:00:00Z", "sealed_at": "2026-04-06T01:00:00Z",
         "human": "approve", "abstain": True},
        {**common, "case_id": "ar-conflict", "raised_at": "2026-04-07T00:00:00Z",
         "answered_at": "2026-04-07T02:00:00Z", "sealed_at": "2026-04-07T01:00:00Z",
         "human": "approve", "predicted": "approve", "probability": 0.97, "conflict": True},
        {**common, "case_id": "ar-cite", "raised_at": "2026-04-08T00:00:00Z",
         "answered_at": "2026-04-08T02:00:00Z", "sealed_at": "2026-04-08T01:00:00Z",
         "human": "approve", "predicted": "approve", "probability": 0.93,
         "same_consequence": False},
        {**common, "case_id": "ar-leaked", "raised_at": "2026-04-09T00:00:00Z",
         "answered_at": "2026-04-09T01:00:00Z", "sealed_at": "2026-04-09T02:00:00Z",
         "human": "approve", "predicted": "approve", "probability": 0.99},
        {**common, "case_id": "ar-superseded", "raised_at": "2026-04-10T00:00:00Z",
         "answered_at": "2026-04-10T02:00:00Z", "sealed_at": "2026-04-10T01:00:00Z",
         "status": "superseded", "human": "approve", "predicted": "dismiss", "probability": 0.2},
        {**common, "case_id": "ar-dev-model", "raised_at": "2026-03-15T00:00:00Z",
         "answered_at": "2026-03-15T02:00:00Z", "sealed_at": "2026-03-15T01:00:00Z",
         "answered_by": "model", "human": "approve", "predicted": "approve", "probability": 0.99},
        {**common, "case_id": "fr-correct", "category": SEED_OTHER, "topic": "requeue",
         "raised_at": "2026-04-12T00:00:00Z", "answered_at": "2026-04-12T02:00:00Z",
         "sealed_at": "2026-04-12T01:00:00Z", "human": "requeue", "predicted": "requeue",
         "probability": 0.95},
    ]


def _seed_label(spec):
    return {
        "provenance": "synthetic",
        "human_ground_truth": False,
        "answered_by": spec["answered_by"],
        "decided_by": "synthetic-author",
        "answered_at": spec["answered_at"],
        "channel": "synthetic",
        "automation": False,
        "value": spec["human"],
        "note": "synthetic expected label authored for offline seed; not human ground truth",
    }


def seed_precedent_dataset():
    cases = []
    for spec in _seed_specs():
        cases.append({
            "case_id": spec["case_id"],
            "consumer": CONSUMER,
            "category": spec["category"],
            "topic": spec["topic"],
            "raised_at": spec["raised_at"],
            "status": spec["status"],
            "expected": {"decision_choice": spec["human"]},
            "label": _seed_label(spec),
        })
    return {
        "format_version": FORMAT,
        "dataset_id": "synthetic-precedent-seed/1",
        "consumer": CONSUMER,
        "question_set": QUESTION_SET,
        "label_provenance": "synthetic",
        "mechanics_only": True,
        "cases": cases,
    }


def seed_precedent_answers():
    answers = []
    for spec in _seed_specs():
        row = {
            "case_id": spec["case_id"],
            "disposition": "suggested",
            "sealed_at": spec["sealed_at"],
            "precedents_conflict": spec["conflict"],
        }
        if spec.get("abstain"):
            row["answers"] = {"decision_choice": {"value": ABSTAIN}}
            row["citations"] = []
        else:
            row["answers"] = {
                "decision_choice": {"value": spec["predicted"], "probability": spec["probability"]},
            }
            row["citations"] = [{
                "precedent_id": "prec-" + spec["case_id"],
                "supports_choice": spec["supports"],
                "same_consequence": spec["same_consequence"],
            }]
        answers.append(row)
    return {"format_version": FORMAT, "answers": answers}


def _target_row(category):
    return {
        "category": category,
        "threshold": 0.9,
        "min_samples": SAMPLE_FLOOR,
        "min_accuracy": ACCURACY_FLOOR,
        "min_coverage": COVERAGE_FLOOR,
        "max_ece": ECE_CAP,
    }


def seed_precedent_targets():
    return {
        "format_version": FORMAT,
        "registered_at": SEED_REGISTERED,
        "window_start": SEED_WINDOW_START,
        "window_end": SEED_WINDOW_END,
        "policy_digest": _digest(POLICY_TEXT),
        "categories": [_target_row(SEED_CATEGORY), _target_row(SEED_OTHER)],
    }


def _path_arg(value, flag):
    if not isinstance(value, str) or not value:
        raise ValueError(f"{flag} requires a file path")
    return value


def precedent_cli(argv):
    if not _owns(argv):
        return None
    parser = argparse.ArgumentParser(
        prog="evaluate-decision-assistance.py",
        description="Offline precedent-decision evaluation (/4). No external calls.",
    )
    parser.add_argument(
        "command",
        choices=("precedent-schema", "seed", "validate", "score", "check-split", "check-targets"),
    )
    parser.add_argument("--format-version", choices=[FORMAT])
    parser.add_argument("--dataset")
    parser.add_argument("--manifest")
    parser.add_argument("--holdout-opened-at")
    parser.add_argument("--prior-holdout-ids", default="")
    parser.add_argument("--answers", nargs="?", const=True)
    parser.add_argument("--targets", nargs="?", const=True)
    args = parser.parse_args(argv)
    command = args.command
    try:
        if command == "precedent-schema":
            return _emit(precedent_schema())
        if command == "seed":
            if args.answers and args.targets:
                return _emit({"ok": False, "errors": ["seed accepts only one of --answers or --targets"]}, 2)
            if isinstance(args.answers, str) or isinstance(args.targets, str):
                return _emit(
                    {"ok": False, "errors": [
                        "seed --answers and seed --targets emit documents and do not take a path"]},
                    2,
                )
            if args.answers:
                return _emit(seed_precedent_answers())
            if args.targets:
                return _emit(seed_precedent_targets())
            return _emit(seed_precedent_dataset())
        if command == "check-targets":
            document = _load(_path_arg(args.targets, "--targets"))
            errors = check_precedent_targets(document, args.holdout_opened_at)
            return _emit({"ok": not errors, "errors": errors}, 0 if not errors else 2)
        dataset = _load(_path_arg(args.dataset, "--dataset"))
        if command == "check-split":
            manifest = _load(_path_arg(args.manifest, "--manifest"))
            prior = [item for item in args.prior_holdout_ids.split(",") if item]
            errors = _dedupe(check_precedent_split(dataset, manifest, prior))
            return _emit({"ok": not errors, "errors": errors}, 0 if not errors else 2)
        answer_doc = _load(_path_arg(args.answers, "--answers")) if isinstance(args.answers, str) else None
        target_doc = _load(_path_arg(args.targets, "--targets")) if isinstance(args.targets, str) else None
        if command == "validate":
            errors = validate_precedent_dataset(dataset)
            if answer_doc is not None:
                errors.extend(validate_precedent_answers(dataset, answer_doc))
            if target_doc is not None:
                errors.extend(check_precedent_targets(target_doc, args.holdout_opened_at))
                bounds = _window_bounds(target_doc)
                if bounds is not None:
                    errors.extend(window_label_errors(
                        dataset, bounds[0], bounds[1], require_recorded=False))
            errors = _dedupe(errors)
            return _emit({"ok": not errors, "errors": errors}, 0 if not errors else 2)
        if answer_doc is None or target_doc is None:
            return _emit({"ok": False, "errors": ["score requires --answers and --targets"]}, 2)
        report, errors = score_precedent(dataset, answer_doc, target_doc)
        if errors:
            return _emit({"ok": False, "errors": errors}, 2)
        return _emit(report)
    except (OSError, ValueError, RecursionError, UnicodeError, TypeError, KeyError) as error:
        return _emit({"ok": False, "errors": [str(error)]}, 2)
