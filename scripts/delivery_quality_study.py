"""Blinded labels and locally irreversible registrations/openings for delivery studies.

Receipts must travel with the study. Filesystem owners can delete local files;
this is exclusive append-only tooling, not a remote trust or access service.
"""
import copy
import json
import os
from datetime import datetime, timezone
from pathlib import Path

import delivery_quality_eval as delivery
import delivery_quality_sampling as sampling

require = sampling.require
LABEL_VERSION = "delivery-quality-labels/1"
EXPORT_VERSION = "delivery-quality-blind/1"
TARGET_VERSION = "delivery-quality-targets/1"
SPLIT_VERSION = "delivery-quality-split/1"
RECEIPTS = Path(".decision-assistance-eval") / "delivery"
UNCERTAINTY = "wilson95+family-bootstrap95/1"
STUDY_IDENTITY_FIELDS = ("question_version", "rubric_version", "grader_version", "builder_version", "source_version",
                         "cohort_id", "epoch", "eligibility_watermark", "sample_frame_digest")
TARGET_METRICS = ("min_exact", "min_adjacent", "min_weighted_agreement", "max_material_overgrading",
                  "max_material_undergrading", "min_coverage", "max_abstention", "max_human_disagreement",
                  "min_repeatability")


def validate_human_labels(case, intake):
    label = case["label"]
    synthetic = label["provenance"] == "synthetic"
    require(set(label) == {"provenance", "human_ground_truth", "intake_snapshot_at", "independent_labels", "adjudication"}, "invalid human label fields")
    require(label.get("human_ground_truth") is (not synthetic), "human_ground_truth must match label provenance")
    require(sampling.instant(label["intake_snapshot_at"]) == sampling.instant(intake), "label intake snapshot mismatch")
    originals = label.get("independent_labels")
    require(isinstance(originals, list) and len(originals) == 2, "retain two independent qualified human labels")
    ids = []
    def person(record, adjudicator=False):
        needed = {"labeler_id", "role", "kind", "qualified", "independent", "blinded", "labeled_at", "values"}
        if adjudicator:
            needed |= {"rationale_category"}
        require(isinstance(record, dict) and set(record) == needed, "invalid independent label/adjudication fields")
        require(sampling.nonempty(record["labeler_id"]) and sampling.nonempty(record["role"]), "human identity and qualification role required")
        require(record["kind"] == ("fixture_person" if synthetic else "human")
                and all(record[k] is True for k in ("qualified", "independent", "blinded")),
                "model/self/nonqualified/unblinded labels cannot be ground truth")
        require(record["labeler_id"] not in case["host_metadata"]["author_ids"], "self-labeling by change author forbidden")
        labeled = sampling.instant(record["labeled_at"])
        require(sampling.instant(case["evidence_packet"]["collected_at"]) <= labeled <= datetime.now(timezone.utc), "label timestamp before evidence or in future")
        delivery.validate_values(record["values"], case, "human label")
        if adjudicator:
            require(sampling.nonempty(record["rationale_category"]), "adjudication rationale category required")
        return record["labeler_id"]
    for record in originals:
        ids.append(person(record))
    require(len(set(ids)) == 2, "independent labels require distinct humans")
    adjudication = label.get("adjudication")
    if adjudication is not None:
        require(person(adjudication, True) not in ids, "adjudicator must be a third human")
        require(sampling.instant(adjudication["labeled_at"]) >= max(sampling.instant(r["labeled_at"]) for r in originals), "adjudication precedes independent labels")
    accepted = resolve_labels(label)
    require(case.get("expected") == accepted, "expected must retain consensus/adjudication or unresolved insufficient_evidence")


def resolve_labels(label):
    if label.get("adjudication"):
        return copy.deepcopy(label["adjudication"]["values"])
    first, second = label["independent_labels"]
    return {q: copy.deepcopy(first["values"][q]) if first["values"][q]["value"] == second["values"][q]["value"]
            else {"value": "insufficient_evidence", "evidence_refs": []} for q in delivery.QUESTIONS}


def blind_id(dataset, case):
    return sampling.content_digest([dataset["dataset_id"], case["case_id"], sampling.case_binding(case)])


