"""Versioned delivery-quality contracts and per-dimension ordinal readouts.

No network, numeric-grade API, composite, ranking, or runtime configuration.
"""
import math
import random
from collections import Counter
from itertools import combinations

import delivery_quality_sampling as sampling

FORMAT = sampling.FORMAT
QUESTION_VERSION = "delivery-quality-questions/1"
RUBRIC_VERSION = "delivery-quality-rubric/1"
SCORER_VERSION = "delivery-quality-scorer/1"
GRADES = ["deficient", "weak", "adequate", "strong", "exemplary"]
CHOICES = GRADES + ["not_applicable", "insufficient_evidence"]
ANCHORS = {
    "deficient": "Fundamental departures; substantial redesign or missing essential work is evidenced.",
    "weak": "Material gaps require more than localized clarification or cleanup.",
    "adequate": "Meets applicable task and repository standards without an evidenced material gap.",
    "strong": "Relevant difficult cases and boundaries are handled particularly well with evidence beyond adequate.",
    "exemplary": "Reference quality for the constraints, supported by specific evidence; size or novelty earns nothing.",
}
QUESTIONS = {
    "scope_contract_fit": "Does the attributable change implement the frozen requested behavior without unrelated or missing work?",
    "robustness": "Does changed behavior preserve relevant failure, cancellation, cleanup, concurrency and boundary contracts?",
    "structure": "Are responsibilities, interfaces and dependencies at suitable existing boundaries?",
    "standards_conventions": "Does changed code follow applicable versioned repository coding, API, error and test conventions?",
    "verification_quality": "Do completed checks exercise the changed guarantee and failure boundary without weakening coverage?",
    "clarity_maintenance": "Are names, data flow and affected docs understandable without unnecessary machinery or duplication?",
}
DISPOSITIONS = ["suggested", "uncertain", "refused", "unavailable", "invalid", "stale"]
GAPS = ["budget", "outage", "pending", "oversize", "partial"]
CONTROLS = ["minimal_good", "deficient_change", "overengineering", "inherited_poor_code",
            "shared_attribution", "partial_attribution", "missing_validation", "author_label",
            "model_label", "verbosity", "formatting", "prompt_injection", "evidence_removal"]
require = sampling.require


def delivery_schema():
    return {"format_version": FORMAT, "consumer": "delivery_quality", "question_version": QUESTION_VERSION,
            "rubric_version": RUBRIC_VERSION, "scorer_version": SCORER_VERSION,
            "questions": {q: {"type": "choice", "allowed": CHOICES, "rubric": r,
                               "anchors": ANCHORS} for q, r in QUESTIONS.items()},
            "join_version": sampling.JOIN_VERSION, "join_fields": list(sampling.JOIN_FIELDS),
            "sample_version": sampling.FRAME_VERSION, "dispositions": DISPOSITIONS,
            "gap_reasons": GAPS, "controls": CONTROLS,
            "ordinal_mapping": {g: i + 1 for i, g in enumerate(GRADES)},
            "gate_grade_choices": ["deficient", "weak"],
            "uncertainty": "95% Wilson proportions; 1000 family-cluster percentile bootstrap ordinal intervals, seed 0",
            "weighting": "quadratic disagreement weight ((predicted_index-human_index)/4)^2",
            "contract_modules": ["delivery_quality_eval", "delivery_quality_study", "delivery_quality_sampling"]}


def validate_values(values, case, where, references=True):
    require(isinstance(values, dict) and set(values) == set(QUESTIONS), f"{where}: exactly six dimensions required")
    packet = case["evidence_packet"]
    for q, answer in values.items():
        require(isinstance(answer, dict) and set(answer) <= {"value", "probability", "evidence_refs"},
                f"{where}.{q}: typed choice only")
        value = answer.get("value")
        require(isinstance(value, str) and value in CHOICES, f"{where}.{q}: invalid ordinal choice")
        if "probability" in answer:
            p = answer["probability"]
            require(type(p) in (int, float) and math.isfinite(p) and 0 <= p <= 1,
                    f"{where}.{q}: confidence must be finite in [0,1]")
        app = case["applicability"][q]["status"]
        require(value != "not_applicable" or app == "not_applicable", f"{where}.{q}: not_applicable needs host approval")
        require(value not in GRADES or app == "applicable", f"{where}.{q}: ordinal needs host-approved applicability")
        refs = answer.get("evidence_refs", [])
        require(isinstance(refs, list) and all(isinstance(r, str) for r in refs)
                and len(refs) == len(set(refs)) and set(refs) <= set(packet["question_evidence"][q]),
                f"{where}.{q}: invented or unselected evidence reference")
        if references and value in GRADES:
            require(bool(refs), f"{where}.{q}: ordinal answer needs selected evidence references")


