#!/usr/bin/env python3
"""Offline evaluation contracts and scoring for bounded decision assistance.

Stdlib only, no network. See docs/design/decision-assistance/2_evaluation.md.
Formats are versioned (`decision-assistance-eval/1` frozen legacy,
`decision-assistance-eval/2` current for human holdout eligibility);
synthetic labels are never human ground truth (label.provenance
distinguishes them). V1 second-label holdouts without a recorded first
value are legacy/unverified and never eligible; v2 records the first
value so rewriting `expected` cannot hide disagreement.
"""
import argparse, hashlib, json, math, os, sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path

FORMAT_V1 = "decision-assistance-eval/1"
FORMAT_V2 = "decision-assistance-eval/2"
FORMAT = FORMAT_V1
SUPPORTED_FORMATS = (FORMAT_V1, FORMAT_V2)
HOLDOUT_RECORDS = Path(".decision-assistance-eval") / "holdouts"
DISPOSITIONS = ["suggested", "uncertain", "refused", "unavailable", "invalid", "stale"]
FAILURE_CATEGORIES = ["product_defect", "misclassified_hold", "provider", "upstream_quality",
                      "environment", "one_off", "insufficient_evidence"]
TASK_PREDICATES = ["outcome_unclear", "criterion_not_observable", "requirements_conflict",
                   "scope_clarification"]
QUESTION_SETS = {
    "failure_categorization": {"version": "failure-questions/1", "questions": {
        "failure_category": {"type": "choice", "allowed": FAILURE_CATEGORIES,
                             "rubric": "Pick the single category the bounded evidence best supports; "
                                       "use insufficient_evidence when evidence is missing, mixed or ambiguous. "
                                       "A provider error alone proves no fallback outcome; one occurrence "
                                       "alone proves no one_off."}}},
    "task_quality": {"version": "task-quality-questions/1", "questions": {
        q: {"type": "predicate", "rubric": r} for q, r in [
            ("outcome_unclear", "True only if the whole task names no observable outcome. Valid investigations delivering evidence are not unclear."),
            ("criterion_not_observable", "True per criterion that names no observable outcome; cite criterion_refs."),
            ("requirements_conflict", "True only for criterion pairs that cannot both hold; cite both refs."),
            ("scope_clarification", "True only if unrelated deliverables lack a common outcome. Coherent cross-component work is not flagged.")]}},
}
PROVENANCES = ["synthetic", "independent_human"]
HUMAN_LABEL_IMPORT_FIELDS_V1 = (
    "labeler_id",
    "labeler_role",
    "labeled_at",
    "intake_snapshot_at",
    "second_label_or_adjudication",
)
HUMAN_LABEL_IMPORT_FIELDS_V2 = (
    "labeler_id",
    "labeler_role",
    "labeled_at",
    "intake_snapshot_at",
    "value",
    "second_label_or_adjudication",
)

def digest(text): return "sha256:" + hashlib.sha256(text.encode()).hexdigest()
def crit_ref(index, text): return {"index": index, "text_digest": digest(text)}
def is_digest(v): return isinstance(v, str) and v.startswith("sha256:") and len(v) == 71 and all(c in "0123456789abcdef" for c in v[7:])

def _ts(value):
    """Parse an ISO-8601 instant, or return None for invalid/naive input."""
    if not isinstance(value, str):
        return None
    try:
        timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return timestamp if timestamp.tzinfo is not None else None

