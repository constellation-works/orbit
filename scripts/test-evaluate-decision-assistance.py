#!/usr/bin/env python3
"""Public-CLI verification for decision-assistance evaluation (acceptance criterion 4).

Exercises scripts/evaluate-decision-assistance.py only through its CLI on
isolated fixtures under .orbit/tmp: malformed answers, task-quality and
failure-category abstentions, a deliberately poor classifier, split/target
guards, and unknown metrics. No external calls.
"""
import json, subprocess, sys, tempfile, unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
TOOL = SCRIPTS / "evaluate-decision-assistance.py"

def run(*args, cwd):
    return subprocess.run([sys.executable, str(TOOL), *args], cwd=cwd, capture_output=True, text=True)

class EvalTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"; scratch.mkdir(parents=True, exist_ok=True)
        td = tempfile.TemporaryDirectory(prefix="decision-eval-", dir=scratch); self.addCleanup(td.cleanup)
        self.root = Path(td.name)
        self.ds = self._dump("ds.json", json.loads(self._run("seed").stdout))
        self.ans = self._dump("ans.json", json.loads(self._run("seed", "--answers").stdout))

    def _run(self, *args):
        return run(*args, cwd=self.root)

    def _dump(self, name, obj):
        p = self.root / name; p.write_text(json.dumps(obj)); return p

    def _load(self, p): return json.loads(Path(p).read_text())
    def _score(self, ds, ans): 
        r = self._run("score", "--dataset", str(ds), "--answers", str(ans)); self.assertEqual(r.returncode, 0, r.stderr); return json.loads(r.stdout)

    def test_seed_perfect_and_insufficient_abstains(self):
        rep = self._score(self.ds, self.ans)
        self.assertEqual(rep["validation_errors"], [])
        # insufficient_evidence seed case abstains from substantive coverage
        self.assertLess(rep["totals"]["coverage"], 1.0); self.assertGreaterEqual(rep["totals"]["abstained"], 1)
        self.assertEqual(rep["questions"]["failure_category"]["per_class"]["provider"]["recall"], 1.0)
        self.assertIn("confusion_matrix", rep["questions"]["failure_category"])
        self.assertEqual(rep["totals"]["cost_usd"]["status"], "known")
        ds = self._load(self.ds)
        self.assertTrue(all(c["label"]["provenance"] == "synthetic" and not c["label"]["human_ground_truth"] for c in ds["cases"]))

    def test_abstained_true_class_counts_as_false_negative(self):
        ds = self._load(self.ds)
        ans = self._load(self.ans)
        provider_case = next(c for c in ds["cases"] if c["case_id"] == "seed-fail-provider")
        extra_case = json.loads(json.dumps(provider_case))
        extra_case["case_id"] = "seed-fail-provider-2"
        extra_case["family_id"] = "fam-provider-abstention"
        extra_case["evidence_packet"]["packet_id"] = "pkt-seed-fail-provider-2"
        extra_case["evidence_packet"]["subject_id"] = "run-provider-2"
        ds["cases"].append(extra_case)

        provider_answer = next(a for a in ans["answers"] if a["case_id"] == "seed-fail-provider")
        extra_answer = json.loads(json.dumps(provider_answer))
        extra_answer["case_id"] = "seed-fail-provider-2"
        extra_answer["answers"]["failure_category"]["value"] = "insufficient_evidence"
        ans["answers"].append(extra_answer)

        abstain_report = self._score(self._dump("provider-abstain-ds.json", ds),
                                     self._dump("provider-abstain-ans.json", ans))
        extra_answer["answers"]["failure_category"]["value"] = "product_defect"
        wrong_class_report = self._score(self._dump("provider-wrong-ds.json", ds),
                                         self._dump("provider-wrong-ans.json", ans))

        abstain_metrics = abstain_report["questions"]["failure_category"]["per_class"]["provider"]
        wrong_metrics = wrong_class_report["questions"]["failure_category"]["per_class"]["provider"]
        self.assertEqual(abstain_metrics["recall"], 0.5)
        self.assertEqual(abstain_metrics["fn"], 1)
        self.assertEqual(abstain_metrics["precision"], wrong_metrics["precision"])
        self.assertEqual(abstain_metrics["precision"], 1.0)
        self.assertEqual(abstain_report["totals"]["abstained"], wrong_class_report["totals"]["abstained"] + 1)
        self.assertEqual(abstain_report["totals"]["answered"], wrong_class_report["totals"]["answered"] - 1)
        self.assertEqual(abstain_report["totals"]["coverage"],
                         abstain_report["totals"]["answered"] / abstain_report["totals"]["cases"])

    def test_malformed_answers_rejected(self):
        ans = self._load(self.ans); ans["answers"][0]["answers"] = {"failure_category": {"value": "bogus", "probability": 1.5}, "extra": {"value": 1}}
        p = self._dump("bad.json", ans); r = self._run("validate", "--dataset", str(self.ds), "--answers", str(p))
        self.assertEqual(r.returncode, 2); self.assertFalse(json.loads(r.stdout)["ok"])
        rep = self._score(self.ds, p); self.assertGreaterEqual(rep["totals"]["invalid"], 1)

    def test_refusal_abstains(self):
        ans = self._load(self.ans); ans["answers"][0] = {"case_id": ans["answers"][0]["case_id"], "disposition": "refused", "answers": {}}
        rep = self._score(self.ds, self._dump("ref.json", ans))
        self.assertGreaterEqual(rep["totals"]["refused_or_unavailable"], 1); self.assertGreaterEqual(rep["totals"]["abstained"], 1)

    def test_abstained_true_class_answers_count_as_false_negatives(self):
        ds = self._load(self.ds)
        ans = self._load(self.ans)
        provider_case = next(c for c in ds["cases"] if c["case_id"] == "seed-fail-provider")
        extra_case = json.loads(json.dumps(provider_case))
        extra_case["case_id"] = "seed-fail-provider-2"
        extra_case["family_id"] = "fam-provider-early-exit"
        extra_case["evidence_packet"]["packet_id"] = "pkt-seed-fail-provider-2"
        extra_case["evidence_packet"]["subject_id"] = "run-provider-2"
        ds["cases"] = [provider_case, extra_case]

        provider_answer = next(a for a in ans["answers"] if a["case_id"] == "seed-fail-provider")
        provider_answer["disposition"] = "suggested"
        provider_answer["answers"]["failure_category"]["value"] = "provider"
        ans["answers"] = [provider_answer]

        for early_exit in ("refused", "unavailable", "uncertain", "invalid", "stale", "omitted"):
            with self.subTest(early_exit=early_exit):
                case_answers = json.loads(json.dumps(ans))
                if early_exit in ("refused", "unavailable", "uncertain", "stale"):
                    case_answers["answers"].append({
                        "case_id": "seed-fail-provider-2", "disposition": early_exit,
                        "answers": {} if early_exit in ("refused", "unavailable") else {
                            "failure_category": {"value": "provider"},
                        },
                    })
                elif early_exit == "invalid":
                    case_answers["answers"].append({
                        "case_id": "seed-fail-provider-2", "disposition": "suggested",
                        "answers": {"failure_category": {"value": "not-a-category"}},
                    })
                report = self._score(self._dump(f"provider-{early_exit}-ds.json", ds),
                                     self._dump(f"provider-{early_exit}-ans.json", case_answers))

                if early_exit == "invalid":
                    self.assertTrue(report["validation_errors"])
                else:
                    self.assertEqual(report["validation_errors"], [])
                metrics = report["questions"]["failure_category"]["per_class"]["provider"]
                self.assertEqual(metrics, {"precision": 1.0, "recall": 0.5, "f1": 2 / 3,
                                           "tp": 1, "fp": 0, "fn": 1})
                self.assertEqual(report["totals"]["answered"], 1)
                self.assertEqual(report["totals"]["abstained"], 1)
                self.assertEqual(report["totals"]["coverage"], 0.5)

    def test_failure_category_abstention_tolerates_missing_expected_label(self):
        ds = self._load(self.ds)
        case = next(c for c in ds["cases"] if c["case_id"] == "seed-fail-provider")
        case["expected"].pop("failure_category")
        malformed = self._dump("missing-failure-category-ds.json", ds)
        control = self._score(self.ds, self.ans)

        rejected_answers = {
            "refused": {"disposition": "refused", "answers": {}},
            "unavailable": {"disposition": "unavailable", "answers": {}},
            "uncertain": {"disposition": "uncertain", "answers": {"failure_category": {"value": "provider"}}},
            "stale": {"disposition": "stale", "answers": {"failure_category": {"value": "provider"}}},
            "invalid": {"disposition": "invalid", "answers": {"failure_category": {"value": "not-a-category"}}},
            "absent": None,
        }
        for name, rejected in rejected_answers.items():
            with self.subTest(answer=name):
                ans = self._load(self.ans)
                ans["answers"] = [a for a in ans["answers"] if a["case_id"] != "seed-fail-provider"]
                if rejected is not None:
                    ans["answers"].append({"case_id": "seed-fail-provider", **rejected})
                report = self._score(malformed, self._dump(f"missing-failure-category-{name}-ans.json", ans))

                self.assertTrue(any("seed-fail-provider" in error for error in report["validation_errors"]),
                                report["validation_errors"])
                self.assertEqual(set(report["questions"]), set(control["questions"]))
                self.assertEqual(report["totals"]["cases"], control["totals"]["cases"])
                self.assertEqual(report["totals"]["answered"], control["totals"]["answered"] - 1)
                self.assertEqual(report["totals"]["abstained"], control["totals"]["abstained"] + 1)

    def test_task_quality_abstentions_count_true_predicates_as_false_negatives(self):
        seed_ds = self._load(self.ds)
        seed_ans = self._load(self.ans)
        templates = {
            "outcome_unclear": "seed-task-vague",
            "criterion_not_observable": "seed-task-vague",
            "requirements_conflict": "seed-task-contradictory",
            "scope_clarification": "seed-task-cross-component",
        }
        abstentions = ("refused", "unavailable", "uncertain", "invalid", "stale", "omitted")

        for predicate, template_id in templates.items():
            template = next(case for case in seed_ds["cases"] if case["case_id"] == template_id)
            for abstention in abstentions:
                with self.subTest(predicate=predicate, abstention=abstention):
                    cases = []
                    answers = []
                    for suffix in ("suggested", "abstained"):
                        case = json.loads(json.dumps(template))
                        case["case_id"] = f"task-quality-{predicate}-{abstention}-{suffix}"
                        case["family_id"] = case["case_id"]
                        case["evidence_packet"]["packet_id"] = f"pkt-{case['case_id']}"
                        case["evidence_packet"]["subject_id"] = case["case_id"]
                        case["expected"] = {
                            question: {"value": question == predicate}
                            for question in case["expected"]
                        }
                        refs = template["expected"].get(predicate, {}).get("criterion_refs")
                        if refs:
                            case["expected"][predicate]["criterion_refs"] = refs
                        cases.append(case)

                    suggested = {"case_id": cases[0]["case_id"], "disposition": "suggested",
                                 "answers": json.loads(json.dumps(cases[0]["expected"]))}
                    answers.append(suggested)
                    if abstention != "omitted":
                        rejected = {"case_id": cases[1]["case_id"], "disposition": abstention}
                        if abstention not in ("refused", "unavailable"):
                            rejected["answers"] = json.loads(json.dumps(cases[1]["expected"]))
                        if abstention == "invalid":
                            rejected["answers"][predicate] = {"value": "not-a-boolean"}
                        answers.append(rejected)

                    dataset = dict(seed_ds, cases=cases)
                    answer_doc = {"format_version": seed_ans["format_version"], "answers": answers}
                    report = self._score(self._dump("task-quality-ds.json", dataset),
                                         self._dump("task-quality-ans.json", answer_doc))

                    metrics = report["questions"][predicate]
                    self.assertEqual((metrics["tp"], metrics["fp"], metrics["fn"]), (1, 0, 1))
                    self.assertEqual(metrics["precision"], 1.0)
                    self.assertEqual(metrics["recall"], 0.5)
                    self.assertEqual(metrics["f1"], 2 / 3)
                    totals = report["totals"]
                    self.assertEqual(totals["cases"], 2)
                    self.assertEqual(totals["answered"], 1)
                    self.assertEqual(totals["abstained"], 1)
                    self.assertEqual(totals["coverage"], 0.5)
                    self.assertEqual(totals["refused_or_unavailable"],
                                     1 if abstention in ("refused", "unavailable") else 0)
                    self.assertEqual(totals["invalid"],
                                     1 if abstention in ("invalid", "omitted") else 0)
                    self.assertEqual(bool(report["validation_errors"]), abstention == "invalid")

    def test_poor_classifier_visible(self):
        ans = self._load(self.ans)
        for a in ans["answers"]:
            if "failure_category" in a["answers"]: a["answers"]["failure_category"] = {"value": "product_defect"}
        rep = self._score(self.ds, self._dump("poor.json", ans))
        self.assertEqual(rep["questions"]["failure_category"]["per_class"]["provider"]["recall"], 0.0)
        self.assertGreater(rep["questions"]["failure_category"]["false_positive_burden"], 0)

    def test_unknown_metrics_not_zero(self):
        ans = self._load(self.ans)
        for a in ans["answers"]: a.pop("cost_usd", None); a.pop("latency_ms", None); a.pop("usage", None)
        rep = self._score(self.ds, self._dump("unk.json", ans))
        self.assertEqual(rep["totals"]["cost_usd"]["status"], "unknown"); self.assertIsNone(rep["totals"]["cost_usd"]["total"])
        self.assertEqual(rep["totals"]["latency_ms"]["status"], "unknown")

    def test_uncertain_answer_usage_is_in_totals(self):
        ans = self._load(self.ans)
        uncertain = next(a for a in ans["answers"] if a["disposition"] == "suggested")
        uncertain["disposition"] = "uncertain"
        uncertain["cost_usd"] = 9.0
        expected_cost = sum(a["cost_usd"] for a in ans["answers"] if isinstance(a.get("cost_usd"), (int, float)))
        expected_latency = sum(a["latency_ms"] for a in ans["answers"] if isinstance(a.get("latency_ms"), (int, float)))
        expected_input_tokens = sum(a["usage"]["input_tokens"] for a in ans["answers"]
                                    if isinstance(a.get("usage"), dict) and isinstance(a["usage"].get("input_tokens"), int))
        expected_output_tokens = sum(a["usage"]["output_tokens"] for a in ans["answers"]
                                     if isinstance(a.get("usage"), dict) and isinstance(a["usage"].get("output_tokens"), int))

        rep = self._score(self.ds, self._dump("uncertain-usage.json", ans))
        self.assertAlmostEqual(expected_cost, 9.008)
        self.assertAlmostEqual(rep["totals"]["cost_usd"]["total"], expected_cost)
        self.assertEqual(rep["totals"]["cost_usd"]["n"], 9)
        self.assertEqual(rep["totals"]["latency_ms"]["total"], expected_latency)
        self.assertEqual(rep["totals"]["input_tokens"]["total"], expected_input_tokens)
        self.assertEqual(rep["totals"]["output_tokens"]["total"], expected_output_tokens)

        uncertain.pop("cost_usd")
        missing_cost = self._score(self.ds, self._dump("uncertain-missing-cost.json", ans))
        self.assertEqual(missing_cost["totals"]["cost_usd"]["status"], "unknown")

    def _manifest(self, ds, **over):
        m = {"format_version": "decision-assistance-eval/1", "holdout_id": "holdout-1",
             "intake_snapshot_at": ds["intake_snapshot_at"], "assignments": {c["case_id"]: "train" for c in ds["cases"]}}
        m.update(over); return m

    def test_offset_intake_ordering(self):
        ds = self._load(self.ds)
        for stamp, accepted in (("2026-01-31T23:30:00-02:00", False),
                                ("2026-02-01T01:00:00+01:00", True)):
            with self.subTest(stamp=stamp):
                ds["cases"][0]["evidence_packet"]["collected_at"] = stamp
                path = self._dump("offset-ds.json", ds)
                result = self._run("validate", "--dataset", str(path))
                self.assertEqual(result.returncode, 0 if accepted else 2, result.stdout + result.stderr)
                report = self._score(path, self.ans)
                self.assertEqual(bool(report["validation_errors"]), not accepted)
                # A later dataset intake isolates the manifest's own guard.
                ds["intake_snapshot_at"] = "2026-03-01T00:00:00Z"
                manifest = self._manifest(ds, intake_snapshot_at="2026-02-01T00:00:00Z",
                                          holdout_id=f"offset-{accepted}")
                result = self._run("check-split", "--dataset", str(self._dump("offset-split-ds.json", ds)),
                                   "--manifest", str(self._dump("offset-manifest.json", manifest)))
                self.assertEqual(result.returncode, 0 if accepted else 2, result.stdout + result.stderr)
                ds["intake_snapshot_at"] = "2026-02-01T00:00:00Z"

    def test_time_split_orders_instants_and_equivalent_label_snapshot(self):
        ds = self._load(self.ds)
        ds["format_version"] = "decision-assistance-eval/2"
        ds["cases"] = ds["cases"][:2]
        train, hold = ds["cases"]
        train["evidence_packet"]["collected_at"] = "2026-01-01T23:00:00-02:00"
        hold["label"] = {
            "provenance": "independent_human", "labeler_id": "first",
            "labeler_role": "investigator", "labeled_at": "2026-01-10T00:00:00Z",
            "intake_snapshot_at": "2026-02-01T01:00:00+01:00", "value": hold["expected"],
            "second_label_or_adjudication": {
                "kind": "second_independent_label", "labeler_id": "second",
                "labeler_role": "investigator", "labeled_at": "2026-01-11T00:00:00Z",
                "value": hold["expected"]}}
        for stamp, accepted in (("2026-01-02T00:30:00Z", False),
                                ("2026-01-02T01:00:00Z", False),
                                ("2026-01-01T23:30:00-02:00", True)):
            with self.subTest(stamp=stamp):
                hold["evidence_packet"]["collected_at"] = stamp
                manifest = self._manifest(ds, format_version=ds["format_version"],
                                          holdout_id=f"time-{stamp}", split_strategy_time=True)
                manifest["assignments"][hold["case_id"]] = "holdout"
                result = self._run("check-split", "--dataset", str(self._dump("time-ds.json", ds)),
                                   "--manifest", str(self._dump("time-manifest.json", manifest)))
                self.assertEqual(result.returncode, 0 if accepted else 2, result.stdout + result.stderr)
                if not accepted:
                    self.assertIn("earliest holdout", result.stdout)

    def test_target_offsets_and_equivalent_deadline(self):
        targets = self._targets()
        for stamp, accepted in (("2026-01-31T23:30:00-02:00", False),
                                ("2026-02-01T01:00:00+01:00", False),
                                ("2026-02-01T00:30:00+01:00", True)):
            with self.subTest(stamp=stamp):
                targets["registered_at"] = stamp
                result = self._run("check-targets", "--targets", str(self._dump("offset-targets.json", targets)),
                                   "--holdout-opened-at", "2026-02-01T00:00:00Z")
                self.assertEqual(result.returncode, 0 if accepted else 2, result.stdout + result.stderr)

    def test_split_guards(self):
        ds = self._load(self.ds)
        # family overlap + synthetic holdout
        m = self._manifest(ds); first = ds["cases"][0]
        m["assignments"][first["case_id"]] = "holdout"
        # add a human twin sharing family to trigger synthetic-holdout (and not family overlap alone)
        r = self._run("check-split", "--dataset", str(self.ds), "--manifest", str(self._dump("m1.json", m)))
        self.assertEqual(r.returncode, 2)
        # reused holdout
        r = self._run("check-split", "--dataset", str(self.ds), "--manifest", str(self._dump("m2.json", self._manifest(ds))), "--prior-holdout-ids", "holdout-1")
        self.assertEqual(r.returncode, 2); self.assertIn("reused", r.stdout + json.dumps(json.loads(r.stdout)["errors"]))
        # post-snapshot evidence
        ds2 = self._load(self.ds); ds2["cases"][0]["evidence_packet"]["collected_at"] = "2027-01-01T00:00:00Z"; ds2["intake_snapshot_at"] = "2027-06-01T00:00:00Z"
        m3 = self._manifest(ds2); m3["intake_snapshot_at"] = "2026-02-01T00:00:00Z"
        r = self._run("check-split", "--dataset", str(self._dump("ds2.json", ds2)), "--manifest", str(self._dump("m3.json", m3)))
        self.assertEqual(r.returncode, 2)

    def test_missing_or_invalid_manifest_snapshot_rejected(self):
        ds = self._load(self.ds)
        ds["cases"][0]["evidence_packet"]["collected_at"] = "2027-01-01T00:00:00Z"
        ds["intake_snapshot_at"] = "2027-06-01T00:00:00Z"
        ds_path = self._dump("late-snapshot-ds.json", ds)
        for snapshot, suffix in ((None, "missing"), ("not-a-timestamp", "invalid")):
            with self.subTest(snapshot=snapshot):
                manifest = {"format_version": "decision-assistance-eval/1",
                            "holdout_id": f"holdout-snapshot-{suffix}",
                            "assignments": {c["case_id"]: "train" for c in ds["cases"]}}
                if snapshot is not None:
                    manifest["intake_snapshot_at"] = snapshot
                r = self._run("check-split", "--dataset", str(ds_path),
                              "--manifest", str(self._dump(f"manifest-{suffix}.json", manifest)))
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertIn("intake_snapshot_at", r.stdout)
                self.assertFalse(json.loads(r.stdout)["ok"])
        valid = {"format_version": "decision-assistance-eval/1", "holdout_id": "holdout-snapshot-valid",
                 "intake_snapshot_at": "2027-06-01T00:00:00Z",
                 "assignments": {c["case_id"]: "train" for c in ds["cases"]}}
        r = self._run("check-split", "--dataset", str(ds_path),
                      "--manifest", str(self._dump("manifest-valid.json", valid)))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_family_overlap_specifically(self):
        ds = self._load(self.ds)
        # make two cases share a family, split across holdout/train, holdout human-labeled
        ds["cases"][1]["family_id"] = ds["cases"][0]["family_id"]
        ds["cases"][0]["label"] = {"provenance": "independent_human"}
        m = self._manifest(ds); m["assignments"][ds["cases"][0]["case_id"]] = "holdout"
        r = self._run("check-split", "--dataset", str(self._dump("ds3.json", ds)), "--manifest", str(self._dump("m4.json", m)))
        self.assertEqual(r.returncode, 2); self.assertIn("family_id", r.stdout)

    def test_holdout_requires_independent_human_import_fields(self):
        v2 = "decision-assistance-eval/2"
        ds = self._load(self.ds)
        case = next(c for c in ds["cases"] if c["case_id"] == "seed-fail-provider")
        case["label"] = {"provenance": "independent_human"}
        manifest = self._manifest(ds)
        manifest["assignments"][case["case_id"]] = "holdout"
        r = self._run("check-split", "--dataset", str(self._dump("bare-human-ds.json", ds)),
                "--manifest", str(self._dump("bare-human-manifest.json", manifest)))
        self.assertEqual(r.returncode, 2, r.stdout)
        errors = json.loads(r.stdout)["errors"]
        for field in ("labeler_id", "labeler_role", "labeled_at", "intake_snapshot_at",
                      "second_label_or_adjudication"):
            self.assertTrue(any(field in error for error in errors), errors)
        missing = [e for e in errors if "missing required import field" in e]
        self.assertTrue(missing and all(", value" not in e and "value," not in e for e in missing), errors)

        v2_bare_ds = self._load(self.ds)
        v2_bare_ds["format_version"] = v2
        v2_bare_case = next(c for c in v2_bare_ds["cases"] if c["case_id"] == "seed-fail-provider")
        v2_bare_case["label"] = {"provenance": "independent_human"}
        v2_bare_manifest = self._manifest(v2_bare_ds, format_version=v2, holdout_id="holdout-bare-v2")
        v2_bare_manifest["assignments"][v2_bare_case["case_id"]] = "holdout"
        r = self._run("check-split", "--dataset", str(self._dump("bare-human-v2-ds.json", v2_bare_ds)),
                "--manifest", str(self._dump("bare-human-v2-manifest.json", v2_bare_manifest)))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        errors = json.loads(r.stdout)["errors"]
        for field in ("labeler_id", "labeler_role", "labeled_at", "intake_snapshot_at",
                      "value", "second_label_or_adjudication"):
            self.assertTrue(any(field in error for error in errors), errors)

        valid_records = [
            {
                "kind": "second_independent_label",
                "labeler_id": "labeler-2",
                "labeler_role": "incident investigator",
                "labeled_at": "2026-01-12T00:00:00Z",
                "value": case["expected"],
            },
            {
                "kind": "adjudication",
                "adjudicator_id": "adjudicator-1",
                "adjudicator_role": "senior investigator",
                "adjudicated_at": "2026-01-13T00:00:00Z",
                "original_labels": [
                    {"labeler_id": "labeler-1", "labeler_role": "investigator",
                     "labeled_at": "2026-01-10T00:00:00Z", "value": case["expected"]},
                    {"labeler_id": "labeler-2", "labeler_role": "investigator",
                     "labeled_at": "2026-01-12T00:00:00Z", "value": {"failure_category": {"value": "product_defect"}}},
                ],
                "rationale_category": "evidence_review",
                "value": case["expected"],
            },
        ]
        v1_adjudication = valid_records[1]
        v1_adj_ds = self._load(self.ds)
        v1_adj_case = next(c for c in v1_adj_ds["cases"] if c["case_id"] == "seed-fail-provider")
        v1_adj_case["label"] = {
            "provenance": "independent_human",
            "labeler_id": "labeler-1",
            "labeler_role": "incident investigator",
            "labeled_at": "2026-01-10T00:00:00Z",
            "intake_snapshot_at": v1_adj_ds["intake_snapshot_at"],
            "second_label_or_adjudication": v1_adjudication,
        }
        v1_adj_manifest = self._manifest(v1_adj_ds, holdout_id="holdout-human-v1-adj")
        v1_adj_manifest["assignments"][v1_adj_case["case_id"]] = "holdout"
        r = self._run("check-split", "--dataset",
                str(self._dump("valid-human-v1-adj-ds.json", v1_adj_ds)),
                "--manifest", str(self._dump("valid-human-v1-adj-manifest.json", v1_adj_manifest)))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

        for index, record in enumerate(valid_records):
            with self.subTest(kind=record["kind"]):
                valid_ds = self._load(self.ds)
                valid_ds["format_version"] = v2
                valid_case = next(c for c in valid_ds["cases"] if c["case_id"] == "seed-fail-provider")
                valid_case["label"] = {
                    "provenance": "independent_human",
                    "labeler_id": "labeler-1",
                    "labeler_role": "incident investigator",
                    "labeled_at": "2026-01-10T00:00:00Z",
                    "intake_snapshot_at": valid_ds["intake_snapshot_at"],
                    "value": valid_case["expected"],
                    "second_label_or_adjudication": record,
                }
                valid_manifest = self._manifest(valid_ds, format_version=v2, holdout_id=f"holdout-human-{index}")
                valid_manifest["assignments"][valid_case["case_id"]] = "holdout"
                r = self._run("check-split", "--dataset",
                        str(self._dump(f"valid-human-ds-{index}.json", valid_ds)),
                        "--manifest", str(self._dump(f"valid-human-manifest-{index}.json", valid_manifest)))
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_check_split_rejects_malformed_holdout_provenance_timestamps(self):
        ds = self._load(self.ds)
        case = next(c for c in ds["cases"] if c["case_id"] == "seed-fail-provider")
        case["label"] = {
            "provenance": "independent_human",
            "labeler_id": "labeler-1",
            "labeler_role": "incident investigator",
            "labeled_at": "2026-01-10T00:00:00Z",
            "intake_snapshot_at": ds["intake_snapshot_at"],
            "second_label_or_adjudication": {
                "kind": "adjudication",
                "adjudicator_id": "adjudicator-1",
                "adjudicator_role": "senior investigator",
                "adjudicated_at": "2026-01-13T00:00:00Z",
                "original_labels": [
                    {"labeler_id": "labeler-1", "labeler_role": "investigator",
                     "labeled_at": "2026-01-10T00:00:00Z", "value": case["expected"]},
                    {"labeler_id": "labeler-2", "labeler_role": "investigator",
                     "labeled_at": "2026-01-12T00:00:00Z", "value": case["expected"]},
                ],
                "rationale_category": "evidence_review",
                "value": case["expected"],
            },
        }
        manifest = self._manifest(ds, holdout_id="holdout-malformed-provenance")
        manifest["assignments"][case["case_id"]] = "holdout"

        for field in ("labeled_at", "intake_snapshot_at", "adjudicated_at"):
            with self.subTest(field=field):
                invalid_ds = json.loads(json.dumps(ds))
                invalid_label = next(c for c in invalid_ds["cases"]
                                     if c["case_id"] == "seed-fail-provider")["label"]
                if field == "adjudicated_at":
                    invalid_label["second_label_or_adjudication"][field] = "zzzz-zz-zz"
                else:
                    invalid_label[field] = "zzzz-zz-zz"
                invalid_manifest = json.loads(json.dumps(manifest))
                invalid_manifest["holdout_id"] = f"holdout-malformed-{field}"
                result = self._run(
                    "check-split", "--dataset",
                    str(self._dump(f"malformed-{field}-ds.json", invalid_ds)),
                    "--manifest",
                    str(self._dump(f"malformed-{field}-manifest.json", invalid_manifest)))
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                report = json.loads(result.stdout)
                self.assertFalse(report["ok"])
                self.assertTrue(any(field in error and "must be ISO-8601" in error
                                    for error in report["errors"]), report["errors"])

    def test_v1_second_label_cannot_hide_disagreement_after_expected_is_rewritten(self):
        # v1 records no first labeler value, so a second label equal to a rewritten
        # expected passed check-split before the fix (base evaluator returns 0).
        ds = self._load(self.ds)
        case = next(c for c in ds["cases"] if c["case_id"] == "seed-fail-provider")
        case["expected"] = {"failure_category": {"value": "product_defect"}}
        case["label"] = {
            "provenance": "independent_human",
            "labeler_id": "labeler-1",
            "labeler_role": "incident investigator",
            "labeled_at": "2026-01-10T00:00:00Z",
            "intake_snapshot_at": ds["intake_snapshot_at"],
            "second_label_or_adjudication": {
                "kind": "second_independent_label",
                "labeler_id": "labeler-2",
                "labeler_role": "incident investigator",
                "labeled_at": "2026-01-12T00:00:00Z",
                "value": case["expected"],
            },
        }
        manifest = self._manifest(ds, holdout_id="holdout-legacy-v1")
        manifest["assignments"][case["case_id"]] = "holdout"
        r = self._run("check-split", "--dataset", str(self._dump("legacy-v1-ds.json", ds)),
                "--manifest", str(self._dump("legacy-v1-manifest.json", manifest)))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        errors = json.loads(r.stdout)["errors"]
        self.assertTrue(any("legacy v1 second label" in error for error in errors), errors)

    def test_holdout_rejects_second_label_disagreement_after_expected_is_rewritten(self):
        v2 = "decision-assistance-eval/2"
        ds = self._load(self.ds)
        ds["format_version"] = v2
        case = next(c for c in ds["cases"] if c["case_id"] == "seed-fail-provider")
        first_label = {"failure_category": {"value": "provider"}}
        second_label = {"failure_category": {"value": "product_defect"}}
        case["label"] = {
            "provenance": "independent_human",
            "labeler_id": "labeler-1",
            "labeler_role": "incident investigator",
            "labeled_at": "2026-01-10T00:00:00Z",
            "intake_snapshot_at": ds["intake_snapshot_at"],
            "value": first_label,
            "second_label_or_adjudication": {
                "kind": "second_independent_label",
                "labeler_id": "labeler-2",
                "labeler_role": "incident investigator",
                "labeled_at": "2026-01-12T00:00:00Z",
                "value": second_label,
            },
        }
        case["expected"] = second_label
        manifest = self._manifest(ds, format_version=v2)
        manifest["assignments"][case["case_id"]] = "holdout"
        r = self._run("check-split", "--dataset",
                str(self._dump("disagreeing-human-ds.json", ds)),
                "--manifest", str(self._dump("disagreeing-human-manifest.json", manifest)))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        errors = json.loads(r.stdout)["errors"]
        self.assertTrue(any("disagrees with the first label" in error for error in errors), errors)

    def test_default_split_rejects_previously_accepted_holdout(self):
        manifest = self._dump("manifest.json", self._manifest(self._load(self.ds)))
        args = ("check-split", "--dataset", str(self.ds), "--manifest", str(manifest))
        first = self._run(*args)
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        for path in (manifest, self._dump("copied-manifest.json", self._load(manifest))):
            with self.subTest(manifest=path.name):
                repeated = self._run("check-split", "--dataset", str(self.ds), "--manifest", str(path))
                self.assertEqual(repeated.returncode, 2, repeated.stdout + repeated.stderr)
                self.assertFalse(json.loads(repeated.stdout)["ok"])
                self.assertIn("reused", repeated.stdout)

    def test_check_split_refuses_manifests_that_leave_cases_unassigned(self):
        # Every dataset case must be assigned. Missing, empty and partial maps were
        # accepted and opened the holdout, so refusal must come before any reservation.
        holdouts = self.root / ".decision-assistance-eval" / "holdouts"
        def opened(): return sorted(holdouts.glob("*.json")) if holdouts.exists() else []
        for version in ("1", "2"):
            format_version = f"decision-assistance-eval/{version}"
            with self.subTest(format_version=format_version):
                ds = self._load(self.ds)
                ds["format_version"] = format_version
                ds_path = self._dump(f"unassigned-v{version}-ds.json", ds)
                hid = f"unassigned-v{version}"
                complete = self._manifest(ds, holdout_id=hid, format_version=format_version)
                omitted = ds["cases"][-1]["case_id"]
                partial = {cid: split for cid, split in complete["assignments"].items() if cid != omitted}
                refused = (
                    ("no-assignments-key", {k: v for k, v in complete.items() if k != "assignments"}),
                    ("empty-assignments", {**complete, "assignments": {}}),
                    ("one-case-omitted", {**complete, "assignments": partial}),
                )
                for name, manifest in refused:
                    with self.subTest(manifest=name):
                        before = opened()
                        r = self._run("check-split", "--dataset", str(ds_path),
                                      "--manifest", str(self._dump(f"{name}-v{version}.json", manifest)))
                        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                        body = json.loads(r.stdout)
                        self.assertFalse(body["ok"])
                        self.assertTrue(any(omitted in error for error in body["errors"]), body["errors"])
                        self.assertEqual(opened(), before)
                # Refusals reserved nothing, so the same holdout_id still opens with a complete map.
                r = self._run("check-split", "--dataset", str(ds_path), "--manifest", str(self._dump(f"complete-v{version}.json", complete)))
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_default_targets_reject_registration_after_recorded_holdout(self):
        manifest = self._dump("manifest.json", self._manifest(self._load(self.ds)))
        opened = self._run("check-split", "--dataset", str(self.ds), "--manifest", str(manifest))
        self.assertEqual(opened.returncode, 0, opened.stdout + opened.stderr)
        targets = self._targets()
        targets["registered_at"] = (datetime.now(timezone.utc) + timedelta(days=1)).isoformat()
        # Exercise the existing no-identifier form as well as explicit holdout binding.
        for holdout_id in (None, "holdout-1"):
            with self.subTest(holdout_id=holdout_id):
                if holdout_id is not None:
                    targets["holdout_id"] = holdout_id
                p = self._dump("late-targets.json", targets)
                late = self._run("check-targets", "--targets", str(p))
                self.assertEqual(late.returncode, 2, late.stdout + late.stderr)
                self.assertFalse(json.loads(late.stdout)["ok"])
                self.assertIn("before holdout access", late.stdout)
                # A supplied later timestamp cannot weaken the recorded deadline.
                override = self._run("check-targets", "--targets", str(p),
                                     "--holdout-opened-at", "9999-01-01T00:00:00Z")
                self.assertEqual(override.returncode, 2, override.stdout + override.stderr)
        targets["registered_at"] = "2026-01-15T00:00:00Z"
        early = self._run("check-targets", "--targets", str(self._dump("early-targets.json", targets)))
        self.assertEqual(early.returncode, 0, early.stdout + early.stderr)

    def test_malformed_values_rejected_structured(self):
        ds = self._load(self.ds)
        ans = self._load(self.ans)
        # Non-object usage is rejected by validate and reported by score.
        bad_usage = json.loads(json.dumps(ans))
        bad_usage["answers"][0]["usage"] = 5
        bad_usage_path = self._dump("malformed-usage-object.json", bad_usage)
        r = self._run("validate", "--dataset", str(self.ds), "--answers", str(bad_usage_path))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertFalse(json.loads(r.stdout)["ok"])
        self.assertIn("usage", r.stdout)
        rep = self._score(self.ds, bad_usage_path)
        self.assertTrue(rep["validation_errors"], rep)
        self.assertTrue(any("usage" in e for e in rep["validation_errors"]), rep)
        # Negative token counts are rejected by validate and reported by score.
        bad_tokens = json.loads(json.dumps(ans))
        bad_tokens["answers"][0]["usage"] = {"input_tokens": -500, "output_tokens": 10}
        bad_tokens_path = self._dump("malformed-usage-tokens.json", bad_tokens)
        r = self._run("validate", "--dataset", str(self.ds), "--answers", str(bad_tokens_path))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertFalse(json.loads(r.stdout)["ok"])
        self.assertIn("usage.input_tokens", r.stdout)
        rep = self._score(self.ds, bad_tokens_path)
        self.assertTrue(rep["validation_errors"], rep)
        # Non-list criterion_refs on an answer is rejected, not a traceback.
        bad_refs = json.loads(json.dumps(ans))
        bad_refs["answers"][0]["answers"]["failure_category"]["criterion_refs"] = 5
        bad_refs_path = self._dump("malformed-criterion-refs-answer.json", bad_refs)
        r = self._run("validate", "--dataset", str(self.ds), "--answers", str(bad_refs_path))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("criterion_refs", r.stdout)
        rep = self._score(self.ds, bad_refs_path)
        self.assertTrue(rep["validation_errors"], rep)
        # Non-list criterion_refs on a dataset is rejected everywhere.
        ds_refs = json.loads(json.dumps(ds))
        kontra = next(c for c in ds_refs["cases"] if c["case_id"] == "seed-task-contradictory")
        kontra["expected"]["requirements_conflict"]["criterion_refs"] = 5
        ds_refs_path = self._dump("malformed-criterion-refs-dataset.json", ds_refs)
        r = self._run("validate", "--dataset", str(ds_refs_path))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("criterion_refs", r.stdout)
        rep = self._score(ds_refs_path, self.ans)
        self.assertTrue(rep["validation_errors"], rep)
        r = self._run("check-split", "--dataset", str(ds_refs_path),
                      "--manifest", str(self._dump("malformed-refs-manifest.json", self._manifest(ds_refs))))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("criterion_refs", r.stdout)
        # A case without expected is rejected by validate and scored structurally.
        ds_no_expected = json.loads(json.dumps(ds))
        del ds_no_expected["cases"][0]["expected"]
        ds_no_expected_path = self._dump("missing-expected-ds.json", ds_no_expected)
        r = self._run("validate", "--dataset", str(ds_no_expected_path))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("expected answers required", r.stdout)
        rep = self._score(ds_no_expected_path, self.ans)
        self.assertTrue(rep["validation_errors"], rep)
        # Missing collected_at with time splitting is a structured check-split error.
        ds_no_ts = json.loads(json.dumps(ds))
        del ds_no_ts["cases"][0]["evidence_packet"]["collected_at"]
        ds_no_ts_path = self._dump("missing-collected-at-ds.json", ds_no_ts)
        r = self._run("validate", "--dataset", str(ds_no_ts_path))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        manifest = self._manifest(ds_no_ts, holdout_id="holdout-malformed-time")
        manifest["split_strategy_time"] = True
        r = self._run("check-split", "--dataset", str(ds_no_ts_path),
                      "--manifest", str(self._dump("malformed-time-manifest.json", manifest)))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertFalse(json.loads(r.stdout)["ok"])
        self.assertIn("collected_at", r.stdout)

    def test_malformed_case_id_rejected_structured(self):
        # ORB-19837: unhashable case identity (array/object) must not traceback.
        control = self._score(self.ds, self.ans)
        self.assertEqual(control["validation_errors"], [])
        self.assertGreater(control["totals"]["cases"], 0)
        # Dataset records with array/object case_id are rejected, not a traceback.
        for index, bad in enumerate(([], {})):
            with self.subTest(document="dataset", case_id=repr(bad)):
                ds = self._load(self.ds)
                ds["cases"][0]["case_id"] = bad
                ds_path = self._dump(f"malformed-case-id-dataset-{index}.json", ds)
                r = self._run("validate", "--dataset", str(ds_path), "--answers", str(self.ans))
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertNotIn("Traceback", r.stderr)
                self.assertFalse(json.loads(r.stdout)["ok"])
                self.assertIn("case_id", r.stdout)
                rep = self._score(ds_path, self.ans)
                self.assertNotIn("Traceback", r.stderr)
                self.assertTrue(rep["validation_errors"], rep)
                self.assertTrue(any("case_id" in e for e in rep["validation_errors"]), rep)
                self.assertEqual(rep["totals"]["cases"], control["totals"]["cases"])
                self.assertEqual(rep["totals"]["answered"], control["totals"]["answered"] - 1)
                self.assertEqual(rep["totals"]["abstained"], control["totals"]["abstained"] + 1)
        # Answer records with array/object case_id are rejected, not a traceback.
        for index, bad in enumerate(([], {})):
            with self.subTest(document="answers", case_id=repr(bad)):
                ans = self._load(self.ans)
                ans["answers"][0]["case_id"] = bad
                ans_path = self._dump(f"malformed-case-id-answers-{index}.json", ans)
                r = self._run("validate", "--dataset", str(self.ds), "--answers", str(ans_path))
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertNotIn("Traceback", r.stderr)
                self.assertFalse(json.loads(r.stdout)["ok"])
                self.assertIn("case_id", r.stdout)
                rep = self._score(self.ds, ans_path)
                self.assertTrue(rep["validation_errors"], rep)
                self.assertTrue(any("case_id" in e for e in rep["validation_errors"]), rep)
                self.assertEqual(rep["totals"]["cases"], control["totals"]["cases"])
                self.assertEqual(rep["totals"]["answered"], control["totals"]["answered"] - 1)

    def test_criterion_refs_checked_against_packet(self):
        import hashlib
        ds = self._load(self.ds)
        ans = self._load(self.ans)
        # Mismatched digest: ref index exists but digest belongs to other text.
        bad_digest = json.loads(json.dumps(ans))
        for a in bad_digest["answers"]:
            if a["case_id"] == "seed-task-contradictory":
                other = "sha256:" + hashlib.sha256(b"unrelated criterion text").hexdigest()
                a["answers"]["requirements_conflict"]["criterion_refs"][0] = {"index": 0, "text_digest": other}
        r = self._run("validate", "--dataset", str(self.ds),
                      "--answers", str(self._dump("bad-digest-ans.json", bad_digest)))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertFalse(json.loads(r.stdout)["ok"])
        self.assertIn("criterion_ref", r.stdout)
        self.assertIn("digest does not match", r.stdout)
        # Unknown index: well-formed ref whose index is absent from the packet.
        bad_index = json.loads(json.dumps(ans))
        for a in bad_index["answers"]:
            if a["case_id"] == "seed-task-contradictory":
                a["answers"]["requirements_conflict"]["criterion_refs"] = [
                    {"index": 999, "text_digest": "sha256:" + hashlib.sha256(b"x").hexdigest()}]
        r = self._run("validate", "--dataset", str(self.ds),
                      "--answers", str(self._dump("bad-index-ans.json", bad_index)))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertFalse(json.loads(r.stdout)["ok"])
        self.assertIn("not in packet criteria", r.stdout)
        # Task-quality packet without criteria is rejected.
        no_criteria = json.loads(json.dumps(ds))
        for c in no_criteria["cases"]:
            if c["case_id"] == "seed-task-vague":
                del c["evidence_packet"]["criteria"]
        r = self._run("validate", "--dataset", str(self._dump("no-criteria-ds.json", no_criteria)))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertFalse(json.loads(r.stdout)["ok"])
        self.assertIn("criteria", r.stdout)

    def _targets(self):
        return {"format_version": "decision-assistance-eval/1", "registered_at": "2026-01-15T00:00:00Z",
                "baseline": {"name": "existing-workflow", "version": "1"},
                "targets": [{"question": q, "min_precision": 0.8, "min_recall": 0.5, "min_samples": 20}
                            for q in ["failure_category", "outcome_unclear", "criterion_not_observable",
                                      "requirements_conflict", "scope_clarification"]]}

    def test_target_min_samples_must_be_a_positive_integer(self):
        targets = self._targets()
        targets["targets"][0]["min_samples"] = 1
        accepted = self._run("check-targets", "--targets", str(self._dump("positive-sample-targets.json", targets)))
        self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)
        self.assertTrue(json.loads(accepted.stdout)["ok"])

        for value in (0, -1, 1.5, "1", True, False):
            with self.subTest(min_samples=value):
                invalid_targets = self._targets()
                invalid_targets["targets"][0]["min_samples"] = value
                result = self._run("check-targets", "--targets",
                                   str(self._dump(f"invalid-sample-{type(value).__name__}.json", invalid_targets)))
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                report = json.loads(result.stdout)
                self.assertFalse(report["ok"])
                self.assertTrue(any("min_samples must be positive int" in error for error in report["errors"]))

    def test_target_precision_and_recall_must_not_be_booleans(self):
        for key in ("min_precision", "min_recall"):
            for value in (True, False):
                with self.subTest(key=key, value=value):
                    targets = self._targets()
                    targets["targets"][0][key] = value
                    result = self._run("check-targets", "--targets",
                                       str(self._dump(f"invalid-{key}-{str(value).lower()}.json", targets)))
                    self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                    report = json.loads(result.stdout)
                    self.assertFalse(report["ok"])
                    self.assertTrue(any(f"{key} must be in [0,1]" in error for error in report["errors"]))

    def test_targets_before_holdout_only(self):
        t = self._targets()
        p = self._dump("t.json", t)
        self.assertEqual(self._run("check-targets", "--targets", str(p), "--holdout-opened-at", "2026-02-01T00:00:00Z").returncode, 0)
        self.assertEqual(self._run("check-targets", "--targets", str(p), "--holdout-opened-at", "2026-01-01T00:00:00Z").returncode, 2)

    def _assert_structured_refusal(self, result):
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(result.stderr, "", "a refusal reaches stdout only; no traceback or diagnostic on stderr")
        report = json.loads(result.stdout)
        self.assertFalse(report["ok"])
        self.assertTrue(report["errors"])
        self.assertTrue(all(isinstance(error, str) and error for error in report["errors"]), report["errors"])
        return report["errors"]

    def test_malformed_target_documents_refuse_naming_the_field(self):
        valid = self._targets()
        cases = [
            ("top-level array", [], "targets"),
            ("top-level string", "failure_category", "targets"),
            ("baseline array", {**valid, "baseline": []}, "baseline"),
            ("baseline null", {**valid, "baseline": None}, "baseline"),
            ("targets null", {**valid, "targets": None}, "targets"),
            ("targets string", {**valid, "targets": "failure_category"}, "targets"),
            ("targets object", {**valid, "targets": {"failure_category": valid["targets"][0]}}, "targets"),
            ("row null", {**valid, "targets": [None, *valid["targets"][1:]]}, "targets[0]"),
            ("row string", {**valid, "targets": ["failure_category", *valid["targets"][1:]]}, "targets[0]"),
            ("row array", {**valid, "targets": [["failure_category"], *valid["targets"][1:]]}, "targets[0]"),
            ("question array", {**valid, "targets": [{**valid["targets"][0], "question": ["failure_category"]}, *valid["targets"][1:]]}, "targets[0]"),
        ]
        for index, (label, doc, field) in enumerate(cases):
            with self.subTest(case=label):
                result = self._run("check-targets", "--targets", str(self._dump(f"malformed-target-{index}.json", doc)))
                errors = self._assert_structured_refusal(result)
                self.assertTrue(any(field in error for error in errors), errors)

    def test_unreadable_or_invalid_target_json_refuses_without_traceback(self):
        payloads = {
            "invalid JSON": b"{",
            "empty file": b"",
            "non-UTF-8 bytes": b"\xff\xfe\x00",
            "deeply nested JSON": b"[" * 1000000 + b"]" * 1000000,
        }
        for label, payload in payloads.items():
            with self.subTest(case=label):
                path = self.root / f"unreadable-{label.replace(' ', '-')}.json"
                path.write_bytes(payload)
                self._assert_structured_refusal(self._run("check-targets", "--targets", str(path)))
        for label, path in (("missing file", self.root / "missing-targets.json"), ("directory", self.root)):
            with self.subTest(case=label):
                self._assert_structured_refusal(self._run("check-targets", "--targets", str(path)))

if __name__ == "__main__": unittest.main()