def validate_delivery_dataset(dataset):
    require(isinstance(dataset, dict) and dataset.get("format_version") == FORMAT, "delivery format_version must be /3")
    require(dataset.get("question_version") == QUESTION_VERSION, "unsupported delivery question version")
    require(sampling.nonempty(dataset.get("dataset_id")), "dataset_id required")
    intake = sampling.instant(dataset["intake_snapshot_at"])
    cases = dataset["cases"]
    require(isinstance(cases, list) and bool(cases), "cases must be a nonempty list")
    ids, tasks = set(), set()
    for c in cases:
        require(isinstance(c, dict) and sampling.nonempty(c.get("case_id")), "case_id required")
        require(c["case_id"] not in ids, "duplicate case_id")
        ids.add(c["case_id"])
        require(c.get("consumer") == "delivery_quality" and c.get("question_version") == QUESTION_VERSION,
                "delivery consumer/question version mismatch")
        require(sampling.nonempty(c.get("family_id")), "family_id required")
        sampling.validate_join(c, dataset["intake_snapshot_at"])
        key = sampling.task_key(c)
        require(key not in tasks, "duplicate task vote; retain repeated assessments separately")
        tasks.add(key)
        require(c["join"]["rubric_version"] == RUBRIC_VERSION, "unsupported rubric version")
        ep = c["evidence_packet"]
        require(isinstance(ep, dict) and set(ep) == {"packet_id", "subject_id", "collected_at",
                "source_revision", "evidence_refs", "question_evidence", "omissions", "truncated", "scope", "blinded"},
                "invalid delivery evidence packet fields")
        for k in ("packet_id", "subject_id", "source_revision"):
            require(sampling.nonempty(ep[k]), f"packet.{k} required")
        collected = sampling.instant(ep["collected_at"])
        require(collected <= intake and collected <= sampling.instant(c["join"]["as_of"]), "future evidence after as_of/intake")
        require(ep["scope"] in ("complete", "partial") and type(ep["truncated"]) is bool
                and isinstance(ep["omissions"], list) and ep["blinded"] is True,
                "packet must explicitly record coverage, omissions, truncation and host-reviewed blinding")
        refs = ep["evidence_refs"]
        require(isinstance(refs, list), "packet evidence_refs must be a list")
        ref_ids = set()
        for ref in refs:
            require(isinstance(ref, dict) and set(ref) == {"id", "content", "digest", "observed_at"}, "invalid evidence reference")
            require(sampling.nonempty(ref["id"]) and ref["id"] not in ref_ids, "duplicate/empty evidence reference")
            ref_ids.add(ref["id"])
            require(isinstance(ref["content"], str) and ref["digest"] == sampling.content_digest(ref["content"]), "evidence digest mismatch")
            require(sampling.instant(ref["observed_at"]) <= collected, "future evidence reference")
        qe = ep["question_evidence"]
        require(isinstance(qe, dict) and set(qe) == set(QUESTIONS), "question evidence must name all dimensions")
        for q, allowed in qe.items():
            require(isinstance(allowed, list) and all(isinstance(r, str) for r in allowed)
                    and len(allowed) == len(set(allowed)) and set(allowed) <= ref_ids, "invented host-selected evidence reference")
        apps = c["applicability"]
        require(isinstance(apps, dict) and set(apps) == set(QUESTIONS), "host applicability must name all dimensions")
        for q, app in apps.items():
            require(isinstance(app, dict) and set(app) == {"status", "rule", "reviewer_id", "reviewed_at"}, "invalid host applicability record")
            require(app["status"] in ("applicable", "not_applicable", "unavailable"), "invalid applicability status")
            if app["status"] != "unavailable":
                require(sampling.nonempty(app["rule"]) and sampling.nonempty(app["reviewer_id"]), "host-reviewed applicability rule required")
                require(sampling.instant(app["reviewed_at"]) <= intake, "future applicability review")
            if c["join"]["non_code"]:
                require(app["status"] != "applicable", "programming rubric cannot grade non-code work")
        if c.get("expected") is not None:
            validate_values(c["expected"], c, "expected")
        label = c["label"]
        require(not dataset.get("mechanics_only") or label.get("provenance") != "independent_human",
                "synthetic mechanics dataset cannot claim human ground truth")
        require(isinstance(label, dict) and label.get("provenance") in ("synthetic", "independent_human", "unlabeled"),
                "model/synthetic labels are not independent human ground truth")
        if label["provenance"] != "independent_human":
            require(label.get("human_ground_truth") is False, "synthetic/unlabeled ground truth forbidden")
            require(label["provenance"] != "unlabeled" or c.get("expected") is None, "unlabeled case cannot have expected answers")
        if label["provenance"] == "independent_human" or "independent_labels" in label:
            import delivery_quality_study as study
            study.validate_human_labels(c, dataset["intake_snapshot_at"])
        metadata = c.get("host_metadata")
        require(isinstance(metadata, dict) and isinstance(metadata.get("author_ids"), list)
                and all(sampling.nonempty(a) for a in metadata["author_ids"]), "host author_ids must be explicit (may be empty/unavailable)")
        if metadata.get("control_kind") is not None:
            require(metadata["control_kind"] in CONTROLS, "unknown control kind")
    for key in ("source_version", "rubric_version", "grader_version", "builder_version"):
        require(len({c["join"][key] for c in cases}) == 1, f"mixed {key} in calibration cohort; use a separately versioned dataset")
    sampling.sampling_core(dataset)
    return []


