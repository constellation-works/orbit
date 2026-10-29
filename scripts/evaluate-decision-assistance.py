#!/usr/bin/env python3
"""Offline evaluation contracts and scoring for bounded decision assistance.

Stdlib only, no network. See docs/design/decision-assistance/2_evaluation.md.
Formats are versioned (`decision-assistance-eval/1`); synthetic labels are
never human ground truth (label.provenance distinguishes them).
"""
import argparse, hashlib, json, math, sys
from collections import Counter, defaultdict
from pathlib import Path

FORMAT = "decision-assistance-eval/1"
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

def digest(text): return "sha256:" + hashlib.sha256(text.encode()).hexdigest()
def crit_ref(index, text): return {"index": index, "text_digest": digest(text)}
def is_digest(v): return isinstance(v, str) and v.startswith("sha256:") and len(v) == 71 and all(c in "0123456789abcdef" for c in v[7:])

def _ts(v): return isinstance(v, str) and len(v) >= 10 and v[4] == "-" and v[7] == "-"

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
    for r in ans.get("criterion_refs", []):
        if not isinstance(r, dict) or not isinstance(r.get("index"), int) or not is_digest(r.get("text_digest", "")):
            errs.append(f"{where}: criterion_refs entries need integer index and sha256 text_digest")

def validate_dataset(ds):
    errs = []
    if not isinstance(ds, dict) or ds.get("format_version") != FORMAT: return [f"format_version must be {FORMAT}"]
    if not _ts(ds.get("intake_snapshot_at", "")): errs.append("intake_snapshot_at must be ISO-8601")
    seen = set()
    for c in ds.get("cases", []):
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
        ep = c.get("evidence_packet") or {}
        if _ts(ep.get("collected_at", "")) and _ts(ds.get("intake_snapshot_at", "")) and ep["collected_at"] > ds["intake_snapshot_at"]:
            errs.append(f"{w}: evidence collected after intake_snapshot_at")
        # criteria digests must match text
        crits = {cr.get("index"): cr for cr in ep.get("criteria", []) if isinstance(cr, dict)}
        for idx, cr in crits.items():
            if "text" in cr and cr.get("text_digest") != digest(cr["text"]): errs.append(f"{w}: criterion {idx} text_digest mismatch")
        exp = c.get("expected")
        if not isinstance(exp, dict): errs.append(f"{w}: expected answers required"); continue
        want = QUESTION_SETS[c["consumer"]]["questions"]
        if set(exp) != set(want): errs.append(f"{w}: expected must contain exactly {sorted(want)}")
        for q, a in exp.items():
            if q in want: validate_answer_value(want[q], a, errs, f"{w}.{q}")
            for r in (a or {}).get("criterion_refs", []) if isinstance(a, dict) else []:
                if isinstance(r, dict) and r.get("index") in crits and r.get("text_digest") != crits[r["index"]].get("text_digest"):
                    errs.append(f"{w}.{q}: criterion_ref digest does not match packet criterion")
    return errs

def validate_answers(ds, ansdoc):
    errs = []
    if not isinstance(ansdoc, dict) or ansdoc.get("format_version") != FORMAT: return [f"answers format_version must be {FORMAT}"]
    cases = {c["case_id"]: c for c in ds.get("cases", [])}
    seen = set()
    for a in ansdoc.get("answers", []):
        w = f"answer {a.get('case_id')}"
        if a.get("case_id") in seen: errs.append(f"{w}: duplicate answer")
        seen.add(a.get("case_id"))
        case = cases.get(a.get("case_id"))
        if case is None: errs.append(f"{w}: unknown case_id"); continue
        if a.get("disposition") not in DISPOSITIONS: errs.append(f"{w}: disposition must be one of {DISPOSITIONS}"); continue
        got = a.get("answers") or {}
        if a["disposition"] in ("refused", "unavailable"):
            if got: errs.append(f"{w}: refused/unavailable answers must carry no question answers")
            continue
        want = QUESTION_SETS[case["consumer"]]["questions"]
        if set(got) != set(want): errs.append(f"{w}: answers must contain exactly {sorted(want)}, got {sorted(got)}")
        for q, v in got.items():
            if q in want: validate_answer_value(want[q], v, errs, f"{w}.{q}")
        for k in ("usage", "latency_ms", "cost_usd"):
            if k in a and a[k] is not None and (not isinstance(a[k], (int, float, dict)) or (isinstance(a[k], (int, float)) and a[k] < 0)):
                errs.append(f"{w}: {k} must be non-negative or null (unknown)")
    return errs

