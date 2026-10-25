"""Public-CLI regression tests for crew type discovery suggestions.

Guards the repair for the defect where a tree leaf combining several
branches (``tag a and tag b``, ``tag a and not tag b``) was serialized
into a ``tags = [...]`` block that the real configuration matcher reads
as any-of, so the reported n/mean described a different population than
the pasted type would match. A second guard keeps a suggested block off
the reserved fallback name ``other``, which configuration refuses. Only
the script's public CLI is exercised; the export payloads below are
private fixtures driving that behavior.
"""

import json
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "crew_type_discovery.py"
FIXTURES = Path(__file__).resolve().parents[1] / "fixtures"

TERMS = [
    {"key": "weighted_landed", "weight": 1.0},
    {"key": "first_pass_accepts", "weight": 1.0},
    {"key": "review_rejects", "weight": -1.0},
    {"key": "reviewer_repairs", "weight": -0.5},
    {"key": "crew_rework", "weight": -1.0},
    {"key": "escaped_regressions", "weight": -2.0},
]


def build_conjunction_export():
    """120 rows, one crew/model/tier, 30 rows per tag set of a/b."""
    tasks = []
    for index, tags in enumerate(([], ["a"], ["b"], ["a", "b"])):
        for copy in range(30):
            tasks.append(
                {
                    "task_id": f"synthetic-{index}-{copy}",
                    "crew": "alpha",
                    "model": "model-a",
                    "tier": "medium",
                    "type": "other",
                    "tags": list(tags),
                    "context_files": [],
                    "path_prefixes": [],
                    "counts": {
                        "weighted_landed": int("a" in tags),
                        "first_pass_accepts": int("b" in tags),
                        "review_rejects": 0,
                        "reviewer_repairs": 0,
                        "crew_rework": 0,
                        "escaped_regressions": 0,
                    },
                }
            )
    return {"terms": TERMS, "tasks": tasks}