def validate_delivery_answers(dataset, doc, frame=None):
    require(isinstance(doc, dict) and doc.get("format_version") == FORMAT, "delivery answer format must be /3")
    require(sampling.is_digest(doc.get("sample_digest")), "answers need frozen sample_digest")
    if frame is not None:
        sampling.validate_sample(dataset, frame)
        require(doc["sample_digest"] == frame["frame_digest"], "answer sampling epoch/frame mismatch")
    cases = {c["case_id"]: c for c in dataset["cases"]}
    require(isinstance(doc.get("answers"), list) and isinstance(doc.get("repeats", []), list), "answers/repeats must be lists")
    primary, assessment_ids = set(), set()
    for kind in ("answers", "repeats"):
        for a in doc.get(kind, []):
            require(isinstance(a, dict) and a.get("case_id") in cases, "answer names unknown case")
            case = cases[a["case_id"]]
            if kind == "answers":
                require(a["case_id"] not in primary, "duplicate task answer; use repeats")
                primary.add(a["case_id"])
            require(sampling.nonempty(a.get("assessment_id")) and a["assessment_id"] not in assessment_ids, "assessment_id must be unique")
            assessment_ids.add(a["assessment_id"])
            require(a.get("binding") == sampling.case_binding(case), "answer epoch/attempt/source/grader/packet binding mismatch")
            require(a.get("epoch") == dataset["population"]["epoch"], "answer epoch mismatch")
            recorded = sampling.instant(a["recorded_at"])
            require(recorded >= sampling.instant(dataset["population"]["frozen_at"]), "answer predates preselection")
            if frame is not None:
                require(recorded >= sampling.instant(frame["created_at"]), "answer predates sample receipt")
                require(any(r["case_id"] == a["case_id"] and r["selected"] for r in frame["rows"]), "answer outside preselected frame")
            require(a.get("disposition") in DISPOSITIONS, "invalid service disposition")
            require(a.get("gap_reason") is None or (a["gap_reason"] in GAPS and a["disposition"] != "suggested"), "invalid gap reason")
            values = a.get("answers")
            require(isinstance(values, dict) and set(values) <= set(QUESTIONS), "invalid answer dimensions")
            if a["disposition"] in ("refused", "unavailable"):
                require(not values, "refused/unavailable cannot carry answers")
            elif a["disposition"] == "suggested":
                validate_values(values, case, "answer")
                ep = case["evidence_packet"]
                require(not ep["omissions"] and not ep["truncated"] and ep["scope"] == "complete",
                        "suggested forbidden for partial/omitted/truncated packet")
            elif values:
                # Validate partial records with the same typed/reference rules; they never vote.
                padded = {q: values.get(q, {"value": "insufficient_evidence", "evidence_refs": []}) for q in QUESTIONS}
                validate_values(padded, case, "partial answer")
            for k in ("latency_ms", "cost_usd"):
                v = a.get(k)
                require(v is None or (type(v) in (int, float) and math.isfinite(v) and v >= 0), f"{k} must be finite nonnegative or null")
            require(a.get("cost_usd") is None or sampling.nonempty(a.get("pricing_version")), "known cost requires pricing provenance")
            usage = a.get("usage")
            require(usage is None or isinstance(usage, dict), "usage must be object or null")
            if usage:
                for k in ("input_tokens", "output_tokens"):
                    v = usage.get(k)
                    require(v is None or (type(v) is int and v >= 0), "token counts must be nonnegative integers or null")
    require(all(a["case_id"] in primary for a in doc.get("repeats", [])), "repeat requires a primary assessment")
    return []