def _pr(tp, fp, fn):
    prec = tp / (tp + fp) if tp + fp else None
    rec = tp / (tp + fn) if tp + fn else None
    f1 = (2 * prec * rec / (prec + rec)) if prec and rec else (0.0 if prec == 0 or rec == 0 else None)
    return {"precision": prec, "recall": rec, "f1": f1, "tp": tp, "fp": fp, "fn": fn}

def score(ds, ansdoc):
    errs = validate_dataset(ds) + validate_answers(ds, ansdoc)
    cases = {c["case_id"]: c for c in ds.get("cases", [])}
    by_id = {a.get("case_id"): a for a in ansdoc.get("answers", [])}
    report = {"format_version": FORMAT, "dataset_id": ds.get("dataset_id"), "validation_errors": errs,
              "questions": {}, "totals": {}}
    total = answered = abstained = invalid = refused = 0
    lat, costs, in_tok, out_tok = [], [], [], []
    usage_known = {"latency": True, "cost": True, "input_tokens": True, "output_tokens": True}
    # per-question accumulators
    choice_conf = defaultdict(Counter); choice_counts = defaultdict(lambda: [0, 0, 0])  # tp,fp,fn
    pred_stats = {}
    for cid, case in cases.items():
        total += 1
        a = by_id.get(cid)
        if a is None or a.get("disposition") not in DISPOSITIONS or (validate_answers(ds, {"format_version": FORMAT, "answers": [a]})):
            invalid += 1; abstained += 1; continue
        if a["disposition"] in ("refused", "unavailable"): refused += 1; abstained += 1; continue
        if a["disposition"] != "suggested": abstained += 1; continue
        got = a.get("answers") or {}
        # usage (missing = unknown, never zero)
        if isinstance(a.get("latency_ms"), (int, float)): lat.append(a["latency_ms"])
        else: usage_known["latency"] = False
        if isinstance(a.get("cost_usd"), (int, float)): costs.append(a["cost_usd"])
        else: usage_known["cost"] = False
        u = a.get("usage") or {}
        if isinstance(u.get("input_tokens"), int): in_tok.append(u["input_tokens"])
        else: usage_known["input_tokens"] = False
        if isinstance(u.get("output_tokens"), int): out_tok.append(u["output_tokens"])
        else: usage_known["output_tokens"] = False
        case_answered = True
        for q, exp in case["expected"].items():
            pred = got.get(q, {}); ev, pv = exp["value"], pred.get("value")
            if case["consumer"] == "failure_categorization":
                if pv == "insufficient_evidence": case_answered = False; continue
                choice_conf[ev][pv] += 1
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

def check_split(ds, manifest, prior_holdout_ids):
    errs = validate_dataset(ds)
    if manifest.get("format_version") != FORMAT: errs.append(f"manifest format_version must be {FORMAT}")
    hid = manifest.get("holdout_id")
    if not hid: errs.append("manifest holdout_id required")
    if hid in (prior_holdout_ids or []): errs.append(f"holdout_id {hid} already used/opened; reused holdout IDs are rejected")
    if hid in manifest.get("previously_opened_holdout_ids", []): errs.append(f"holdout_id {hid} listed in previously_opened_holdout_ids")
    cases = {c["case_id"]: c for c in ds.get("cases", [])}
    assign = manifest.get("assignments", {})
    seen_split = {}
    for cid, split in assign.items():
        if cid not in cases: errs.append(f"manifest assigns unknown case {cid}")
        if split not in ("train", "dev", "holdout"): errs.append(f"case {cid}: split must be train/dev/holdout")
        seen_split[cid] = split
    fam, subj = {}, {}
    for cid, split in seen_split.items():
        c = cases.get(cid)
        if not c: continue
        for key, store in (("family_id", fam), ("subject", subj)):
            k = c.get("family_id") if key == "family_id" else (c.get("evidence_packet") or {}).get("subject_id")
            if k in store and (store[k] == "holdout") != (split == "holdout"):
                errs.append(f"{key} {k} overlaps holdout and non-holdout splits (case {cid})")
            store.setdefault(k, split)
        if split == "holdout" and c.get("label", {}).get("provenance") != "independent_human":
            errs.append(f"holdout case {cid}: label.provenance must be independent_human, got {c.get('label', {}).get('provenance')!r} (synthetic is not human ground truth)")
        ep = c.get("evidence_packet") or {}
        if _ts(ep.get("collected_at", "")) and _ts(manifest.get("intake_snapshot_at", "")) and ep["collected_at"] > manifest["intake_snapshot_at"]:
            errs.append(f"case {cid}: evidence after intake_snapshot_at")
    if manifest.get("split_strategy_time"):
        non = [cases[c]["evidence_packet"]["collected_at"] for c, s in seen_split.items() if s != "holdout" and c in cases]
        hold = [cases[c]["evidence_packet"]["collected_at"] for c, s in seen_split.items() if s == "holdout" and c in cases]
        if non and hold and min(hold) <= max(non): errs.append("time holdout: earliest holdout evidence must be after latest train/dev evidence")
    return errs