def _validate_human_label_import(label, case_id, case_expected, manifest_snapshot,
                               dataset_version=None):
    errs = []
    where = f"holdout case {case_id}: label"
    is_v2 = dataset_version == FORMAT_V2
    required = HUMAN_LABEL_IMPORT_FIELDS_V2 if is_v2 else HUMAN_LABEL_IMPORT_FIELDS_V1
    missing = [field for field in required if field not in label]
    if missing:
        errs.append(f"{where} missing required import field(s): {', '.join(missing)}")

    for field in ("labeler_id", "labeler_role"):
        if not isinstance(label.get(field), str) or not label[field].strip():
            errs.append(f"{where}.{field} must be a non-empty string")
    for field in ("labeled_at", "intake_snapshot_at"):
        if not _ts(label.get(field, "")):
            errs.append(f"{where}.{field} must be ISO-8601")
    if _ts(label.get("intake_snapshot_at", "")) and _ts(manifest_snapshot):
        if _ts(label["intake_snapshot_at"]) != _ts(manifest_snapshot):
            errs.append(f"{where}.intake_snapshot_at must match manifest intake_snapshot_at")

    record = label.get("second_label_or_adjudication")
    if not isinstance(record, dict):
        if "second_label_or_adjudication" in label:
            errs.append(f"{where}.second_label_or_adjudication must be an object")
        return errs

    kind = record.get("kind")
    if kind == "second_independent_label":
        record_where = f"{where}.second_label_or_adjudication"
        for field in ("labeler_id", "labeler_role"):
            if not isinstance(record.get(field), str) or not record[field].strip():
                errs.append(f"{record_where}.{field} must be a non-empty string")
        if record.get("labeler_id") == label.get("labeler_id"):
            errs.append(f"{record_where}.labeler_id must identify a second independent labeler")
        if not _ts(record.get("labeled_at", "")):
            errs.append(f"{record_where}.labeled_at must be ISO-8601")
        if "value" not in record:
            errs.append(f"{record_where}.value is required")
        elif not is_v2:
            errs.append(f"{record_where} is a legacy v1 second label without a recorded first value; "
                        f"independent agreement cannot be verified from expected and it is not proven "
                        f"independent human ground truth; re-author as {FORMAT_V2} with recorded "
                        f"first/second values")
        elif "value" not in label:
            errs.append(f"{where}.value is required to verify independent agreement")
        elif record["value"] != label["value"]:
            errs.append(f"{record_where} disagrees with the first label; an adjudication record is required")
        elif record["value"] != case_expected:
            errs.append(f"{record_where} disagrees with the accepted label; an adjudication record is required")
    elif kind == "adjudication":
        record_where = f"{where}.second_label_or_adjudication"
        for field in ("adjudicator_id", "adjudicator_role", "rationale_category"):
            if not isinstance(record.get(field), str) or not record[field].strip():
                errs.append(f"{record_where}.{field} must be a non-empty string")
        if not _ts(record.get("adjudicated_at", "")):
            errs.append(f"{record_where}.adjudicated_at must be ISO-8601")
        originals = record.get("original_labels")
        if not isinstance(originals, list) or len(originals) != 2:
            errs.append(f"{record_where}.original_labels must retain both independent labels")
        else:
            original_ids = []
            for index, original in enumerate(originals):
                original_where = f"{record_where}.original_labels[{index}]"
                if not isinstance(original, dict):
                    errs.append(f"{original_where} must be an object")
                    continue
                for field in ("labeler_id", "labeler_role"):
                    if not isinstance(original.get(field), str) or not original[field].strip():
                        errs.append(f"{original_where}.{field} must be a non-empty string")
                if not _ts(original.get("labeled_at", "")):
                    errs.append(f"{original_where}.labeled_at must be ISO-8601")
                if "value" not in original:
                    errs.append(f"{original_where}.value is required")
                elif (is_v2 and "value" in label and original.get("labeler_id") == label.get("labeler_id")
                      and original["value"] != label["value"]):
                    errs.append(f"{original_where}.value must match the imported first label value")
                if isinstance(original.get("labeler_id"), str):
                    original_ids.append(original["labeler_id"])
            if len(original_ids) == 2 and len(set(original_ids)) != 2:
                errs.append(f"{record_where}.original_labels must come from two independent labelers")
            if label.get("labeler_id") not in original_ids:
                errs.append(f"{record_where}.original_labels must include labeler_id")
            if record.get("adjudicator_id") in original_ids:
                errs.append(f"{record_where}.adjudicator_id must identify a third adjudicator")
        if "value" not in record:
            errs.append(f"{record_where}.value is required")
        elif record["value"] != case_expected:
            errs.append(f"{record_where}.value must match the accepted label")
    else:
        errs.append(f"{where}.second_label_or_adjudication.kind must be second_independent_label or adjudication")
    return errs

def validate_packet(p, errs, where):
    if not isinstance(p, dict): errs.append(f"{where}: evidence_packet must be an object"); return
    for k in ("packet_id", "subject_id", "collected_at", "evidence_refs"):
        if k not in p: errs.append(f"{where}: evidence_packet missing {k}")
    if not _ts(p.get("collected_at", "")): errs.append(f"{where}: evidence_packet.collected_at must be ISO-8601")
    if not isinstance(p.get("evidence_refs"), list): errs.append(f"{where}: evidence_refs must be a list")
    for k in ("omissions", "truncated"):
        if k not in p: errs.append(f"{where}: evidence_packet missing {k} (omissions/truncation must be explicit)")

def validate_answer_value(qdef, ans, errs, where):
    if not isinstance(ans, dict) or "value" not in ans: errs.append(f"{where}: answer must be an object with value"); return
    v = ans["value"]
    if qdef["type"] == "choice" and v not in qdef["allowed"]: errs.append(f"{where}: value {v!r} not in allowed {qdef['allowed']}")
    if qdef["type"] == "predicate" and not isinstance(v, bool): errs.append(f"{where}: predicate value must be boolean")
    if "probability" in ans:
        pr = ans["probability"]
        if not isinstance(pr, (int, float)) or isinstance(pr, bool) or not math.isfinite(pr) or not 0 <= pr <= 1:
            errs.append(f"{where}: probability must be finite in [0,1]")
    refs = ans.get("criterion_refs", [])
    if "criterion_refs" in ans and not isinstance(refs, list):
        errs.append(f"{where}: criterion_refs must be a list")
    else:
        for r in refs:
            if not isinstance(r, dict) or not isinstance(r.get("index"), int) or not is_digest(r.get("text_digest", "")):
                errs.append(f"{where}: criterion_refs entries need integer index and sha256 text_digest")