def wilson(successes, n):
    if not n:
        return {"value": None, "count": successes, "n": 0, "interval_95": None, "method": "Wilson"}
    z = 1.959963984540054
    p = successes / n
    center = (p + z*z / (2*n)) / (1 + z*z/n)
    half = z * math.sqrt(p*(1-p)/n + z*z/(4*n*n)) / (1 + z*z/n)
    return {"value": p, "count": successes, "n": n, "interval_95": [max(0, center-half), min(1, center+half)], "method": "Wilson"}


def ordinal_statistics(pairs):
    if not pairs:
        return {"quadratic_weighted_agreement": None, "quadratic_weighted_kappa": None}
    n = len(pairs)
    observed = sum(((p-h)/4)**2 for h, p in pairs) / n
    human, predicted = Counter(h for h, _ in pairs), Counter(p for _, p in pairs)
    expected = sum(hn*pn*((p-h)/4)**2 for h, hn in human.items() for p, pn in predicted.items()) / (n*n)
    return {"quadratic_weighted_agreement": 1-observed,
            "quadratic_weighted_kappa": 1-observed/expected if expected else None}


def ordinal_readout(records):
    pairs = [(h, p) for _, h, p in records]
    result = ordinal_statistics(pairs)
    families = {}
    for family, h, p in records:
        families.setdefault(family, []).append((h, p))
    intervals = {key: [] for key in result}
    if len(families) >= 2:
        rng = random.Random(0)
        keys = sorted(families)
        for _ in range(1000):
            resample = [pair for _ in keys for pair in families[rng.choice(keys)]]
            for key, value in ordinal_statistics(resample).items():
                if value is not None:
                    intervals[key].append(value)
    for key, value in list(result.items()):
        draws = sorted(intervals[key])
        result[key] = {"value": value, "interval_95": [draws[int((len(draws)-1)*0.025)], draws[int((len(draws)-1)*0.975)]] if len(draws) >= 950 else None,
                       "uncertainty_status": "estimated" if len(draws) >= 950 else "unavailable",
                       "method": "1000 family-cluster percentile bootstrap, seed 0", "families": len(families)}
    n = len(pairs)
    result.update({"exact": wilson(sum(h == p for h, p in pairs), n),
                   "adjacent": wilson(sum(abs(h-p) <= 1 for h, p in pairs), n),
                   "material_overgrading": wilson(sum(p-h >= 2 for h, p in pairs), n),
                   "material_undergrading": wilson(sum(h-p >= 2 for h, p in pairs), n),
                   "confusion_matrix": {h: {p: sum(e == i and a == j for e, a in pairs) for j, p in enumerate(GRADES)} for i, h in enumerate(GRADES)}})
    return result


def measurement(calls, field):
    values = [a.get(field) for a in calls]
    known = bool(values) and all(v is not None for v in values)
    return {"status": "known" if known else "unknown", "n_known": sum(v is not None for v in values),
            "n_calls": len(calls), "total": sum(values) if known else None,
            "mean": sum(values)/len(values) if known else None}