def check_targets(doc, holdout_opened_at):
    errs = []
    if doc.get("format_version") != FORMAT: errs.append(f"targets format_version must be {FORMAT}")
    if not _ts(doc.get("registered_at", "")): errs.append("targets registered_at required")
    if holdout_opened_at and _ts(doc.get("registered_at", "")) and doc["registered_at"] >= holdout_opened_at:
        errs.append("targets must be registered before holdout access (registered_at >= holdout_opened_at)")
    if not doc.get("baseline", {}).get("name"): errs.append("targets baseline.name required (baseline comparison)")
    want = {"failure_category"} | set(TASK_PREDICATES)
    got = {t.get("question") for t in doc.get("targets", [])}
    for q in sorted(want - got): errs.append(f"missing per-question target for {q}")
    for t in doc.get("targets", []):
        if not isinstance(t.get("min_samples"), int) or t["min_samples"] <= 0: errs.append(f"target {t.get('question')}: min_samples must be positive int")
        for k in ("min_precision", "min_recall"):
            if k in t and (not isinstance(t[k], (int, float)) or not 0 <= t[k] <= 1): errs.append(f"target {t.get('question')}: {k} must be in [0,1]")
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

def seed_dataset():
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
    return {"format_version": FORMAT, "dataset_id": "synthetic-seed/1", "intake_snapshot_at": "2026-02-01T00:00:00Z",
            "label_provenance": "synthetic", "cases": cases}

def seed_answers():
    ds = seed_dataset(); out = []
    for c in ds["cases"]:
        ans = {q: dict(v) for q, v in c["expected"].items()}
        out.append({"case_id": c["case_id"], "disposition": "suggested", "answers": ans,
                    "usage": {"input_tokens": 100, "output_tokens": 10}, "latency_ms": 50, "cost_usd": 0.001, "pricing_version": "seed"})
    return {"format_version": FORMAT, "answers": out}

def load(p): return json.loads(Path(p).read_text())
def emit(obj, code=0): print(json.dumps(obj, indent=2, sort_keys=True)); return code

def main(argv=None):
    ap = argparse.ArgumentParser(description="Offline decision-assistance evaluation (no external calls).")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("schema"); s = sub.add_parser("seed"); s.add_argument("--answers", action="store_true")
    for name in ("validate", "score"):
        p = sub.add_parser(name); p.add_argument("--dataset", required=True); p.add_argument("--answers")
    p = sub.add_parser("check-split"); p.add_argument("--dataset", required=True); p.add_argument("--manifest", required=True); p.add_argument("--prior-holdout-ids", default="")
    p = sub.add_parser("check-targets"); p.add_argument("--targets", required=True); p.add_argument("--holdout-opened-at")
    a = ap.parse_args(argv)
    if a.cmd == "schema":
        return emit({"format_version": FORMAT, "question_sets": QUESTION_SETS, "dispositions": DISPOSITIONS,
                     "label_provenances": PROVENANCES, "criterion_ref": {"index": "int", "text_digest": "sha256:<64 hex of criterion text>"}})
    if a.cmd == "seed": return emit(seed_answers() if a.answers else seed_dataset())
    if a.cmd in ("validate", "score"):
        ds = load(a.dataset); errs = validate_dataset(ds)
        ans = load(a.answers) if a.answers else None
        if ans is not None: errs += validate_answers(ds, ans)
        if a.cmd == "validate": return emit({"ok": not errs, "errors": errs}, 0 if not errs else 2)
        if ans is None: return emit({"error": "score requires --answers"}, 2)
        return emit(score(ds, ans))
    if a.cmd == "check-split":
        errs = check_split(load(a.dataset), load(a.manifest), [x for x in a.prior_holdout_ids.split(",") if x])
        return emit({"ok": not errs, "errors": errs}, 0 if not errs else 2)
    if a.cmd == "check-targets":
        errs = check_targets(load(a.targets), a.holdout_opened_at)
        return emit({"ok": not errs, "errors": errs}, 0 if not errs else 2)
    return 2

if __name__ == "__main__": sys.exit(main())
