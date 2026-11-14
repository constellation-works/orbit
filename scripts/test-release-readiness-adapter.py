#!/usr/bin/env python3
"""Exercise security findings and native proof through the readiness executable."""

import copy
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parent.parent
REVISION = "b" * 40
CANDIDATE = "native-proof-candidate"
IDENTITY_PATHS = (
    "scripts/check-ci-macos.sh", "scripts/qa-full-sweep-inventory.json",
    "scripts/test-qa-full-sweep.py", ".github/workflows/ci-macos.yml",
)


class SecurityReadinessRegression(unittest.TestCase):
    def test_deduplicated_findings_preserve_provenance_and_blockers(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        existing = {
            "id": "existing-finding", "status": "done", "priority": "high",
            "relations": [{"type": "spawned_from", "target": "earlier-review"}],
        }
        new = {**existing, "id": "new-finding",
               "relations": [{"type": "spawned_from", "target": "current-review"}]}
        duplicate = {"task_id": existing["id"], "severity": "high", "disposition": "duplicate"}
        filed = {"task_id": new["id"], "severity": "high"}
        cases = [
            ("resolved-duplicate", [duplicate], [], [existing], "passed", None),
            ("clean-review", [], [], [], "passed", None),
            ("new-finding", [filed], [new], [new], "passed", None),
            ("mixed-findings", [duplicate, filed], [new], [existing, new], "passed", None),
            ("missing-reference", [duplicate], [], [], "unavailable", "finding-unresolved"),
            ("missing-task", [duplicate], [], [{"id": existing["id"], "status": "missing"}],
             "unavailable", "finding-unresolved"),
            ("omitted-new-finding", [duplicate], [new], [existing], "unavailable", "finding-omitted"),
            ("mislinked-new-finding", [{**duplicate, "disposition": "new"}], [], [existing],
             "unavailable", "finding-unlinked"),
            ("implicit-new-finding", [{"task_id": existing["id"], "severity": "high"}], [], [existing],
             "unavailable", "finding-unlinked"),
            ("mislinked-spawn", [{"task_id": existing["id"], "severity": "high"}], [existing], [existing],
             "unavailable", "finding-unlinked"),
            ("unknown-disposition", [{**duplicate, "disposition": "resolved"}], [], [existing],
             "unavailable", "finding-malformed"),
            ("new-disguised-as-duplicate", [{**filed, "disposition": "duplicate"}], [new], [new],
             "unavailable", "finding-not-duplicate"),
            ("mislinked-spawn-disguised-as-duplicate", [duplicate], [existing], [existing],
             "unavailable", "finding-not-duplicate"),
        ]
        # Either the reported severity or the owner's priority must hold open
        # release blockers, even when the other source downplays the finding.
        for disposition, finding, task, spawned in (
            ("duplicate", duplicate, existing, []), ("new", filed, new, [new]),
        ):
            for severity in ("high", "critical"):
                for source in ("finding", "task"):
                    open_task = {**task, "status": "in-progress",
                                 "priority": severity if source == "task" else "low"}
                    reported = {**finding, "severity": severity if source == "finding" else "low"}
                    cases.append((f"open-{disposition}-{source}-{severity}", [reported],
                                  [open_task] if spawned else [], [open_task],
                                  "blocked", "unresolved-findings"))
        for name, findings, spawned, referenced, status, code in cases:
            with self.subTest(name=name):
                request = {
                    "contract": "release-readiness/v1", "form": "security-review",
                    "trusted": {"source_revision": REVISION, "task_id": "current-review"},
                    "tasks": {"spawned": spawned, "referenced": referenced},
                    "evidence": {"artifacts": {
                        "proof.json": {"json": {}},
                        "security-review-evidence.json": {"json": {
                            "source_revision": REVISION,
                            "scope": [{"area": area, "summary": "Reviewed with evidence",
                                       "evidence": ["proof.json"]}
                                      for area in ("application-code", "dependencies",
                                                   "secret-handling", "configuration")],
                            "findings": findings,
                        }},
                    }},
                }
                result = subprocess.run(
                    [sys.executable, "-B", str(ROOT / "scripts/release-readiness-adapter.py")],
                    input=json.dumps(request), text=True, capture_output=True, timeout=30,
                    cwd=scratch)
                self.assertEqual(result.returncode, 0, result.stderr)
                verdict = json.loads(result.stdout)
                self.assertEqual(verdict["status"], status, verdict)
                self.assertEqual(verdict["reasons"][0]["code"] if verdict["reasons"] else None,
                                 code, verdict)


class NativeReadinessRegression(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        cls.temp = tempfile.TemporaryDirectory(dir=scratch, prefix="readiness-native-")
        cls.addClassCleanup(cls.temp.cleanup)
        cls.inputs = Path(cls.temp.name)
        # Mirror the sandbox's flat input mounts, without the checkout cwd.
        manifest = yaml.safe_load((ROOT / "examples/release-readiness/scripts/release-readiness.yaml").read_text())
        for entry in manifest["spec"]["files"]["inputs"]:
            target = cls.inputs / entry["as"]
            shutil.copyfile(ROOT / entry["from"], target)
        inventory_bytes = (cls.inputs / "qa-full-sweep-inventory.json").read_bytes()
        inventory = json.loads(inventory_bytes)
        cls.native_scenario = next(s for s in inventory["scenarios"] if s.get("capability") == "macos")
        harness = runpy.run_path(str(ROOT / "scripts/test-qa-full-sweep.py"))
        execution = []
        for command in harness["native_required_commands"](ROOT):
            target = "unittests src/lib.rs (target/debug/deps/native-fixture)"
            if "--test" in command:
                name = command[command.index("--test") + 1]
                target = f"tests/{name}.rs (target/debug/deps/{name.replace('-', '_')}-fixture)"
            execution.append({
                "command": command,
                "executed_command": harness["native_execution_command"](command),
                "source_revision": REVISION, "source_status": [], "source_stable": True,
                "exit_code": 0, "cleanup_verified": True,
                "stdout": "running 1 test\ntest native_boundary ... ok\n"
                          "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                          "0 filtered out; finished in 0.00s\n",
                "stderr": f"     Running {target}\n",
            })
        cls.native = {
            "schema_version": 1, "evidence_type": "orbit-macos-platform", "platform": "macos",
            "source_revision": REVISION, "command": cls.native_scenario["command"],
            "assertions": cls.native_scenario["assertions"], "outcome": "PASS",
            "producer": {"system": "github-actions", "repository": "owner/orbit", "run_id": "123",
                         "run_attempt": "1",
                         "workflow_ref": "owner/orbit/.github/workflows/ci-macos.yml@refs/heads/agent-main",
                         "workflow_sha": REVISION},
            "source_identity": {p: hashlib.sha256((cls.inputs / Path(p).name).read_bytes()).hexdigest()
                                for p in IDENTITY_PATHS},
            "execution": execution,
        }
        # The positive fixture meets the existing native contract independently.
        harness["validate_hosted_macos_evidence"](
            copy.deepcopy(cls.native), cls.native_scenario,
            {"head": REVISION, "status": []}, ROOT)
        report = {
            "schema_version": 2, "candidate": {"id": CANDIDATE, "source_revision": REVISION},
            "source_state": {"status": []}, "environment": {"os": "Linux"}, "decision": "PASS",
            "inventory_sha256": hashlib.sha256(inventory_bytes).hexdigest(), "results": [],
        }
        for scenario in inventory["scenarios"]:
            if scenario.get("required"):
                row = {"scenario": scenario["id"], "candidate_id": CANDIDATE,
                       "outcome": "PASS", "assertions": scenario.get("assertions", [])}
                if scenario.get("capability") == "macos":
                    row["native_evidence"] = cls.native
                if scenario.get("capability") == "browser":
                    row["retained_evidence"] = {"directory": "/retained", "files": [{"path": "proof.json", "sha256": "0" * 64}]}
                if scenario.get("command"):
                    row["command"] = scenario["command"]
                report["results"].append(row)
                for case in scenario.get("retired_cases") or []:
                    if isinstance(case, dict) and case.get("command"):
                        report["results"].append({
                            "scenario": scenario["id"], "candidate_id": CANDIDATE,
                            "outcome": "PASS", "assertions": [], "command": case["command"]})
        artifacts = {"qa-full-sweep-report.json": {"json": report}, "proof.json": {"json": {}}}
        artifacts["forecast-history-qa.json"] = {"json": {
            "candidate_id": CANDIDATE, "source_revision": REVISION,
            "scenarios": [{"id": name, "outcome": "PASS", "evidence": ["proof.json"]}
                          for name in ("legacy-history", "forecast-contract", "rendered-behavior",
                                       "historical-evidence")],
        }}
        artifacts["capability-gap-evidence.json"] = {"json": {
            "candidate_id": CANDIDATE, "source_revision": REVISION,
            "surfaces": {name: {"outcome": "PASS", "evidence": ["proof.json"]}
                         for name in inventory.get("required_capability_gaps", {})},
        }}
        cls.request = {"contract": "release-readiness/v1", "form": "qa-full-sweep",
                       "trusted": {"source_revision": REVISION, "task_id": "fixture"},
                       "tasks": {}, "evidence": {"artifacts": artifacts}}

    def verdict(self, native):
        request = copy.deepcopy(self.request)
        report = request["evidence"]["artifacts"]["qa-full-sweep-report.json"]["json"]
        for row in report["results"]:
            if row["scenario"] == self.native_scenario["id"]:
                row["native_evidence"] = native
        result = subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/release-readiness-adapter.py")],
            input=json.dumps(request), text=True, capture_output=True, timeout=30,
            cwd=self.inputs, env={**os.environ, "ORBIT_SCRIPT_IN": str(self.inputs)})
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_native_certificate_required_for_readiness(self):
        self.assertEqual(self.verdict(self.native)["status"], "passed")
        cases = {
            "missing-native-proof": {"source_revision": REVISION, "platform": "macos"},
            "stale-failed-native-execution": {
                "source_revision": REVISION, "platform": "macos",
                "execution": [{"source_revision": "a" * 40, "exit_code": 1}]},
        }
        for field, value in (
            ("platform", "linux"), ("evidence_type", "linux"), ("source_revision", "a" * 40),
            ("command", ["true"]), ("outcome", "FAIL"), ("assertions", []),
            ("producer", {}), ("source_identity", {}), ("execution", []),
        ):
            cases[field] = {**self.native, field: value}
        for field, value in (
            ("source_revision", "a" * 40), ("exit_code", 1), ("cleanup_verified", False),
            ("source_status", [" M changed"]), ("source_stable", False), ("executed_command", []),
            ("stdout", "SKIP: native execution unavailable\n"),
            ("output_truncated", True),
        ):
            native = copy.deepcopy(self.native)
            native["execution"][0][field] = value
            cases[f"execution-{field}"] = native
        cases["execution-incomplete"] = {**self.native, "execution": self.native["execution"][:-1]}
        cases["execution-wrong-command"] = copy.deepcopy(self.native)
        cases["execution-wrong-command"]["execution"][0]["command"] = ["cargo", "test", "wrong"]
        cases["producer-wrong-workflow"] = {
            **self.native, "producer": {**self.native["producer"], "workflow_ref": "other.yml@main"}}
        for name, native in cases.items():
            with self.subTest(name=name):
                verdict = self.verdict(native)
                self.assertEqual(verdict["status"], "unavailable", verdict)


    def test_browser_and_retired_parity_with_producer(self):
        # Both probes must fail in the producer (scenario_decision) and in
        # this adapter on the same shapes, not only in one gate.
        harness = runpy.run_path(str(ROOT / "scripts/test-qa-full-sweep.py"))
        inventory = {"scenarios": [
            {"id": "browser-x", "required": True, "capability": "browser",
             "assertions": ["a"]},
            {"id": "retired-y", "required": True, "capability": "local",
             "assertions": ["b"],
             "retired_cases": [{"retired": "old::case", "command": ["cargo", "test", "x"]}]},
        ], "required_capability_gaps": {}}
        results = [
            {"scenario": "browser-x", "outcome": "PASS", "candidate_id": CANDIDATE,
             "assertions": ["a"]},
            {"scenario": "retired-y", "outcome": "PASS", "candidate_id": CANDIDATE,
             "assertions": ["b"]},
        ]
        import copy as _copy
        passed, failures = harness["scenario_decision"](
            _copy.deepcopy(inventory), _copy.deepcopy(results), CANDIDATE)
        self.assertFalse(passed, failures)
        self.assertTrue(any("retained" in row.get("retained_evidence_error", "") or
                            row.get("outcome") == "FAIL" for row in results) or failures, failures)

        # Adapter verdict on the same inventory/report shapes.
        inputs = Path(tempfile.mkdtemp(dir=ROOT / ".orbit/tmp", prefix="readiness-parity-"))
        self.addCleanup(shutil.rmtree, inputs, True)
        inventory_bytes = json.dumps(inventory).encode()
        (inputs / "qa-full-sweep-inventory.json").write_bytes(inventory_bytes)
        report = {
            "schema_version": 2, "candidate": {"id": CANDIDATE, "source_revision": REVISION},
            "source_state": {"status": []}, "environment": {"os": "Linux"}, "decision": "PASS",
            "inventory_sha256": hashlib.sha256(inventory_bytes).hexdigest(),
            "results": _copy.deepcopy(results),
        }
        # Producer mutates browser rows to FAIL; restore PASS shapes for adapter.
        for row in report["results"]:
            row["outcome"] = "PASS"
        request = {"contract": "release-readiness/v1", "form": "qa-full-sweep",
                   "trusted": {"source_revision": REVISION, "task_id": "fixture"},
                   "tasks": {}, "evidence": {"artifacts": {
                       "qa-full-sweep-report.json": {"json": report},
                       "proof.json": {"json": {}},
                       "forecast-history-qa.json": {"json": {
                           "candidate_id": CANDIDATE, "source_revision": REVISION,
                           "scenarios": [{"id": name, "outcome": "PASS", "evidence": ["proof.json"]}
                                         for name in ("legacy-history", "forecast-contract",
                                                      "rendered-behavior", "historical-evidence")]}},
                   }}}
        result = subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/release-readiness-adapter.py")],
            input=json.dumps(request), text=True, capture_output=True, timeout=30,
            cwd=inputs, env={**os.environ, "ORBIT_SCRIPT_IN": str(inputs)})
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = json.loads(result.stdout)
        self.assertEqual(verdict["status"], "unavailable", verdict)
        self.assertIn(verdict["reasons"][0]["code"],
                      ("retained-evidence-missing", "retired-case-not-executed"), verdict)

        # Fixing only the browser shape still fails on the retired shape.
        for row in report["results"]:
            if row["scenario"] == "browser-x":
                row["retained_evidence"] = {"directory": "/retained", "files": []}
        request["evidence"]["artifacts"]["qa-full-sweep-report.json"]["json"] = report
        result = subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/release-readiness-adapter.py")],
            input=json.dumps(request), text=True, capture_output=True, timeout=30,
            cwd=inputs, env={**os.environ, "ORBIT_SCRIPT_IN": str(inputs)})
        verdict = json.loads(result.stdout)
        self.assertEqual(verdict["status"], "unavailable", verdict)
        self.assertEqual(verdict["reasons"][0]["code"], "retired-case-not-executed", verdict)


