"""Frozen, outcome-independent sampling and task-level ladder joins (offline only)."""
import hashlib
import json
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path

FORMAT = "decision-assistance-eval/3"
FRAME_VERSION = "delivery-quality-sample/1"
JOIN_VERSION = "delivery-quality-join/1"
ALGORITHM = "sha256-sort/1"
JOIN_FIELDS = (
    "owner", "workspace", "experiment_id", "trial_id", "assignment_arm",
    "segment_digest", "overlay_digest", "excluded", "forced", "live", "task_id",
    "implementation_attempt", "change_unit", "admission_tier", "admission_type",
    "actual_run_id", "actual_provider", "actual_model", "actual_effort",
    "configuration_fingerprint", "source_version", "rubric_version", "grader_version",
    "builder_version", "as_of", "attribution", "non_code",
)
IDENTITY_FIELDS = ("owner", "workspace", "task_id")
ACTUAL_FIELDS = ("implementation_attempt", "actual_run_id", "actual_provider",
                 "actual_model", "actual_effort", "configuration_fingerprint")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def content_digest(value):
    return "sha256:" + hashlib.sha256(canonical(value).encode()).hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def instant(value):
    require(isinstance(value, str), "timestamp must be a timezone-qualified ISO-8601 string")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(parsed.tzinfo is not None, "timestamp must have a timezone")
    return parsed


def nonempty(value):
    return isinstance(value, str) and bool(value.strip())


def is_digest(value):
    return (isinstance(value, str) and value.startswith("sha256:") and len(value) == 71
            and all(c in "0123456789abcdef" for c in value[7:]))


def task_key(case):
    return tuple(case["join"][key] for key in IDENTITY_FIELDS)


def case_binding(case):
    # Grades/labels never influence sampling; frozen host attribution stays bound.
    return content_digest({key: case[key] for key in
                           ("join", "evidence_packet", "applicability", "question_version", "family_id", "host_metadata")})


def validate_join(case, intake):
    join = case["join"]
    require(isinstance(join, dict) and set(join) == set(JOIN_FIELDS),
            "join must explicitly carry every versioned field (null means unavailable)")
    for key in JOIN_FIELDS:
        if key in ("excluded", "forced", "live", "non_code", "change_unit"):
            continue
        require(join[key] is None or nonempty(join[key]), f"join.{key} must be a string or null")
    for key in IDENTITY_FIELDS:
        require(nonempty(join[key]), f"join.{key} is required for task deduplication")
    for key in ("excluded", "forced", "live", "non_code"):
        require(join[key] is None or type(join[key]) is bool, f"join.{key} must be boolean or null (uncaptured)")
    for key in ("segment_digest", "overlay_digest", "configuration_fingerprint"):
        require(join[key] is None or is_digest(join[key]), f"join.{key} must be a digest or null")
    require(join["attribution"] in ("available", "shared", "partial", "unavailable"),
            "join.attribution must be available/shared/partial/unavailable")
    unit = join["change_unit"]
    require(unit is None or (isinstance(unit, dict) and set(unit) == {
        "base_revision", "candidate_revision", "landed_revision", "patch_digest"}
        and all(nonempty(unit[k]) for k in ("base_revision", "candidate_revision"))
        and (unit["landed_revision"] is None or nonempty(unit["landed_revision"]))
        and is_digest(unit["patch_digest"])), "change_unit needs immutable revisions and patch_digest, or null")
    if join["attribution"] == "available":
        require(unit is not None and all(nonempty(join[k]) for k in ACTUAL_FIELDS),
                "available attribution requires actual attempt/run/provider/model/effort/fingerprint")
    require(instant(join["as_of"]) <= instant(intake), "join.as_of exceeds intake")
    for key in ("source_version", "rubric_version", "grader_version", "builder_version"):
        require(nonempty(join[key]), f"join.{key} required; do not infer from current config")


def cohort(case):
    j = case["join"]
    # Preserve combinations instead of folding forced or hypothetical into an arm.
    return canonical({key: j[key] for key in ("excluded", "forced", "live", "non_code")})


def eligible(case):
    j = case["join"]
    return (j["excluded"] is False and j["forced"] is False and j["non_code"] is False and j["live"] is True
            and j["assignment_arm"] in ("candidate", "control")
            and all(j[k] is not None for k in ("experiment_id", "trial_id", "segment_digest", "overlay_digest")))