def validate_dataset(ds):
    errs = []
    if not isinstance(ds, dict) or ds.get("format_version") not in SUPPORTED_FORMATS: return [f"format_version must be one of {', '.join(SUPPORTED_FORMATS)}"]
    if not _ts(ds.get("intake_snapshot_at", "")): errs.append("intake_snapshot_at must be ISO-8601")
    cases_list = ds.get("cases", [])
    if not isinstance(cases_list, list):
        return errs + ["cases must be a list"]
    seen = set()
    for c in cases_list:
        if not isinstance(c, dict):
            errs.append("case <unknown>: case must be an object")
            continue
        w = f"case {c.get('case_id')}"
        if c.get("case_id") in seen: errs.append(f"{w}: duplicate case_id")
        seen.add(c.get("case_id"))
        if c.get("consumer") not in QUESTION_SETS: errs.append(f"{w}: unknown consumer {c.get('consumer')!r}"); continue
        if not c.get("family_id"): errs.append(f"{w}: family_id required (split leakage key)")
        lab = c.get("label", {})
        if lab.get("provenance") not in PROVENANCES: errs.append(f"{w}: label.provenance must be one of {PROVENANCES}")
        if lab.get("provenance") == "synthetic" and lab.get("human_ground_truth"):
            errs.append(f"{w}: synthetic labels must never be marked human_ground_truth")
        validate_packet(c.get("evidence_packet"), errs, w)
        raw_ep = c.get("evidence_packet")
        ep = raw_ep if isinstance(raw_ep, dict) else {}
        if _ts(ep.get("collected_at", "")) and _ts(ds.get("intake_snapshot_at", "")) and _ts(ep["collected_at"]) > _ts(ds["intake_snapshot_at"]):
            errs.append(f"{w}: evidence collected after intake_snapshot_at")
        # criteria digests must match text
        raw_criteria = ep.get("criteria", [])
        criteria_list = raw_criteria if isinstance(raw_criteria, list) else []
        if "criteria" in ep and not isinstance(raw_criteria, list):
            errs.append(f"{w}: evidence_packet.criteria must be a list")
        if c.get("consumer") == "task_quality":
            if "criteria" not in ep:
                errs.append(f"{w}: task_quality evidence_packet must carry non-empty criteria")
            elif isinstance(raw_criteria, list) and len(criteria_list) == 0:
                errs.append(f"{w}: task_quality evidence_packet must carry non-empty criteria")
        crits = {cr.get("index"): cr for cr in criteria_list if isinstance(cr, dict)}
        for idx, cr in crits.items():
            if "text" in cr and cr.get("text_digest") != digest(cr["text"]): errs.append(f"{w}: criterion {idx} text_digest mismatch")
        exp = c.get("expected")
        if not isinstance(exp, dict): errs.append(f"{w}: expected answers required"); continue
        want = QUESTION_SETS[c["consumer"]]["questions"]
        if set(exp) != set(want): errs.append(f"{w}: expected must contain exactly {sorted(want)}")
        for q, a in exp.items():
            if q in want: validate_answer_value(want[q], a, errs, f"{w}.{q}")
            refs = a.get("criterion_refs", []) if isinstance(a, dict) else []
            if isinstance(refs, list):
                for r in refs:
                    if not isinstance(r, dict):
                        continue
                    ref_index = r.get("index")
                    ref_digest = r.get("text_digest")
                    if not isinstance(ref_index, int) or not is_digest(ref_digest):
                        continue
                    if ref_index not in crits:
                        errs.append(f"{w}.{q}: criterion_ref index {ref_index} not in packet criteria")
                    elif ref_digest != crits[ref_index].get("text_digest"):
                        errs.append(f"{w}.{q}: criterion_ref digest does not match packet criterion")
    return errs

