#!/usr/bin/env python3
"""Public-CLI verification for decision-assistance evaluation (acceptance criterion 4).

Exercises scripts/evaluate-decision-assistance.py only through its CLI on
isolated fixtures under .orbit/tmp: malformed answers, insufficient evidence,
refusals, a deliberately poor classifier, split/target guards, unknown metrics.
No external calls.
"""
import json, subprocess, sys, tempfile, unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
TOOL = SCRIPTS / "evaluate-decision-assistance.py"

def run(*args):
    return subprocess.run([sys.executable, str(TOOL), *args], capture_output=True, text=True)

class EvalTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"; scratch.mkdir(parents=True, exist_ok=True)
        td = tempfile.TemporaryDirectory(prefix="decision-eval-", dir=scratch); self.addCleanup(td.cleanup)
        self.root = Path(td.name)
        self.ds = self._dump("ds.json", json.loads(run("seed").stdout))
        self.ans = self._dump("ans.json", json.loads(run("seed", "--answers").stdout))

    def _dump(self, name, obj):
        p = self.root / name; p.write_text(json.dumps(obj)); return p

    def _load(self, p): return json.loads(Path(p).read_text())
    def _score(self, ds, ans): 
        r = run("score", "--dataset", str(ds), "--answers", str(ans)); self.assertEqual(r.returncode, 0, r.stderr); return json.loads(r.stdout)

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

    def test_malformed_answers_rejected(self):
        ans = self._load(self.ans); ans["answers"][0]["answers"] = {"failure_category": {"value": "bogus", "probability": 1.5}, "extra": {"value": 1}}
        p = self._dump("bad.json", ans); r = run("validate", "--dataset", str(self.ds), "--answers", str(p))
        self.assertEqual(r.returncode, 2); self.assertFalse(json.loads(r.stdout)["ok"])
        rep = self._score(self.ds, p); self.assertGreaterEqual(rep["totals"]["invalid"], 1)

    def test_refusal_abstains(self):
        ans = self._load(self.ans); ans["answers"][0] = {"case_id": ans["answers"][0]["case_id"], "disposition": "refused", "answers": {}}
        rep = self._score(self.ds, self._dump("ref.json", ans))
        self.assertGreaterEqual(rep["totals"]["refused_or_unavailable"], 1); self.assertGreaterEqual(rep["totals"]["abstained"], 1)

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

    def _manifest(self, ds, **over):
        m = {"format_version": "decision-assistance-eval/1", "holdout_id": "holdout-1",
             "intake_snapshot_at": ds["intake_snapshot_at"], "assignments": {c["case_id"]: "train" for c in ds["cases"]}}
        m.update(over); return m

    def test_split_guards(self):
        ds = self._load(self.ds)
        # family overlap + synthetic holdout
        m = self._manifest(ds); first = ds["cases"][0]
        m["assignments"][first["case_id"]] = "holdout"
        # add a human twin sharing family to trigger synthetic-holdout (and not family overlap alone)
        r = run("check-split", "--dataset", str(self.ds), "--manifest", str(self._dump("m1.json", m)))
        self.assertEqual(r.returncode, 2)
        # reused holdout
        r = run("check-split", "--dataset", str(self.ds), "--manifest", str(self._dump("m2.json", self._manifest(ds))), "--prior-holdout-ids", "holdout-1")
        self.assertEqual(r.returncode, 2); self.assertIn("reused", r.stdout + json.dumps(json.loads(r.stdout)["errors"]))
        # post-snapshot evidence
        ds2 = self._load(self.ds); ds2["cases"][0]["evidence_packet"]["collected_at"] = "2027-01-01T00:00:00Z"; ds2["intake_snapshot_at"] = "2027-06-01T00:00:00Z"
        m3 = self._manifest(ds2); m3["intake_snapshot_at"] = "2026-02-01T00:00:00Z"
        r = run("check-split", "--dataset", str(self._dump("ds2.json", ds2)), "--manifest", str(self._dump("m3.json", m3)))
        self.assertEqual(r.returncode, 2)

    def test_family_overlap_specifically(self):
        ds = self._load(self.ds)
        # make two cases share a family, split across holdout/train, holdout human-labeled
        ds["cases"][1]["family_id"] = ds["cases"][0]["family_id"]
        ds["cases"][0]["label"] = {"provenance": "independent_human"}
        m = self._manifest(ds); m["assignments"][ds["cases"][0]["case_id"]] = "holdout"
        r = run("check-split", "--dataset", str(self._dump("ds3.json", ds)), "--manifest", str(self._dump("m4.json", m)))
        self.assertEqual(r.returncode, 2); self.assertIn("family_id", r.stdout)

    def test_targets_before_holdout_only(self):
        t = {"format_version": "decision-assistance-eval/1", "registered_at": "2026-01-15T00:00:00Z",
             "baseline": {"name": "existing-workflow", "version": "1"},
             "targets": [{"question": q, "min_precision": 0.8, "min_recall": 0.5, "min_samples": 20}
                         for q in ["failure_category", "outcome_unclear", "criterion_not_observable", "requirements_conflict", "scope_clarification"]]}
        p = self._dump("t.json", t)
        self.assertEqual(run("check-targets", "--targets", str(p), "--holdout-opened-at", "2026-02-01T00:00:00Z").returncode, 0)
        self.assertEqual(run("check-targets", "--targets", str(p), "--holdout-opened-at", "2026-01-01T00:00:00Z").returncode, 2)

if __name__ == "__main__": unittest.main()