def sampling_core(dataset):
    p = dataset["population"]
    require(set(p) == {"version", "cohort_id", "epoch", "eligibility_watermark", "frozen_at",
                       "seed", "algorithm", "control_count", "source_version"}, "invalid population manifest fields")
    require(p["version"] == "delivery-quality-population/1" and p["algorithm"] == ALGORITHM,
            "unsupported population version or sampling algorithm")
    for key in ("cohort_id", "epoch", "eligibility_watermark", "seed", "source_version"):
        require(nonempty(p[key]), f"population.{key} required")
    require(type(p["control_count"]) is int and p["control_count"] >= 0,
            "population.control_count must be a nonnegative integer")
    require(instant(p["frozen_at"]) <= instant(dataset["intake_snapshot_at"]),
            "population frozen_at exceeds intake snapshot")
    cases = dataset["cases"]
    controls = [c for c in cases if eligible(c) and c["join"]["assignment_arm"] == "control"]
    def order(c):
        return (content_digest([ALGORITHM, p["seed"], p["cohort_id"], p["epoch"],
                                p["eligibility_watermark"], task_key(c)]), task_key(c))
    def stratum(c):
        return tuple(c["join"][k] for k in ("owner", "workspace", "experiment_id", "trial_id",
                     "segment_digest", "admission_tier", "admission_type", "source_version",
                     "rubric_version", "grader_version", "builder_version"))
    strata = defaultdict(list)
    for control in controls:
        strata[stratum(control)].append(control)
    selected_controls, probabilities = set(), {}
    for members in strata.values():
        count = min(p["control_count"], len(members))
        selected_controls.update(c["case_id"] for c in sorted(members, key=order)[:count])
        probabilities.update({c["case_id"]: count/len(members) for c in members})
    rows = []
    for c in sorted(cases, key=lambda c: task_key(c)):
        admitted = eligible(c)
        candidate = c["join"]["assignment_arm"] == "candidate"
        selected = admitted and (candidate or c["case_id"] in selected_controls)
        probability = (1.0 if candidate else probabilities[c["case_id"]]) if admitted else None
        rows.append({"case_id": c["case_id"], "task_key": list(task_key(c)),
                     "binding": case_binding(c), "eligible": admitted, "cohort": cohort(c),
                     "selected": selected, "sampling_probability": probability})
    return {"version": FRAME_VERSION, "population": p, "rows": rows}


def freeze_sample(dataset, output):
    core = sampling_core(dataset)
    path = Path(output)
    if path.exists():
        previous = json.loads(path.read_text())
        validate_sample(dataset, previous)
        return previous  # retry/restart never redraws or refreshes the receipt
    frame = {**core, "frame_digest": content_digest(core),
             "created_at": datetime.now(timezone.utc).isoformat()}
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x") as handle:
        handle.write(json.dumps(frame, indent=2, sort_keys=True) + "\n")
        handle.flush()
        import os
        os.fsync(handle.fileno())
    return frame


def validate_sample(dataset, frame):
    core = sampling_core(dataset)
    require(set(frame) == set(core) | {"frame_digest", "created_at"}, "invalid sample receipt fields")
    require(all(frame.get(k) == v for k, v in core.items())
            and frame["frame_digest"] == content_digest(core),
            "sample differs from frozen epoch/cohort/watermark/attempt/evidence; retry must reuse original frame")
    require(instant(frame["created_at"]) >= instant(core["population"]["frozen_at"]),
            "sample receipt predates population freeze")


def dimension_state(case, answer, dimension):
    j = case["join"]
    if j["attribution"] != "available":
        return "partial" if j["attribution"] == "partial" else "unattributed"
    if not all(j[k] is not None for k in ("admission_tier", "admission_type")):
        return "capture_unavailable"
    if case["applicability"][dimension]["status"] == "not_applicable":
        return "not_applicable"
    if case["applicability"][dimension]["status"] == "unavailable":
        return "applicability_unavailable"
    if answer is None:
        return "missing"
    if answer["disposition"] != "suggested":
        return answer.get("gap_reason") or answer["disposition"]
    packet = case["evidence_packet"]
    if packet["omissions"] or packet["truncated"] or packet["scope"] != "complete":
        return "partial"
    value = answer["answers"].get(dimension, {}).get("value")
    if value == "insufficient_evidence":
        return "insufficient_evidence"
    if value == "not_applicable":
        return "unapproved_not_applicable"
    return "valid_ordinal" if value is not None else "partial"