def validate_answers(ds, ansdoc):
    errs = []
    if not isinstance(ansdoc, dict) or ansdoc.get("format_version") not in SUPPORTED_FORMATS: return [f"answers format_version must be one of {', '.join(SUPPORTED_FORMATS)}"]
    raw_cases = ds.get("cases", []) if isinstance(ds, dict) else []
    cases = {c.get("case_id"): c for c in raw_cases if isinstance(c, dict)}
    raw_answers = ansdoc.get("answers", [])
    if not isinstance(raw_answers, list):
        return errs + ["answers must be a list"]
    seen = set()
    for a in raw_answers:
        if not isinstance(a, dict):
            errs.append("answer <unknown>: answer must be an object")
            continue
        w = f"answer {a.get('case_id')}"
        if a.get("case_id") in seen: errs.append(f"{w}: duplicate answer")
        seen.add(a.get("case_id"))
        case = cases.get(a.get("case_id"))
        if case is None: errs.append(f"{w}: unknown case_id"); continue
        if not isinstance(case, dict) or case.get("consumer") not in QUESTION_SETS:
            errs.append(f"{w}: unknown consumer for case {a.get('case_id')!r}"); continue
        if a.get("disposition") not in DISPOSITIONS: errs.append(f"{w}: disposition must be one of {DISPOSITIONS}"); continue
        raw_got = a.get("answers") or {}
        if not isinstance(raw_got, dict):
            errs.append(f"{w}: answers must be an object")
            continue
        got = raw_got
        if a["disposition"] in ("refused", "unavailable"):
            if got: errs.append(f"{w}: refused/unavailable answers must carry no question answers")
            continue
        want = QUESTION_SETS[case["consumer"]]["questions"]
        if set(got) != set(want): errs.append(f"{w}: answers must contain exactly {sorted(want)}, got {sorted(got)}")
        raw_packet = case.get("evidence_packet")
        packet = raw_packet if isinstance(raw_packet, dict) else {}
        raw_criteria = packet.get("criteria", [])
        criteria_list = raw_criteria if isinstance(raw_criteria, list) else []
        crits = {cr.get("index"): cr for cr in criteria_list if isinstance(cr, dict)}
        for q, v in got.items():
            if q in want: validate_answer_value(want[q], v, errs, f"{w}.{q}")
            refs = v.get("criterion_refs", []) if isinstance(v, dict) else []
            if not isinstance(refs, list):
                continue
            for r in refs:
                if not isinstance(r, dict):
                    continue
                ref_index = r.get("index")
                ref_digest = r.get("text_digest")
                if not isinstance(ref_index, int) or not is_digest(ref_digest):
                    continue
                if ref_index not in crits:
                    errs.append(f"{w}.{q}: criterion_ref index {ref_index} not in packet criteria")
                elif ref_digest != crits[ref_index].get("text_digest"):
                    errs.append(f"{w}.{q}: criterion_ref digest does not match packet criterion")
        for k in ("latency_ms", "cost_usd"):
            if k in a and a[k] is not None and (not isinstance(a[k], (int, float, dict)) or (isinstance(a[k], (int, float)) and a[k] < 0)):
                errs.append(f"{w}: {k} must be non-negative or null (unknown)")
        if "usage" in a and a["usage"] is not None:
            usage = a["usage"]
            if not isinstance(usage, dict):
                errs.append(f"{w}: usage must be an object or null (unknown)")
            else:
                for key in ("input_tokens", "output_tokens"):
                    if key in usage and usage[key] is not None:
                        count = usage[key]
                        if isinstance(count, bool) or not isinstance(count, int) or count < 0:
                            errs.append(f"{w}: usage.{key} must be a non-negative integer or null (unknown)")
    return errs

def _pr(tp, fp, fn):
    prec = tp / (tp + fp) if tp + fp else None
    rec = tp / (tp + fn) if tp + fn else None
    f1 = (2 * prec * rec / (prec + rec)) if prec and rec else (0.0 if prec == 0 or rec == 0 else None)
    return {"precision": prec, "recall": rec, "f1": f1, "tp": tp, "fp": fp, "fn": fn}

