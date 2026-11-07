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
        # Mirror the manifest's input tree, without access to the checkout cwd.
        manifest = yaml.safe_load((ROOT / "examples/release-readiness/scripts/release-readiness.yaml").read_text())
        for entry in manifest["spec"]["files"]["inputs"]:
            target = cls.inputs / entry["as"]
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / entry["from"], target)
        inventory_bytes = (cls.inputs / "scripts/qa-full-sweep-inventory.json").read_bytes()
        inventory = json.loads(inventory_bytes)
        cls.native_scenario = next(s for s in inventory["scenarios"] if s.get("capability") == "macos")
        harness = runpy.run_path(str(ROOT / "scripts/test-qa-full-sweep.py"))
        execution = []
        for command in harness["native_required_commands"](cls.inputs):
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
            "source_identity": {p: hashlib.sha256((cls.inputs / p).read_bytes()).hexdigest()
                                for p in IDENTITY_PATHS},
            "execution": execution,
        }
        # The positive fixture meets the existing native contract independently.
        harness["validate_hosted_macos_evidence"](
            copy.deepcopy(cls.native), cls.native_scenario,
            {"head": REVISION, "status": []}, cls.inputs)
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
                report["results"].append(row)
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


if __name__ == "__main__":
    unittest.main()