def blind_export(dataset, frame):
    delivery.validate_delivery_dataset(dataset)
    sampling.validate_sample(dataset, frame)
    selected = {r["case_id"] for r in frame["rows"] if r["selected"]}
    cases = []
    for case in dataset["cases"]:
        if case["case_id"] not in selected:
            continue
        # Allowlist only; join, expected, labels, family, controls and host metadata stay local.
        packet = copy.deepcopy(case["evidence_packet"])
        packet.pop("subject_id")
        packet.pop("packet_id")
        cases.append({"blind_id": blind_id(dataset, case), "evidence_packet": packet,
                      "applicability": {q: {k: a[k] for k in ("status", "rule")} for q, a in case["applicability"].items()}})
    result = {"version": EXPORT_VERSION, "sample_digest": frame["frame_digest"],
              "intake_snapshot_at": dataset["intake_snapshot_at"], "schema": delivery.delivery_schema(), "cases": cases}
    result["export_digest"] = sampling.content_digest(result)
    return result


def import_labels(dataset, frame, imported):
    export = blind_export(dataset, frame)
    require(imported.get("version") == LABEL_VERSION and imported.get("export_digest") == export["export_digest"]
            and imported.get("sample_digest") == frame["frame_digest"], "label import blinding/sample binding mismatch")
    require(isinstance(imported.get("records"), list), "label import records must be a list")
    selected = {r["case_id"] for r in frame["rows"] if r["selected"]}
    result = copy.deepcopy(dataset)
    cases = {blind_id(dataset, c): c for c in result["cases"] if c["case_id"] in selected}
    seen = set()
    for record in imported["records"]:
        bid = record.get("blind_id")
        require(bid in cases and bid not in seen, "duplicate or unknown blinded label identity")
        seen.add(bid)
        require(set(record) == {"blind_id", "label"}, "label record cannot carry author/model answers or metadata")
        case = cases[bid]
        require(case["label"]["provenance"] != "independent_human", "independent labels are immutable; create a separately versioned study")
        label = record["label"]
        require(label.get("provenance") in ("synthetic", "independent_human"), "model labels are not human ground truth")
        require(not dataset.get("mechanics_only") or label["provenance"] == "synthetic", "synthetic fixtures cannot become human ground truth")
        case["label"] = copy.deepcopy(label)
        case["expected"] = resolve_labels(label)
        validate_human_labels(case, dataset["intake_snapshot_at"])
    delivery.validate_delivery_dataset(result)
    return result


def receipt_path(kind, purpose, holdout_id):
    require(purpose in ("human_evaluation", "synthetic_mechanics"), "study purpose required")
    require(sampling.nonempty(holdout_id), "holdout_id required")
    return RECEIPTS / purpose / kind / (sampling.content_digest(holdout_id)[7:] + ".json")


def read_receipt(kind, purpose, holdout_id):
    path = receipt_path(kind, purpose, holdout_id)
    if not path.exists():
        return None
    record = json.loads(path.read_text())
    body = {k: v for k, v in record.items() if k != "receipt_digest"}
    require(record.get("receipt_digest") == sampling.content_digest(body) and record.get("holdout_id") == holdout_id
            and record.get("purpose") == purpose and record.get("kind") == kind,
            "invalid or corrupted immutable study receipt")
    sampling.instant(record["recorded_at"])
    return record