def delivery_score(dataset, doc, frame):
    validate_delivery_dataset(dataset)
    validate_delivery_answers(dataset, doc, frame)
    export = sampling.ladder_export(dataset, doc, frame, QUESTIONS)
    by_id = {a["case_id"]: a for a in doc["answers"]}
    states = {r["case_id"]: r["dimension_states"] for r in export["rows"]}
    calls = doc["answers"] + doc.get("repeats", [])
    questions = {}
    host_bias = {}
    useful_cases = set()
    controls = {}
    for q in QUESTIONS:
        records, human_pairs, repeats = [], [], []
        bias_records = {}
        counts = Counter()
        for c in dataset["cases"]:
            state = states[c["case_id"]][q]
            counts[state] += 1
            a = by_id.get(c["case_id"])
            exp = c.get("expected") or {}
            ev = exp.get(q, {}).get("value")
            pv = a.get("answers", {}).get(q, {}).get("value") if a else None
            if state == "valid_ordinal":
                useful_cases.add(c["case_id"])
                if ev in GRADES:
                    record = (c["family_id"], GRADES.index(ev), GRADES.index(pv))
                    records.append(record)
                    meta = c["host_metadata"]
                    bias_key = sampling.canonical({k: meta.get(k) for k in ("author_model_family", "grader_model_family")})
                    bias_records.setdefault(bias_key, []).append(record)
            label = c["label"]
            originals = label.get("independent_labels", [])
            if len(originals) == 2 and state not in ("unsampled", "ineligible"):
                first, second = (lab["values"][q]["value"] for lab in originals)
                counts["human_disagreement"] += int(first != second)
                counts["human_abstention"] += int("insufficient_evidence" in (first, second))
                counts["human_dual_labeled"] += 1
                counts["human_adjudicated"] += int(label.get("adjudication") is not None)
                if first in GRADES and second in GRADES:
                    human_pairs.append((c["family_id"], GRADES.index(first), GRADES.index(second)))
            if state == "valid_ordinal":
                for repeat in doc.get("repeats", []):
                    if repeat["case_id"] == c["case_id"] and sampling.dimension_state(c, repeat, q) == "valid_ordinal":
                        repeats.append((c["family_id"], GRADES.index(pv), GRADES.index(repeat["answers"][q]["value"])))
        denominator = sum(g["dimensions"][q]["coverage_denominator"] for g in export["groups"])
        valid = sum(g["dimensions"][q]["valid_ordinal"] for g in export["groups"])
        questions[q] = {**ordinal_readout(records), "states": dict(counts),
                        "coverage": wilson(valid, denominator),
                        "abstention": wilson(denominator-valid, denominator),
                        "human_disagreement": wilson(counts["human_disagreement"], counts["human_dual_labeled"]),
                        "human_abstention": wilson(counts["human_abstention"], counts["human_dual_labeled"]),
                        "human_interrater": ordinal_readout(human_pairs), "repeatability": ordinal_readout(repeats)}
        host_bias[q] = {key: ordinal_readout(rows) for key, rows in bias_records.items()}
    # Paired perturbations report stability separately; identity effects remain host-only.
    paired = {}
    for c in dataset["cases"]:
        meta = c["host_metadata"]
        if meta.get("pair_id"):
            paired.setdefault(meta["pair_id"], []).append(c)
    for pair_id, members in paired.items():
        controls[pair_id] = {"kinds": sorted({m["host_metadata"]["control_kind"] for m in members}), "dimensions": {}}
        for q in QUESTIONS:
            pairs = []
            for left, right in combinations(members, 2):
                if all(states[c["case_id"]][q] == "valid_ordinal" for c in (left, right)):
                    a, b = (by_id[c["case_id"]]["answers"][q]["value"] for c in (left, right))
                    pairs.append((pair_id, GRADES.index(a), GRADES.index(b)))
            controls[pair_id]["dimensions"][q] = ordinal_readout(pairs)
    totals = {key: measurement(calls, key) for key in ("latency_ms", "cost_usd")}
    for key in ("input_tokens", "output_tokens"):
        totals[key] = measurement([{key: (a.get("usage") or {}).get(key)} for a in calls], key)
    cost = totals["cost_usd"]["total"]
    totals["useful_assessments"] = len(useful_cases)
    totals["cost_per_useful_assessment_usd"] = cost / len(useful_cases) if cost is not None and useful_cases else None
    for q, report in questions.items():
        valid = report["coverage"]["count"]
        report["cost_per_useful_assessment_usd"] = cost / valid if cost is not None and valid else None
        report["cost_status"] = totals["cost_usd"]["status"]
    totals["useful_definition"] = "unique selected attributable tasks with at least one valid applicable ordinal dimension; all calls including repeats in cost"
    return {"format_version": FORMAT, "scorer_version": SCORER_VERSION, "dataset_id": dataset["dataset_id"],
            "evidence_status": "synthetic mechanics only; not accuracy evidence" if any(c["label"]["provenance"] != "independent_human" for c in dataset["cases"]) else "human labels; adoption requires registered study readout",
            "questions": questions, "controls": controls, "host_bias_analysis": host_bias, "measurements": totals, "ladder": export,
            "validation_errors": []}