def run_cli(payload=None, fixture=None, extra=("--json",)):
    """Run the discovery script; payload goes via stdin, fixture via path."""
    args = [sys.executable, str(SCRIPT), *extra]
    if fixture is not None:
        args.append(str(fixture))
    proc = subprocess.run(
        args,
        input=json.dumps(payload) if payload is not None else None,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert proc.returncode == 0, f"CLI failed: {proc.stderr}"
    return proc.stdout


def relative_scores(payload):
    """Independent oracle for per-task comparative-advantage scores.

    Reimplements the documented formula (raw = 100 + sum(weight *
    (actual - fleet rate) * 100), relative to the crew's own tier mean)
    straight from the export rows, without touching the script's code.
    """
    rows = payload["tasks"] if isinstance(payload, dict) else payload
    weights = {term["key"]: term["weight"] for term in TERMS}
    tier_totals: dict = {}
    tier_n: dict = {}
    for row in rows:
        tier = row["tier"]
        tier_n[tier] = tier_n.get(tier, 0) + 1
        for key, value in row["counts"].items():
            tier_totals.setdefault(tier, {}).setdefault(key, 0.0)
            tier_totals[tier][key] += float(value or 0)
    rates = {
        tier: {key: total / tier_n[tier] for key, total in counts.items()}
        for tier, counts in tier_totals.items()
    }
    raws = []
    for row in rows:
        raw = 100.0
        for key, weight in weights.items():
            actual = float(row["counts"].get(key, 0) or 0)
            raw += weight * (actual - rates[row["tier"]].get(key, 0.0)) * 100.0
        raws.append(raw)
    crew_groups: dict = {}
    for row, raw in zip(rows, raws):
        crew_groups.setdefault((row["crew"], row["model"], row["tier"]), []).append(raw)
    crew_mean = {key: sum(vals) / len(vals) for key, vals in crew_groups.items()}
    return [
        raw - crew_mean[(row["crew"], row["model"], row["tier"])]
        for row, raw in zip(rows, raws)
    ]


def matched_by_suggestion(rows, suggestion):
    """Rows the real matcher assigns to a tag-only suggestion (any-of)."""
    wanted = set(suggestion["tags"])
    return [row for row in rows if wanted & set(row.get("tags") or [])]


class DiscoveryCliTest(unittest.TestCase):
    def test_conjunction_leaf_never_becomes_any_of_suggestion(self):
        payload = build_conjunction_export()
        report = json.loads(run_cli(payload=payload))
        self.assertIn("unsupported", report)
        unsupported_predicates = [u["predicate"] for u in report["unsupported"]]
        self.assertIn("tag a and tag b", unsupported_predicates)
        conjunction = next(
            u for u in report["unsupported"] if u["predicate"] == "tag a and tag b"
        )
        self.assertEqual(conjunction["n"], 30)
        self.assertIn("conjunction", conjunction["reason"])
        for suggestion in report["suggestions"]:
            self.assertNotEqual(
                suggestion["predicate"],
                "tag a and tag b",
                "a conjunction leaf must not ship as a ready-to-paste block",
            )
        self.assertEqual(
            report["suggestions"],
            [],
            "no leaf of the a/b export is expressible as one any-of matcher",
        )

    def test_exclusion_leaves_never_become_single_tag_suggestions(self):
        payload = build_conjunction_export()
        report = json.loads(run_cli(payload=payload))
        unsupported_predicates = [u["predicate"] for u in report["unsupported"]]
        self.assertIn("tag a and not tag b", unsupported_predicates)
        self.assertIn("not tag a and tag b", unsupported_predicates)
        for suggestion in report["suggestions"]:
            self.assertFalse(
                suggestion["tags"] == ["a"] and suggestion["n"] == 30,
                "tag a and not tag b (n=30) must not ship as tags=[a] (n=60)",
            )
            self.assertFalse(
                suggestion["tags"] == ["b"] and suggestion["n"] == 30,
                "not tag a and tag b (n=30) must not ship as tags=[b] (n=60)",
            )
        self.assertEqual(report["suggestions"], [])

    def test_emitted_suggestion_matches_its_reported_population(self):
        fixture = FIXTURES / "planted.json"
        payload = json.loads(fixture.read_text(encoding="utf-8"))
        rows = payload["tasks"]
        expected_rel = relative_scores(payload)
        report = json.loads(run_cli(fixture=fixture))
        self.assertNotEqual(report["suggestions"], [])
        for suggestion in report["suggestions"]:
            matched = matched_by_suggestion(rows, suggestion)
            self.assertEqual(
                len(matched),
                suggestion["n"],
                "reported n must equal the matcher's actual population",
            )
            matched_rel = [
                rel for row, rel in zip(rows, expected_rel) if row in matched
            ]
            self.assertAlmostEqual(
                sum(matched_rel) / len(matched_rel),
                suggestion["mean_relative"],
                places=1,
                msg="reported mean must describe the matcher's population",
            )

    def test_planted_fixture_still_yields_ui_suggestion(self):
        report = json.loads(run_cli(fixture=FIXTURES / "planted.json"))
        ui = [s for s in report["suggestions"] if s["tags"] == ["ui"]]
        self.assertEqual(len(ui), 1)
        self.assertEqual(ui[0]["n"], 100)
        self.assertIn('tags = ["ui"]', ui[0]["toml"])

    def test_no_effect_fixture_yields_no_suggestion(self):
        report = json.loads(run_cli(fixture=FIXTURES / "no-effect.json"))
        self.assertEqual(report["suggestions"], [])
        self.assertEqual(report["unsupported"], [])

    def test_text_report_explains_unrepresentable_leaf(self):
        payload = build_conjunction_export()
        text = run_cli(payload=payload, extra=())
        block_lines = [
            line.strip()
            for line in text.splitlines()
            if line.strip() in ("[[workflow.crew_performance_types]]",)
            or line.strip().startswith(("name = ", "tags = ", "paths = "))
        ]
        self.assertEqual(
            block_lines,
            [],
            "no broader matcher may be labeled as ready-to-paste configuration",
        )
        self.assertIn("tag a and tag b", text)
        self.assertIn("no equivalent ready-to-paste type", text)
        self.assertIn("not a suggestion", text)

    def test_missing_file_reports_one_line_error_without_traceback(self):
        missing = FIXTURES / "does-not-exist-crew-type-discovery.json"
        self.assertFalse(missing.exists(), "regression fixture must stay absent")
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), str(missing)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(proc.stdout, "")
        lines = proc.stderr.splitlines()
        self.assertEqual(len(lines), 1, f"expected one error line, got: {proc.stderr!r}")
        self.assertIn(str(missing), proc.stderr)
        self.assertNotIn("Traceback", proc.stderr)

    def test_malformed_stdin_reports_one_line_error_without_traceback(self):
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "-"],
            input="not json",
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(proc.stdout, "")
        lines = proc.stderr.splitlines()
        self.assertEqual(len(lines), 1, f"expected one error line, got: {proc.stderr!r}")
        self.assertIn("stdin", proc.stderr)
        self.assertNotIn("Traceback", proc.stderr)

    def test_reserved_fallback_name_never_becomes_ready_to_paste_name(self):
        fixture = FIXTURES / "planted.json"
        payload = json.loads(fixture.read_text(encoding="utf-8"))
        for row in payload["tasks"]:
            row["tags"] = ["other" if tag == "ui" else tag for tag in row["tags"]]
        report = json.loads(run_cli(payload=payload))
        other = [s for s in report["suggestions"] if s["tags"] == ["other"]]
        self.assertEqual(len(other), 1, "the expressible `other` tag leaf must still be suggested")
        self.assertEqual(other[0]["n"], 100)
        self.assertNotEqual(other[0]["name"], "other", "`other` is the reserved fallback type name")
        self.assertIn(f'name = "{other[0]["name"]}"', other[0]["toml"])
        for suggestion in report["suggestions"]:
            self.assertNotEqual(suggestion["name"], "other")
        text = run_cli(payload=payload, extra=())
        self.assertNotIn('name = "other"', text, "the text report must not print a reserved name")


if __name__ == "__main__":
    unittest.main()