def score(ds, ansdoc):
    try:
        errs = validate_dataset(ds) + validate_answers(ds, ansdoc)
    except Exception as error:
        errs = [f"validation failed: {error}"]
    raw_cases = ds.get("cases", []) if isinstance(ds, dict) else []
    cases = {c.get("case_id"): c for c in raw_cases if isinstance(c, dict)}
    raw_ans = ansdoc.get("answers", []) if isinstance(ansdoc, dict) else []
    if not isinstance(raw_ans, list):
        raw_ans = []
    by_id = {a.get("case_id"): a for a in raw_ans if isinstance(a, dict)}
    report_version = ds.get("format_version") if isinstance(ds, dict) and ds.get("format_version") in SUPPORTED_FORMATS else FORMAT
    report = {"format_version": report_version, "dataset_id": ds.get("dataset_id") if isinstance(ds, dict) else None, "validation_errors": errs,
              "questions": {}, "totals": {}}
    total = answered = abstained = invalid = refused = 0
    lat, costs, in_tok, out_tok = [], [], [], []
    usage_known = {"latency": True, "cost": True, "input_tokens": True, "output_tokens": True}
    # Usage belongs to each reported call, including calls omitted from case scoring.
    for a in raw_ans:
        if not isinstance(a, dict):
            usage_known["latency"] = usage_known["cost"] = False
            usage_known["input_tokens"] = usage_known["output_tokens"] = False
            continue
        latency = a.get("latency_ms")
        if (isinstance(latency, (int, float)) and not isinstance(latency, bool) and latency >= 0
                and (not isinstance(latency, float) or math.isfinite(latency))):
            lat.append(latency)
        else:
            usage_known["latency"] = False
        cost = a.get("cost_usd")
        if (isinstance(cost, (int, float)) and not isinstance(cost, bool) and cost >= 0
                and (not isinstance(cost, float) or math.isfinite(cost))):
            costs.append(cost)
        else:
            usage_known["cost"] = False
        usage = a.get("usage")
        for key, values in (("input_tokens", in_tok), ("output_tokens", out_tok)):
            count = usage.get(key) if isinstance(usage, dict) else None
            if isinstance(count, int) and not isinstance(count, bool) and count >= 0:
                values.append(count)
            else:
                usage_known[key] = False
    # per-question accumulators
    choice_conf = defaultdict(Counter); choice_counts = defaultdict(lambda: [0, 0, 0])  # tp,fp,fn
    pred_stats = {}
    def record_abstained_expected_labels(case):
        if not isinstance(case, dict):
            return
        exp = case.get("expected")
        if not isinstance(exp, dict):
            return
        if case.get("consumer") == "failure_categorization":
            fc = exp.get("failure_category")
            if not isinstance(fc, dict) or "value" not in fc:
                return
            expected = fc["value"]
            if not isinstance(expected, str):
                return
            if expected != "insufficient_evidence":
                try:
                    choice_counts[expected][2] += 1
                except TypeError:
                    return
        elif case.get("consumer") == "task_quality":
            for question, expected in exp.items():
                if isinstance(expected, dict) and expected.get("value") is True:
                    pred_stats.setdefault(question, [0, 0, 0, 0])[2] += 1

    for cid, case in cases.items():
        total += 1
        a = by_id.get(cid)
        try:
            answer_invalid = a is None or not isinstance(a, dict) or a.get("disposition") not in DISPOSITIONS or bool(validate_answers(ds, {"format_version": report_version, "answers": [a]}))
        except Exception:
            answer_invalid = True
        if answer_invalid:
            invalid += 1
            abstained += 1
            record_abstained_expected_labels(case)
            continue
        if not isinstance(case, dict) or not isinstance(case.get("expected"), dict):
            invalid += 1
            abstained += 1
            continue
        if a["disposition"] in ("refused", "unavailable"):
            refused += 1
            abstained += 1
            record_abstained_expected_labels(case)
            continue
        if a["disposition"] != "suggested":
            abstained += 1
            record_abstained_expected_labels(case)
            continue
        raw_got = a.get("answers") or {}
        got = raw_got if isinstance(raw_got, dict) else {}
        case_answered = True
        for q, exp in case["expected"].items():
            if not isinstance(exp, dict) or "value" not in exp:
                continue
            pred = got.get(q, {})
            if not isinstance(pred, dict):
                pred = {}
            ev, pv = exp["value"], pred.get("value")
            if case["consumer"] == "failure_categorization":
                if pv == "insufficient_evidence":
                    case_answered = False
                    record_abstained_expected_labels(case)
                    continue
                try:
                    choice_conf[ev][pv] += 1
                except TypeError:
                    continue
                for cls in FAILURE_CATEGORIES:
                    if cls == "insufficient_evidence": continue
                    if ev == cls and pv == cls: choice_counts[cls][0] += 1
                    elif pv == cls and ev != cls: choice_counts[cls][1] += 1
                    elif ev == cls and pv != cls: choice_counts[cls][2] += 1
            else:
                s = pred_stats.setdefault(q, [0, 0, 0, 0])  # tp,fp,fn,tn
                if ev and pv: s[0] += 1
                elif pv and not ev: s[1] += 1
                elif ev and not pv: s[2] += 1
                else: s[3] += 1
        if case_answered: answered += 1
        else: abstained += 1
    for cls in FAILURE_CATEGORIES:
        if cls == "insufficient_evidence": continue
        tp, fp, fn = choice_counts[cls]
        report["questions"].setdefault("failure_category", {"type": "choice", "per_class": {}, "confusion_matrix": {k: dict(v) for k, v in choice_conf.items()}})["per_class"][cls] = _pr(tp, fp, fn)
    for q, (tp, fp, fn, tn) in pred_stats.items():
        r = _pr(tp, fp, fn); r["tn"] = tn
        r["confusion_matrix"] = {"true": {"true": tp, "false": fn}, "false": {"true": fp, "false": tn}}
        r["false_positive_burden"] = fp / (tp + fp) if tp + fp else None
        report["questions"][q] = {"type": "predicate", **r}
    if "failure_category" in report["questions"]:
        fp_total = sum(v[1] for v in choice_counts.values()); sug = sum(sum(v.values()) for v in choice_conf.values())
        report["questions"]["failure_category"]["false_positive_burden"] = (fp_total / sug) if sug else None
    def agg(vals, known): return ({"status": "known", "total": sum(vals), "mean": (sum(vals) / len(vals) if vals else None), "n": len(vals)} if known and vals else {"status": "unknown", "total": None, "mean": None, "n": len(vals)})
    report["totals"] = {"cases": total, "answered": answered, "abstained": abstained, "invalid": invalid,
                        "refused_or_unavailable": refused, "coverage": (answered / total if total else None),
                        "abstention_rate": (abstained / total if total else None),
                        "latency_ms": agg(lat, usage_known["latency"]), "cost_usd": agg(costs, usage_known["cost"]),
                        "input_tokens": agg(in_tok, usage_known["input_tokens"]), "output_tokens": agg(out_tok, usage_known["output_tokens"])}
    return report

def _holdout_timestamp(value):
    timestamp = _ts(value)
    if timestamp is None:
        raise ValueError("holdout timestamps must be ISO 8601 strings with a timezone")
    return timestamp

def _record_holdout_opening(holdout_id, format_version=None):
    HOLDOUT_RECORDS.mkdir(parents=True, exist_ok=True)
    # Exclusive creation arbitrates concurrent checks without overwriting history.
    path = HOLDOUT_RECORDS / (hashlib.sha256(holdout_id.encode()).hexdigest() + ".json")
    try:
        record = path.open("x", encoding="utf-8")
    except FileExistsError:
        return False
    version = format_version if format_version in SUPPORTED_FORMATS else FORMAT_V1
    with record:
        json.dump({"format_version": version, "holdout_id": holdout_id,
                   "opened_at": datetime.now(timezone.utc).isoformat()}, record)
        record.write("\n")
        record.flush()
        os.fsync(record.fileno())
    return True