def delivery_cli(argv):
    """Dispatch only additive commands or explicit v3 documents; legacy stays frozen."""
    import argparse
    import json
    from pathlib import Path
    import delivery_quality_seed as seed
    import delivery_quality_study as study

    if not argv:
        return None
    command = argv[0]
    new_commands = {"delivery-schema", "freeze-sample", "export-labels", "import-labels",
                    "ladder-export", "register-targets", "study-seed"}
    explicit = command in new_commands or (command == "seed" and FORMAT in argv)
    if not explicit and command in ("validate", "score", "check-split", "check-targets"):
        flag = "--targets" if command == "check-targets" else "--dataset"
        try:
            doc = json.loads(Path(argv[argv.index(flag)+1]).read_text())
            explicit = isinstance(doc, dict) and (doc.get("format_version") == FORMAT or doc.get("version") == study.TARGET_VERSION)
        except (ValueError, IndexError, OSError, RecursionError):
            pass  # original CLI owns legacy/malformed legacy input
    if not explicit:
        return None
    parser = argparse.ArgumentParser(description="Offline delivery-quality evaluation; no external calls.")
    parser.add_argument("command", choices=sorted(new_commands | {"seed", "validate", "score", "check-split", "check-targets"}))
    for flag in ("dataset", "sample", "labels", "targets", "manifest", "output", "output-dir", "holdout-opened-at"):
        parser.add_argument("--" + flag)
    parser.add_argument("--format-version", default=FORMAT, choices=[FORMAT])
    parser.add_argument("--answers", nargs="?", const=True)
    parser.add_argument("--scenario", choices=["ladder", "controls", "study"], default="ladder")
    parser.add_argument("--prior-holdout-ids", default="")
    args = parser.parse_args(argv)
    def load(path):
        require(isinstance(path, str), "required input file argument missing")
        return json.loads(Path(path).read_text())
    try:
        if command == "delivery-schema":
            result = delivery_schema()
        elif command == "seed":
            dataset = load(args.dataset) if args.dataset else seed.delivery_seed(args.scenario)
            result = seed.recorded_answers(dataset, load(args.sample)) if args.answers else dataset
        elif command in ("check-targets", "register-targets"):
            result = study.check_delivery_targets(load(args.targets), args.holdout_opened_at, command == "register-targets")
        else:
            dataset = load(args.dataset)
            validate_delivery_dataset(dataset)
            if command == "check-split":
                result = study.check_delivery_split(dataset, load(args.manifest), args.prior_holdout_ids.split(","))
            elif command == "freeze-sample":
                require(args.output is not None, "freeze-sample requires --output")
                result = sampling.freeze_sample(dataset, args.output)
            else:
                frame = load(args.sample) if args.sample else None
                if command == "validate" and not args.answers:
                    result = {"ok": True, "errors": []}
                else:
                    require(frame is not None, "delivery answers/exports require --sample")
                    sampling.validate_sample(dataset, frame)
                    if command == "export-labels":
                        result = study.blind_export(dataset, frame)
                    elif command == "import-labels":
                        result = study.import_labels(dataset, frame, load(args.labels))
                    elif command == "study-seed":
                        result = seed.study_seed(dataset, frame)
                        if args.output_dir:
                            directory = Path(args.output_dir)
                            directory.mkdir(parents=True, exist_ok=True)
                            for key, value in result.items():
                                with (directory / (key + ".json")).open("x") as handle:
                                    handle.write(json.dumps(value, indent=2, sort_keys=True) + "\n")
                    else:
                        doc = load(args.answers)
                        validate_delivery_answers(dataset, doc, frame)
                        if command == "score":
                            result = delivery_score(dataset, doc, frame)
                        elif command == "ladder-export":
                            result = sampling.ladder_export(dataset, doc, frame, QUESTIONS)
                        else:
                            result = {"ok": True, "errors": []}
        print(json.dumps(result, indent=2, sort_keys=True, allow_nan=False))
        return 0
    except (ValueError, TypeError, KeyError, AttributeError, OSError, RecursionError, ImportError) as error:
        print(json.dumps({"ok": False, "errors": [str(error)]}, indent=2))
        return 2