def ladder_export(dataset, answers, frame, dimensions):
    validate_sample(dataset, frame)
    by_id = {a["case_id"]: a for a in answers["answers"]}
    rows_by_id = {row["case_id"]: row for row in frame["rows"]}
    groups = {}
    units = defaultdict(list)
    for c in dataset["cases"]:
        unit = c["join"]["change_unit"]
        if unit is not None:
            units[content_digest(unit)].append(c["case_id"])
    shared_ids = {cid for ids in units.values() if len(ids) > 1 for cid in ids}
    exports = []
    for case in dataset["cases"]:
        j, row = case["join"], rows_by_id[case["case_id"]]
        # Group by comparability identities; never mix versions or segments.
        identity = {k: j[k] for k in ("owner", "workspace", "experiment_id", "trial_id",
                    "assignment_arm", "segment_digest", "overlay_digest", "admission_tier",
                    "admission_type", "source_version", "rubric_version", "grader_version", "builder_version")}
        identity["cohort"] = row["cohort"]
        key = canonical(identity)
        group = groups.setdefault(key, {"identity": identity, "enrolled": 0, "eligible": 0,
                                      "preselected": 0, "unsampled": 0, "dimensions": {}})
        group["enrolled"] += 1
        group["eligible"] += int(row["eligible"])
        group["preselected"] += int(row["selected"])
        group["unsampled"] += int(row["eligible"] and not row["selected"])
        states = {}
        for d in dimensions:
            state = "ineligible" if not row["eligible"] else "unsampled" if not row["selected"] else dimension_state(case, by_id.get(case["case_id"]), d)
            if row["selected"] and case["case_id"] in shared_ids:
                state = "unattributed"  # shared changes never vote once per task
            states[d] = state
            counts = group["dimensions"].setdefault(d, Counter())
            counts[state] += 1
            answer = by_id.get(case["case_id"])
            if row["selected"] and answer is not None:
                counts["assessment_recorded"] += 1
                counts["assessed"] += int(d in answer["answers"])
            if state == "valid_ordinal":
                grade = by_id[case["case_id"]]["answers"][d]["value"]
                counts[grade] += 1
                counts["bad_grade"] += int(grade in ("deficient", "weak"))
        answer = by_id.get(case["case_id"])
        exports.append({"case_id": case["case_id"], "join": j, "sample": row,
                        "dimension_states": states,
                        "assessment": {key: answer.get(key) for key in
                                       ("assessment_id", "disposition", "gap_reason", "answers")} if answer else None})
    for group in groups.values():
        for counts in group["dimensions"].values():
            # Approved N/A is removed only with available complete task attribution.
            counts["enrolled"] = group["enrolled"]
            counts["eligible"] = group["eligible"]
            counts["preselected"] = group["preselected"]
            counts["host_approved_not_applicable"] = counts["not_applicable"]
            counts["coverage_denominator"] = group["preselected"] - counts["not_applicable"]
            counts["coverage_numerator"] = counts["valid_ordinal"]
            counts["coverage"] = (counts["valid_ordinal"] / counts["coverage_denominator"]
                                  if counts["coverage_denominator"] else None)
            counts["bad_grade_denominator"] = counts["valid_ordinal"]
            counts["bad_grade_numerator"] = counts["bad_grade"]
            # Explicit zeroes distinguish measured absence from unavailable fields.
            for state in ("valid_ordinal", "not_applicable", "missing", "partial", "unattributed",
                          "applicability_unavailable", "capture_unavailable", "budget", "outage", "pending",
                          "oversize", "insufficient_evidence", "refused", "unavailable", "invalid",
                          "stale", "uncertain", "unsampled", "ineligible", "unapproved_not_applicable",
                          "deficient", "weak", "adequate", "strong", "exemplary", "bad_grade", "assessed", "assessment_recorded"):
                counts.setdefault(state, 0)
    return {"version": JOIN_VERSION, "sample_frame": frame, "groups": list(groups.values()),
            "rows": exports, "gate_grade_choices": ["deficient", "weak"],
            "authority": "offline counts only; no routing or adoption action"}