def _target_recorded_opening(doc):
    try:
        paths = list(HOLDOUT_RECORDS.iterdir())
    except FileNotFoundError:
        paths = []
    openings = {}
    for path in paths:
        if path.suffix != ".json":
            continue
        record = load(path)
        if not isinstance(record, dict) or record.get("format_version") not in SUPPORTED_FORMATS:
            raise ValueError(f"invalid holdout record {path}")
        hid = record.get("holdout_id")
        if not isinstance(hid, str) or not hid.strip():
            raise ValueError(f"invalid holdout_id in {path}")
        if path.stem != hashlib.sha256(hid.encode()).hexdigest():
            raise ValueError(f"holdout record filename does not match holdout_id in {path}")
        openings[hid] = _holdout_timestamp(record.get("opened_at"))
    hid = doc.get("holdout_id")
    if hid is not None:
        if not isinstance(hid, str) or not hid.strip():
            raise ValueError("targets holdout_id must be a non-empty string")
        return openings.get(hid)
    if len(openings) > 1:
        raise ValueError("targets holdout_id required when multiple holdouts have been opened")
    return next(iter(openings.values()), None)

def check_split(ds, manifest, prior_holdout_ids):
    try:
        errs = validate_dataset(ds)
    except Exception as error:
        errs = [f"validation failed: {error}"]
    if not isinstance(manifest, dict):
        return errs + ["manifest must be an object"]
    if manifest.get("format_version") not in SUPPORTED_FORMATS: errs.append(f"manifest format_version must be one of {', '.join(SUPPORTED_FORMATS)}")
    hid = manifest.get("holdout_id")
    if not isinstance(hid, str) or not hid.strip(): errs.append("manifest holdout_id must be a non-empty string")
    if hid in (prior_holdout_ids or []): errs.append(f"holdout_id {hid} already used/opened; reused holdout IDs are rejected")
    prev_opened = manifest.get("previously_opened_holdout_ids", [])
    if isinstance(prev_opened, list) and hid in prev_opened: errs.append(f"holdout_id {hid} listed in previously_opened_holdout_ids")
    if not _ts(manifest.get("intake_snapshot_at")):
        errs.append("manifest intake_snapshot_at must be ISO-8601")
    raw_cases = ds.get("cases", []) if isinstance(ds, dict) else []
    cases = {c.get("case_id"): c for c in raw_cases if isinstance(c, dict)}
    assign = manifest.get("assignments", {})
    if not isinstance(assign, dict):
        return errs + ["manifest assignments must be an object"]
    seen_split = {}
    for cid, split in assign.items():
        if cid not in cases: errs.append(f"manifest assigns unknown case {cid}")
        if split not in ("train", "dev", "holdout"): errs.append(f"case {cid}: split must be train/dev/holdout")
        seen_split[cid] = split
    fam, subj = {}, {}
    for cid, split in seen_split.items():
        c = cases.get(cid)
        if not isinstance(c, dict): continue
        for key, store in (("family_id", fam), ("subject", subj)):
            packet = c.get("evidence_packet")
            packet = packet if isinstance(packet, dict) else {}
            k = c.get("family_id") if key == "family_id" else packet.get("subject_id")
            if k in store and (store[k] == "holdout") != (split == "holdout"):
                errs.append(f"{key} {k} overlaps holdout and non-holdout splits (case {cid})")
            store.setdefault(k, split)
        label = c.get("label", {})
        if not isinstance(label, dict):
            label = {}
        if split == "holdout" and label.get("provenance") != "independent_human":
            errs.append(f"holdout case {cid}: label.provenance must be independent_human, got {label.get('provenance')!r} (synthetic is not human ground truth)")
        elif split == "holdout":
            try:
                errs.extend(_validate_human_label_import(
                    label, cid, c.get("expected"), manifest.get("intake_snapshot_at", ""),
                    ds.get("format_version") if isinstance(ds, dict) else None))
            except Exception as error:
                errs.append(f"holdout case {cid}: label validation failed: {error}")
        ep = c.get("evidence_packet")
        ep = ep if isinstance(ep, dict) else {}
        if _ts(ep.get("collected_at", "")) and _ts(manifest.get("intake_snapshot_at", "")) and _ts(ep["collected_at"]) > _ts(manifest["intake_snapshot_at"]):
            errs.append(f"case {cid}: evidence after intake_snapshot_at")
    if manifest.get("split_strategy_time") and not errs:
        def _collected(case_id):
            pkt = cases[case_id].get("evidence_packet") if isinstance(cases.get(case_id), dict) else None
            pkt = pkt if isinstance(pkt, dict) else {}
            return _ts(pkt.get("collected_at", ""))
        non = [_collected(c) for c, s in seen_split.items() if s != "holdout" and c in cases]
        hold = [_collected(c) for c, s in seen_split.items() if s == "holdout" and c in cases]
        non = [t for t in non if t is not None]
        hold = [t for t in hold if t is not None]
        if non and hold and min(hold) <= max(non): errs.append("time holdout: earliest holdout evidence must be after latest train/dev evidence")
    if not errs:
        try:
            if not _record_holdout_opening(hid, manifest.get("format_version")):
                errs.append(f"holdout_id {hid} already used/opened; reused holdout IDs are rejected")
        except OSError as error:
            errs.append(f"cannot record holdout access: {error}")
    return errs