class PrepReadinessRegression(unittest.TestCase):
    """A handoff passes only against the owner's pinned survey, never merely
    by agreeing with the (also agent-authored) canonical task."""

    HEAD = "d" * 40
    COMMITS = ["b" * 40, "c" * 40]

    def request(self, *, candidate=None, trusted_survey="real", canonical_version=None,
                scenario="handoff"):
        survey = {"latest_tag": "v0.1.0", "head": self.HEAD,
                  "commits": list(self.COMMITS), "breaking": False}
        if candidate:
            survey.update(candidate)
        version = canonical_version or {
            ("v0.1.0", False): "0.1.1", ("v0.1.0", True): "0.2.0",
        }.get((survey["latest_tag"], survey["breaking"] is True), "0.0.1")
        trusted = {"source_revision": self.HEAD, "task_id": "probe"}
        if trusted_survey == "real":
            trusted["release_survey"] = {"latest_tag": "v0.1.0", "head": self.HEAD,
                                         "commits": list(self.COMMITS), "breaking": False}
        elif trusted_survey is not None:
            trusted["release_survey"] = trusted_survey
        body = {"source_revision": self.HEAD, "scenario": scenario, "blockers": [],
                "survey": survey, "version": version, "canonical_task_id": "ORB-1"}
        if scenario == "blocked":
            body["blockers"] = ["ORB-9"]
        return {
            "contract": "release-readiness/v1", "form": "release-prep",
            "trusted": trusted,
            "tasks": {"tagged": [{
                "id": "ORB-1", "title": f"Prepare v{version} release", "status": "backlog",
                "tags": ["release", "no-diff-expected"],
                "context_files": ["file:CHANGELOG.md", "file:Cargo.toml",
                                  "file:Cargo.lock", "file:npm/package.json"],
                "description": f"pinned {self.HEAD} version {version}",
            }]},
            "evidence": {"artifacts": {"release-prep-evidence.json": {"json": body}}},
        }

    def verdict(self, request):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        result = subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/release-readiness-adapter.py")],
            input=json.dumps(request), text=True, capture_output=True, timeout=30,
            cwd=scratch)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_valid_handoff_passes(self):
        self.assertEqual(self.verdict(self.request())["status"], "passed")
        breaking = self.request(candidate={"breaking": True})
        breaking["trusted"]["release_survey"]["breaking"] = True
        self.assertEqual(self.verdict(breaking)["status"], "passed")

    def test_invented_and_mismatched_surveys_are_unavailable(self):
        cases = {
            # The ORB-22658 fixture: an invented commit, tag and version that
            # agree only with the canonical task.
            "invented-survey": self.request(
                candidate={"latest_tag": "v0.0.0", "commits": ["not-a-commit"]}),
            "no-trusted-survey": self.request(trusted_survey=None),
            "mismatched-tag": self.request(candidate={"latest_tag": "v0.0.9"}),
            "mismatched-range": self.request(candidate={"commits": ["e" * 40]}),
            "mismatched-breaking": self.request(candidate={"breaking": True}),
            "mismatched-version": self.request(canonical_version="9.9.9"),
            "empty-trusted-range": self.request(trusted_survey={
                "latest_tag": "v0.1.0", "head": self.HEAD, "commits": [], "breaking": False}),
            "malformed-trusted-range": self.request(trusted_survey={
                "latest_tag": "v0.1.0", "head": self.HEAD,
                "commits": ["not-a-commit"], "breaking": False}),
        }
        for name, request in cases.items():
            with self.subTest(name=name):
                verdict = self.verdict(request)
                self.assertEqual(verdict["status"], "unavailable", verdict)

    def test_busy_board_and_empty_range_probes_stay_blocked(self):
        for scenario, code in (("blocked", "prep-board-busy"),
                               ("no_release", "prep-nothing-to-release")):
            with self.subTest(scenario=scenario):
                verdict = self.verdict(self.request(scenario=scenario, trusted_survey=None))
                self.assertEqual(verdict["status"], "blocked", verdict)
                self.assertEqual(verdict["reasons"][0]["code"], code, verdict)


if __name__ == "__main__":
    unittest.main()