def write_receipt(kind, purpose, holdout_id, payload):
    path = receipt_path(kind, purpose, holdout_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    record = {"version": "delivery-quality-receipt/1", "kind": kind, "purpose": purpose,
              "holdout_id": holdout_id, "recorded_at": datetime.now(timezone.utc).isoformat(), **payload}
    record["receipt_digest"] = sampling.content_digest(record)
    # Opening is exclusive and never undone even if a later write/read fails.
    with path.open("x") as handle:
        handle.write(json.dumps(record, indent=2, sort_keys=True) + "\n")
        handle.flush()
        os.fsync(handle.fileno())
    return record


def legacy_opened_at(holdout_id):
    """A legacy human holdout cannot be made fresh by changing format versions."""
    import hashlib
    path = Path(".decision-assistance-eval") / "holdouts" / (hashlib.sha256(holdout_id.encode()).hexdigest() + ".json")
    if not path.exists():
        return None
    record = json.loads(path.read_text())
    require(record.get("holdout_id") == holdout_id and record.get("format_version") in
            ("decision-assistance-eval/1", "decision-assistance-eval/2"), "invalid legacy holdout receipt")
    return sampling.instant(record["opened_at"])


def check_delivery_targets(doc, opened_at=None, register=False):
    require(doc.get("version") == TARGET_VERSION, "unsupported delivery target version")
    purpose, hid = doc["purpose"], doc["holdout_id"]
    if purpose == "human_evaluation":
        require(legacy_opened_at(hid) is None, "holdout already opened in legacy evaluation; register a fresh holdout")
    registration_time = sampling.instant(doc["registered_at"])
    require(registration_time <= datetime.now(timezone.utc), "target registration cannot be in future")
    identity = doc.get("study_identity")
    require(isinstance(identity, dict) and set(identity) == set(STUDY_IDENTITY_FIELDS)
            and all(sampling.nonempty(v) for v in identity.values())
            and sampling.is_digest(identity["sample_frame_digest"])
            and identity["question_version"] == delivery.QUESTION_VERSION
            and identity["rubric_version"] == delivery.RUBRIC_VERSION,
            "targets need frozen question/rubric/grader/builder/source/cohort/epoch/watermark/sample identity")
    baseline = doc.get("baseline")
    require(isinstance(baseline, dict) and sampling.nonempty(baseline.get("name"))
            and isinstance(baseline.get("measurements"), dict), "baseline name and explicit measurements/unknowns required")
    uncertainty = doc.get("uncertainty")
    require(isinstance(uncertainty, dict) and uncertainty.get("method") == UNCERTAINTY
            and type(uncertainty.get("max_interval_width")) in (int, float)
            and 0 < uncertainty["max_interval_width"] <= 1, "predeclare supported uncertainty method and maximum interval width")
    rows = doc.get("targets")
    require(isinstance(rows, list) and len(rows) == len(delivery.QUESTIONS)
            and {t.get("question") for t in rows} == set(delivery.QUESTIONS), "exactly six per-dimension targets required")
    for target in rows:
        require(type(target.get("min_samples")) is int and target["min_samples"] > 0, "predeclare positive per-dimension sample size")
        for key in TARGET_METRICS:
            v = target.get(key)
            require(type(v) in (int, float) and math_finite(v) and 0 <= v <= 1, f"predeclare {key} in [0,1]")
    require(set(doc.get("controls", [])) == set(delivery.CONTROLS), "predeclare all bias/adversarial/evidence controls")
    require(doc.get("comparison") == "absolute_ordinal", "pairwise comparisons require a separately versioned order-swap protocol")
    opening = read_receipt("openings", purpose, hid)
    registered = read_receipt("targets", purpose, hid)
    digest = sampling.content_digest(doc)
    if opening:
        require(registered is not None and registered["target_digest"] == digest, "targets registered/changed after holdout opening")
        require(registration_time < sampling.instant(opening["recorded_at"]), "targets registered after opening")
        require(not register, "holdout already opened; cannot register targets")
    if opened_at:
        require(registration_time < sampling.instant(opened_at), "targets registered after external holdout opening")
    if registered:
        require(registered["target_digest"] == digest, "targets already registered; new targets need a fresh holdout")
    elif register:
        registered = write_receipt("targets", purpose, hid, {"target_digest": digest, "targets": doc})
    return {"ok": True, "registered": registered is not None, "receipt": registered}


def math_finite(value):
    import math
    return math.isfinite(value)


def check_delivery_split(dataset, manifest, prior_ids=()):
    delivery.validate_delivery_dataset(dataset)
    require(manifest.get("version") == SPLIT_VERSION, "unsupported delivery split version")
    hid, purpose = manifest["holdout_id"], manifest["purpose"]
    if purpose == "human_evaluation":
        require(legacy_opened_at(hid) is None, "holdout already opened in legacy evaluation")
    require(hid not in prior_ids and hid not in manifest.get("previously_opened_holdout_ids", []), "reused holdout ID")
    require(read_receipt("openings", purpose, hid) is None, "holdout already opened/reused")
    registration = read_receipt("targets", purpose, hid)
    require(registration is not None, "register targets before opening holdout")
    require(manifest.get("target_digest") == registration["target_digest"], "split target registration mismatch")
    sampling.validate_sample(dataset, manifest["sample"])
    identity = registration["targets"]["study_identity"]
    require(identity["sample_frame_digest"] == manifest["sample"]["frame_digest"]
            and identity["question_version"] == dataset["question_version"]
            and all(identity[k] == dataset["population"][k] for k in ("cohort_id", "epoch", "eligibility_watermark"))
            and all(all(c["join"][k] == identity[k] for k in ("rubric_version", "grader_version", "builder_version", "source_version")) for c in dataset["cases"]),
            "holdout source/grader/epoch/sample does not match registered study")
    require(sampling.instant(manifest["intake_snapshot_at"]) == sampling.instant(dataset["intake_snapshot_at"]), "manifest intake snapshot mismatch")
    require(manifest.get("split_strategy") == "family_and_time/1", "fresh holdout must be both family and time separated")
    cases = {c["case_id"]: c for c in dataset["cases"]}
    assignments = manifest.get("assignments")
    require(isinstance(assignments, dict) and set(assignments) == set(cases)
            and set(assignments.values()) <= {"train", "dev", "holdout"}, "split must assign every case exactly once")
    hold = [c for cid, c in cases.items() if assignments[cid] == "holdout"]
    dev = [c for cid, c in cases.items() if assignments[cid] != "holdout"]
    require(bool(hold) and bool(dev), "development and fresh holdout identities required")
    for identity in (lambda c: c["family_id"], lambda c: c["evidence_packet"]["subject_id"], sampling.task_key):
        require(not {identity(c) for c in hold} & {identity(c) for c in dev}, "family/subject/task leakage across holdout")
    require(min(sampling.instant(c["evidence_packet"]["collected_at"]) for c in hold)
            > max(sampling.instant(c["evidence_packet"]["collected_at"]) for c in dev), "time leakage across holdout")
    if purpose == "human_evaluation":
        require(not dataset.get("mechanics_only"), "synthetic fixtures are not human ground truth")
        for case in hold:
            require(case["label"]["provenance"] == "independent_human", "synthetic/model ground truth refused")
            validate_human_labels(case, dataset["intake_snapshot_at"])
    else:
        require(dataset.get("mechanics_only") is True and all(c["label"]["provenance"] == "synthetic" for c in hold), "fixture openings only admit explicit synthetic mechanics")
    # Reusing the same holdout family/task under another ID is also refused locally.
    identities = sorted({sampling.content_digest(["family", c["family_id"]]) for c in hold}
                        | {sampling.content_digest(["task", sampling.task_key(c)]) for c in hold}
                        | {sampling.content_digest(["subject", c["evidence_packet"]["subject_id"]]) for c in hold})
    directory = receipt_path("openings", purpose, hid).parent
    directory.mkdir(parents=True, exist_ok=True)
    # Serialize cross-ID reuse checks as well as exclusive same-ID creation.
    # flock is available on the supported Unix development hosts.
    import fcntl
    with (directory / ".opening-lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for path in directory.glob("*.json"):
            old = json.loads(path.read_text())
            checked = read_receipt("openings", purpose, old["holdout_id"])
            require(path == receipt_path("openings", purpose, old["holdout_id"]), "opening receipt path mismatch")
            require(not set(checked["holdout_identities"]) & set(identities), "holdout identities already opened under another ID")
        receipt = write_receipt("openings", purpose, hid, {
            "target_digest": registration["target_digest"], "target_receipt_digest": registration["receipt_digest"],
            "dataset_digest": sampling.content_digest(dataset), "manifest_digest": sampling.content_digest(manifest),
            "holdout_identities": identities, "accuracy_eligible": purpose == "human_evaluation"})
    samples = {q: sum(c["applicability"][q]["status"] == "applicable"
                          and (c.get("expected") or {}).get(q, {}).get("value") in delivery.GRADES for c in hold)
               for q in delivery.QUESTIONS}
    return {"ok": True, "accuracy_eligible": purpose == "human_evaluation", "receipt": receipt,
            "per_dimension_holdout_samples": samples,
            "sample_sizes_met": {t["question"]: samples[t["question"]] >= t["min_samples"] for t in registration["targets"]["targets"]},
            "sample_size_note": "opening is access, not a claim that targets/sample sizes were met"}