def check_targets(doc, holdout_opened_at):
    errs = []
    if doc.get("format_version") not in SUPPORTED_FORMATS: errs.append(f"targets format_version must be one of {', '.join(SUPPORTED_FORMATS)}")
    if not _ts(doc.get("registered_at", "")): errs.append("targets registered_at required")
    try:
        recorded_opening = _target_recorded_opening(doc)
        deadlines = [recorded_opening] if recorded_opening is not None else []
        if holdout_opened_at:
            deadlines.append(_holdout_timestamp(holdout_opened_at))
        if deadlines and _holdout_timestamp(doc.get("registered_at")) >= min(deadlines):
            errs.append("targets must be registered before holdout access (registered_at >= holdout_opened_at)")
    except (OSError, ValueError) as error:
        errs.append(f"cannot verify holdout access: {error}")
    if not doc.get("baseline", {}).get("name"): errs.append("targets baseline.name required (baseline comparison)")
    want = {"failure_category"} | set(TASK_PREDICATES)
    got = {t.get("question") for t in doc.get("targets", [])}
    for q in sorted(want - got): errs.append(f"missing per-question target for {q}")
    for t in doc.get("targets", []):
        if type(t.get("min_samples")) is not int or t["min_samples"] <= 0: errs.append(f"target {t.get('question')}: min_samples must be positive int")
        for k in ("min_precision", "min_recall"):
            if k in t and (isinstance(t[k], bool) or not isinstance(t[k], (int, float)) or not 0 <= t[k] <= 1): errs.append(f"target {t.get('question')}: {k} must be in [0,1]")
    return errs

# ---- synthetic seed set (provenance=synthetic; NOT human ground truth) ----
def _case(cid, consumer, family, subject, collected, refs, excerpts, expected, criteria=None, typed=None):
    ep = {"packet_id": f"pkt-{cid}", "subject_id": subject, "collected_at": collected,
          "source_revision": "seed", "evidence_refs": refs, "excerpts": excerpts,
          "omissions": [], "truncated": False, "typed_errors": typed or []}
    if criteria: ep["criteria"] = [{"index": i, "text": t, "text_digest": digest(t)} for i, t in enumerate(criteria)]
    return {"case_id": cid, "consumer": consumer, "family_id": family, "collected_at": collected,
            "evidence_packet": ep, "expected": expected,
            "label": {"provenance": "synthetic", "human_ground_truth": False,
                      "note": "synthetic expected label authored for offline seed; not human ground truth"}}

def seed_dataset(format_version=None):
    version = format_version if format_version in SUPPORTED_FORMATS else FORMAT_V1
    F = lambda v: {"failure_category": {"value": v}}
    P = lambda **kw: {q: {"value": kw.get(q, False)} for q in TASK_PREDICATES}
    crit_bad = ["Ship it", "Code should be clean."]
    cases = [
        _case("seed-fail-provider", "failure_categorization", "fam-provider-429", "run-a", "2026-01-01T00:00:00Z", ["run:a/log"], ["429 rate limit"], F("provider"), typed=[{"code": "rate_limit"}]),
        _case("seed-fail-product", "failure_categorization", "fam-product-panic", "run-b", "2026-01-02T00:00:00Z", ["run:b/log"], ["panic in parser on valid input"], F("product_defect")),
        _case("seed-fail-env", "failure_categorization", "fam-env-disk", "run-c", "2026-01-03T00:00:00Z", ["run:c/log"], ["no space left on device"], F("environment")),
        _case("seed-fail-mixed", "failure_categorization", "fam-mixed", "run-d", "2026-01-04T00:00:00Z", ["run:d/log"], ["timeout then panic; truncated log"], F("insufficient_evidence")),
        _case("seed-fail-hold", "failure_categorization", "fam-hold", "run-e", "2026-01-05T00:00:00Z", ["run:e/log"], ["held for missing credential, classified as product failure"], F("misclassified_hold")),
        _case("seed-task-investigation", "task_quality", "fam-task-inv", "task-inv", "2026-01-01T00:00:00Z", ["task:inv"], [], P(), criteria=["Report which of hypotheses H1/H2 the logs support, with citations."]),
        _case("seed-task-cross-component", "task_quality", "fam-task-cross", "task-cross", "2026-01-02T00:00:00Z", ["task:cross"], [], P(), criteria=["API returns X and UI displays X, verified by the named checks."]),
        _case("seed-task-contradictory", "task_quality", "fam-task-contra", "task-contra", "2026-01-03T00:00:00Z", ["task:contra"], [],
              {**P(requirements_conflict=True), "requirements_conflict": {"value": True, "criterion_refs": [crit_ref(0, "Keep the stored schema unchanged."), crit_ref(1, "Remove required stored field email.")]}},
              criteria=["Keep the stored schema unchanged.", "Remove required stored field email."]),
        _case("seed-task-vague", "task_quality", "fam-task-vague", "task-vague", "2026-01-04T00:00:00Z", ["task:vague"], [],
              {**P(outcome_unclear=True), "criterion_not_observable": {"value": True, "criterion_refs": [crit_ref(1, crit_bad[1])]}},
              criteria=crit_bad),
    ]
    return {"format_version": version, "dataset_id": "synthetic-seed/1", "intake_snapshot_at": "2026-02-01T00:00:00Z",
            "label_provenance": "synthetic", "cases": cases}

def seed_answers(format_version=None):
    version = format_version if format_version in SUPPORTED_FORMATS else FORMAT_V1
    ds = seed_dataset(version); out = []
    for c in ds["cases"]:
        ans = {q: dict(v) for q, v in c["expected"].items()}
        out.append({"case_id": c["case_id"], "disposition": "suggested", "answers": ans,
                    "usage": {"input_tokens": 100, "output_tokens": 10}, "latency_ms": 50, "cost_usd": 0.001, "pricing_version": "seed"})
    return {"format_version": version, "answers": out}

def load(p): return json.loads(Path(p).read_text())
def emit(obj, code=0): print(json.dumps(obj, indent=2, sort_keys=True)); return code

def main(argv=None):
    ap = argparse.ArgumentParser(description="Offline decision-assistance evaluation (no external calls).")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("schema"); s = sub.add_parser("seed"); s.add_argument("--answers", action="store_true")
    s.add_argument("--format-version", default=FORMAT_V1, choices=list(SUPPORTED_FORMATS))
    for name in ("validate", "score"):
        p = sub.add_parser(name); p.add_argument("--dataset", required=True); p.add_argument("--answers")
    p = sub.add_parser("check-split"); p.add_argument("--dataset", required=True); p.add_argument("--manifest", required=True); p.add_argument("--prior-holdout-ids", default="")
    p = sub.add_parser("check-targets"); p.add_argument("--targets", required=True); p.add_argument("--holdout-opened-at")
    a = ap.parse_args(argv)
    if a.cmd == "schema":
        return emit({"format_version": FORMAT_V1, "supported_format_versions": list(SUPPORTED_FORMATS),
                     "question_sets": QUESTION_SETS, "dispositions": DISPOSITIONS,
                     "label_provenances": PROVENANCES,
                     "human_label_import_fields": {"v1_legacy": list(HUMAN_LABEL_IMPORT_FIELDS_V1),
                                                   "v2": list(HUMAN_LABEL_IMPORT_FIELDS_V2)},
                     "human_holdout_eligibility": f"v1 second labels are legacy/unverified; v2 ({FORMAT_V2}) with a recorded first value is required",
                     "criterion_ref": {"index": "int", "text_digest": "sha256:<64 hex of criterion text>"}})
    if a.cmd == "seed": return emit(seed_answers(a.format_version) if a.answers else seed_dataset(a.format_version))
    if a.cmd in ("validate", "score"):
        try:
            ds = load(a.dataset)
            try:
                errs = validate_dataset(ds)
            except Exception as error:
                errs = [f"validation failed: {error}"]
            ans = load(a.answers) if a.answers else None
            if ans is not None:
                try:
                    errs += validate_answers(ds, ans)
                except Exception as error:
                    errs += [f"validation failed: {error}"]
        except Exception as error:
            errs = [f"validation failed: {error}"]
            ans = None
            ds = {}
        if a.cmd == "validate": return emit({"ok": not errs, "errors": errs}, 0 if not errs else 2)
        if ans is None and a.answers is None: return emit({"error": "score requires --answers"}, 2)
        try:
            return emit(score(ds, ans))
        except Exception as error:
            fallback = validate_dataset(ds) if isinstance(ds, dict) else [f"validation failed: {error}"]
            try:
                fallback += validate_answers(ds, ans) if isinstance(ans, dict) else [f"validation failed: {error}"]
            except Exception:
                pass
            if not fallback:
                fallback = [f"scoring failed: {error}"]
            report_version = ds.get("format_version") if isinstance(ds, dict) and ds.get("format_version") in SUPPORTED_FORMATS else FORMAT
            return emit({"format_version": report_version, "dataset_id": ds.get("dataset_id") if isinstance(ds, dict) else None,
                         "validation_errors": fallback, "questions": {}, "totals": {}}, 0)
    if a.cmd == "check-split":
        try:
            errs = check_split(load(a.dataset), load(a.manifest), [x for x in a.prior_holdout_ids.split(",") if x])
        except Exception as error:
            errs = [f"validation failed: {error}"]
        return emit({"ok": not errs, "errors": errs}, 0 if not errs else 2)
    if a.cmd == "check-targets":
        errs = check_targets(load(a.targets), a.holdout_opened_at)
        return emit({"ok": not errs, "errors": errs}, 0 if not errs else 2)
    return 2

if __name__ == "__main__": sys.exit(main())
