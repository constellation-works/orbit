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
CERTIFICATE = "macos-platform-evidence.json"
IDENTITY_PATHS = (
    "scripts/check-ci-macos.sh", "scripts/qa-full-sweep-inventory.json",
    "scripts/test-qa-full-sweep.py", ".github/workflows/ci-macos.yml",
)


def command_evidence(command, tests=(), rows=(), cases=()):
    evidence = {"command": command, "exit_code": 0, "cleanup_verified": True, "stdout": "", "stderr": ""}
    if command[:2] == ["cargo", "test"]:
        tests = list(tests) or ["fixture_case"]
        target = (f"tests/{command[command.index('--test') + 1]}.rs"
                  if "--test" in command else "unittests src/lib.rs")
        binary = command[command.index("--test") + 1].replace("-", "_") if "--test" in command else "fixture"
        evidence["stderr"] = f"     Running {target} (target/debug/deps/{binary}-fixture)\n"
        evidence["stdout"] = (f"running {len(tests)} tests\n" +
            "".join(f"test {name} ... ok\n" for name in tests) +
            "\nsuccesses:\n\n" +
            f"---- {tests[0]} stdout ----\n" +
            "".join(f"qa-scenario-pass: {row}\n" for row in rows) +
            "\nsuccesses:\n" + "".join(f"    {name}\n" for name in tests) +
            f"\ntest result: ok. {len(tests)} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n")
    elif command and command[0] == "node":
        evidence["stdout"] = "".join(f"ok {i} - {name}\n" for i, name in enumerate(cases or ["fixture"], 1))
    return evidence


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
        manifest = yaml.safe_load((ROOT / "docs/development/examples/release-readiness/scripts/release-readiness.yaml").read_text())
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
            "source_state": {"status": []}, "environment": {"os": "Linux"}, "decision": "INCOMPLETE",
            "findings": [f"{cls.native_scenario['id']}: missing, failed, or unrun evidence"],
            "inventory_sha256": hashlib.sha256(inventory_bytes).hexdigest(), "results": [],
        }
        for scenario in inventory["scenarios"]:
            if scenario.get("required"):
                row = {"scenario": scenario["id"], "candidate_id": CANDIDATE,
                       "outcome": "PASS", "assertions": scenario.get("assertions", [])}
                if scenario.get("capability") == "macos":
                    # The sweep records an imported certificate as a claim.
                    row.update(outcome="NOT_RUN", assertions=[], evidence_type="native-certificate-claim",
                               certificate_assertions=scenario["assertions"], native_evidence=cls.native)
                if scenario.get("capability") == "browser":
                    row["retained_evidence"] = {"directory": "/retained", "files": [{"path": "proof.json", "sha256": "0" * 64}]}
                if scenario.get("kind") == "builtin" and scenario["id"] != "inventory-guard":
                    row.update(command_evidence(["orbit", "fixture"]))
                if scenario.get("command") and scenario.get("capability") != "macos":
                    command = scenario["command"]
                    if scenario.get("capability") == "browser":
                        report["browser"] = {"playwright_module": "/playwright"}
                        command = harness["expand_scenario_command"](command, "/playwright", "/retained")
                    row.update(command_evidence(command, scenario.get("required_tests", []),
                                                scenario.get("required_rows", []), scenario.get("required_cases", [])))
                report["results"].append(row)
                for case in scenario.get("retired_cases") or []:
                    if isinstance(case, dict) and case.get("command"):
                        report["results"].append({
                            "scenario": scenario["id"], "candidate_id": CANDIDATE,
                            "outcome": "PASS", "assertions": [], **command_evidence(
                                case["command"], case.get("command_tests", []), case.get("command_rows", []))})
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

    def verdict(self, native, writer="operator", attached=None, row_digest=None):
        """Judge a report whose macOS claim carries `native`.

        `attached` is the certificate stored as a task artifact (by default the
        same bytes) and `writer` the class the owner stamped on it; None means
        an agent attached it, so the owner records no authority."""
        request = copy.deepcopy(self.request)
        stored = json.dumps(native if attached is None else attached, sort_keys=True).encode()
        digest = hashlib.sha256(stored).hexdigest()
        request["evidence"]["artifacts"][CERTIFICATE] = {"sha256": digest, "json": json.loads(stored)}
        request["trusted"]["artifact_authority"] = (
            {CERTIFICATE: {"sha256": digest, "writer": writer}} if writer else {})
        report = request["evidence"]["artifacts"]["qa-full-sweep-report.json"]["json"]
        for row in report["results"]:
            if row["scenario"] == self.native_scenario["id"]:
                row["native_evidence"] = native
                row["native_certificate_sha256"] = row_digest or hashlib.sha256(
                    json.dumps(native, sort_keys=True).encode()).hexdigest()
        result = subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/release-readiness-adapter.py")],
            input=json.dumps(request), text=True, capture_output=True, timeout=30,
            cwd=self.inputs, env={**os.environ, "ORBIT_SCRIPT_IN": str(self.inputs)})
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def judged(self, mutate):
        """Judge the positive QA request after `mutate(request, report)` breaks it."""
        request = copy.deepcopy(self.request)
        digest = hashlib.sha256(json.dumps(self.native, sort_keys=True).encode()).hexdigest()
        request["evidence"]["artifacts"][CERTIFICATE] = {"sha256": digest, "json": self.native}
        request["trusted"]["artifact_authority"] = {CERTIFICATE: {"sha256": digest, "writer": "operator"}}
        report = request["evidence"]["artifacts"]["qa-full-sweep-report.json"]["json"]
        for row in report["results"]:
            if row["scenario"] == self.native_scenario["id"]:
                row["native_evidence"] = self.native
                row["native_certificate_sha256"] = digest
        mutate(request, report)
        result = subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/release-readiness-adapter.py")],
            input=json.dumps(request), text=True, capture_output=True, timeout=30,
            cwd=self.inputs, env={**os.environ, "ORBIT_SCRIPT_IN": str(self.inputs)})
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = json.loads(result.stdout)
        return verdict["status"], verdict["reasons"][0]["code"] if verdict["reasons"] else None

    def test_required_linux_rows_need_trusted_host_or_authenticated_import(self):
        self.assertEqual(self.judged(lambda request, report: None), ("passed", None))
        inventory = json.loads((self.inputs / "qa-full-sweep-inventory.json").read_text())
        scenario = next(s for s in inventory["scenarios"] if s.get("required") and s.get("capability") == "linux")
        linux = scenario["id"]
        inventory_sha = hashlib.sha256((self.inputs / "qa-full-sweep-inventory.json").read_bytes()).hexdigest()
        tests = scenario.get("required_tests") or ["linux_case"]
        target_name = (
            scenario["command"][scenario["command"].index("--test") + 1]
            if "--test" in scenario["command"] else None
        )
        stderr = (
            f"     Running tests/{target_name}/main.rs (target/debug/deps/{target_name.replace('-', '_')}-fixture)\n"
            if target_name else
            "     Running unittests src/lib.rs (target/debug/deps/linux-fixture)\n"
        )
        rows_output = "\n".join(f"qa-scenario-pass: {row}" for row in scenario.get("required_rows", []))
        test_lines = "\n".join(f"test {test} ... ok" for test in tests)
        diagnostic_blocks = "\n\n".join(
            f"---- {test} stdout ----\n{rows_output}" if idx == 0 and rows_output else f"---- {test} stdout ----"
            for idx, test in enumerate(tests)
        )
        success_list = "\n".join(f"    {test}" for test in tests)
        stdout = (
            f"running {len(tests)} test{'s' if len(tests) != 1 else ''}\n"
            f"{test_lines}\n\n"
            f"successes:\n\n"
            f"{diagnostic_blocks}\n\n"
            f"successes:\n"
            f"{success_list}\n\n"
            f"test result: ok. {len(tests)} passed; 0 failed; 0 ignored; 0 measured; "
            f"0 filtered out; finished in 0.00s\n"
        )
        passing_row = {
            "scenario": linux, "candidate_id": CANDIDATE, "outcome": "PASS",
            "command": scenario["command"], "assertions": scenario["assertions"],
            "exit_code": 0, "cleanup_verified": True, "stdout": stdout, "stderr": stderr,
        }

        def qa_report(**changes):
            """A Linux QA report for this candidate whose row passes the Linux scenario."""
            report = {"schema_version": 2, "candidate": {"id": CANDIDATE, "source_revision": REVISION},
                      "source_state": {"status": []}, "environment": {"os": "Linux"},
                      "inventory_sha256": inventory_sha, "results": [dict(passing_row)]}
            report.update(changes)
            return report

        def imported(os_name="Darwin", evidence=None, sha=None, authority="operator", listed_sha=None):
            body = qa_report() if evidence is None else evidence
            digest = hashlib.sha256(json.dumps(body, sort_keys=True).encode()).hexdigest()

            def mutate(request, report):
                report["environment"] = {"os": os_name}
                for row in report["results"]:
                    if row["scenario"] == linux:
                        row.update(imported_from="imported-qa.json", evidence_sha256=sha or digest)
                request["evidence"]["artifacts"]["imported-qa.json"] = {
                    "sha256": listed_sha or digest, "json": body}
                if authority:
                    request["trusted"]["artifact_authority"]["imported-qa.json"] = {
                        "sha256": digest, "writer": authority}
            return mutate

        def native_certificate_digest(request, report):
            """The bypass: a Darwin report cites the owner's macOS certificate, which is
            authenticated but is not Linux QA evidence."""
            digest = hashlib.sha256(json.dumps(self.native, sort_keys=True).encode()).hexdigest()
            report["environment"] = {"os": "Darwin"}
            for row in report["results"]:
                if row["scenario"] == linux:
                    row.update(imported_from="macos-platform-evidence.json", evidence_sha256=digest)

        # A candidate-written non-Linux report with imported evidence passes only
        # when the owner authenticated an exact-digest Linux QA report for this candidate.
        self.assertEqual(self.judged(imported()), ("passed", None))
        self.assertEqual(self.judged(imported("Linux")), ("passed", None))
        # Truthy but unauthenticated strings (the ORB-20165 bypass) are refused.
        cases = {
            "truthy-strings": (lambda request, report: (
                report.update(environment={"os": "Darwin"}),
                [row.update(imported_from="x", evidence_sha256="y")
                 for row in report["results"] if row["scenario"] == linux]),
                "platform-evidence-missing"),
            "bad-digest": (imported(sha="not-a-digest"), "platform-evidence-missing"),
            "no-authority": (imported(authority=None), "platform-evidence-unauthenticated"),
            "agent-writer": (imported(authority="agent"), "platform-evidence-unauthenticated"),
            "digest-not-listed": (imported(listed_sha="0" * 64), "platform-evidence-unauthenticated"),
            "no-authority-block": (lambda request, report: (
                imported()(request, report), request["trusted"].pop("artifact_authority")),
                "platform-evidence-unauthenticated"),
            "non-linux-claim": (lambda request, report: report.update(environment={"os": "Darwin"}),
                                "platform-evidence-missing"),
            "no-environment": (lambda request, report: report.pop("environment"),
                               "platform-evidence-missing"),
            # An authenticated artifact that is not a passing Linux QA report for
            # this candidate earns no Linux row, whatever digest cites it.
            "native-certificate-digest": (native_certificate_digest, "platform-evidence-missing"),
            "no-matching-row": (imported(evidence=qa_report(results=[])), "platform-evidence-missing"),
            "imported-run-on-darwin": (imported(evidence=qa_report(environment={"os": "Darwin"})),
                                       "platform-evidence-missing"),
            "imported-row-failed": (imported(evidence=qa_report(results=[{**passing_row, "outcome": "FAIL"}])),
                                    "platform-evidence-missing"),
            "imported-other-inventory": (imported(evidence=qa_report(inventory_sha256="0" * 64)),
                                         "platform-evidence-missing"),
            # Per-row completeness: every matching row must carry all assertions
            # and satisfy the harness cargo-test and finalize_result contract.
            "unioned-assertions-across-rows": (
                imported(evidence=qa_report(results=[
                    {**passing_row, "assertions": scenario["assertions"][:-1]},
                    {**passing_row, "assertions": scenario["assertions"][-1:]},
                ])),
                "platform-evidence-missing"),
            "second-row-incomplete": (
                imported(evidence=qa_report(results=[
                    dict(passing_row),
                    {"scenario": linux, "candidate_id": CANDIDATE, "outcome": "PASS",
                     "command": scenario["command"], "assertions": scenario["assertions"]},
                ])),
                "platform-evidence-missing"),
            "missing-execution-evidence": (
                imported(evidence=qa_report(results=[{
                    "scenario": linux, "candidate_id": CANDIDATE, "outcome": "PASS",
                    "command": scenario["command"], "assertions": scenario["assertions"],
                }])),
                "platform-evidence-missing"),
            "missing-required-test": (
                imported(evidence=qa_report(results=[{
                    **passing_row,
                    "stdout": stdout.replace(tests[0], "unrelated_case"),
                }])),
                "platform-evidence-missing"),
            "missing-required-row": (
                imported(evidence=qa_report(results=[{
                    **passing_row,
                    "stdout": stdout.replace(f"qa-scenario-pass: {scenario['required_rows'][-1]}\n", ""),
                }])),
                "platform-evidence-missing"),
            "wrong-test-target": (
                imported(evidence=qa_report(results=[{
                    **passing_row,
                    "stderr": "     Running tests/other.rs (target/debug/deps/other-fixture)\n",
                }])),
                "platform-evidence-missing"),
            "cleanup-unverified": (
                imported(evidence=qa_report(results=[{**passing_row, "cleanup_verified": False}])),
                "platform-evidence-missing"),
            "output-truncated": (
                imported(evidence=qa_report(results=[{**passing_row, "output_truncated": True}])),
                "platform-evidence-missing"),
            "skip-notice": (
                imported(evidence=qa_report(results=[{
                    **passing_row,
                    "stderr": stderr + "SKIP: linux capability missing\n",
                }])),
                "platform-evidence-missing"),
            "boolean-exit-code": (
                imported(evidence=qa_report(results=[{**passing_row, "exit_code": False}])),
                "platform-evidence-missing"),
            "non-list-assertions": (
                imported(evidence=qa_report(results=[{**passing_row, "assertions": None}])),
                "platform-evidence-missing"),
        }
        for name, (mutate, code) in cases.items():
            with self.subTest(name=name):
                self.assertEqual(self.judged(mutate), ("unavailable", code))

    def test_local_command_rows_require_actual_execution_evidence(self):
        inventory = json.loads((self.inputs / "qa-full-sweep-inventory.json").read_text())
        scenarios = [next(s for s in inventory["scenarios"] if s.get("required") and s.get("capability") == cap
                          and s.get("kind") != "builtin") for cap in ("local", "linux", "browser")]
        scenarios.append(next(s for s in inventory["scenarios"] if s.get("retired_cases") and s.get("required")))
        for scenario in scenarios:
            for field, value in (("command", None), ("exit_code", None), ("exit_code", 1),
                                 ("cleanup_verified", False), ("stdout", "")):
                with self.subTest(scenario=scenario["id"], field=field, value=value):
                    def mutate(_request, report):
                        rows = [r for r in report["results"] if r["scenario"] == scenario["id"]]
                        row = rows[-1] if scenario.get("retired_cases") else rows[0]
                        row[field] = value
                    # A non-test command may have an empty stdout legitimately.
                    if field == "stdout" and not (scenario.get("kind") == "cargo-test" or scenario.get("capability") == "browser"):
                        continue
                    self.assertEqual(self.judged(mutate)[0], "unavailable")

    def test_builtin_independent_rows_and_expected_refusal(self):
        name = "workspace-boundary"
        def set_refusal(_request, report):
            row = next(r for r in report["results"] if r["scenario"] == name)
            row["assertions"].remove("cross-workspace-task-read-refused")
            report["results"].append({"scenario": name, "candidate_id": CANDIDATE,
                "outcome": "PASS", "assertions": ["cross-workspace-task-read-refused"],
                "command": ["orbit", "task", "show"], "exit_code": 3, "cleanup_verified": True,
                "stdout": "", "stderr": "refused"})
        self.assertEqual(self.judged(set_refusal), ("passed", None))
        for field, value in (("command", []), ("exit_code", 0), ("exit_code", None),
                             ("cleanup_verified", False)):
            with self.subTest(field=field, value=value):
                def mutate(request, report):
                    set_refusal(request, report)
                    next(r for r in reversed(report["results"]) if r["scenario"] == name)[field] = value
                self.assertEqual(self.judged(mutate), ("unavailable", "execution-evidence-invalid"))
        def missing_independent_row(request, report):
            set_refusal(request, report)
            report["results"].append({"scenario": name, "candidate_id": CANDIDATE,
                                      "outcome": "PASS", "assertions": []})
        self.assertEqual(self.judged(missing_independent_row), ("unavailable", "execution-evidence-invalid"))

    def test_incomplete_decision_needs_an_accounting_finding(self):
        def drop_findings(request, report):
            report["findings"] = []

        def no_findings(request, report):
            del report["findings"]
        cases = {
            "incomplete-empty-findings": (drop_findings, "report-incomplete"),
            "incomplete-missing-findings": (no_findings, "report-incomplete"),
            "dict-findings": (lambda request, report: report.update(findings={"a": 1}), "report-malformed"),
            "string-findings": (lambda request, report: report.update(findings="oops"), "report-malformed"),
            "unaccounted-finding": (lambda request, report: report["findings"].append("surprise"),
                                    "report-incomplete"),
        }
        for name, (mutate, code) in cases.items():
            with self.subTest(name=name):
                self.assertEqual(self.judged(mutate), ("unavailable", code))

    def test_source_state_status_must_be_the_exact_clean_value(self):
        def state(value):
            return lambda request, report: report["source_state"].__setitem__("status", value)

        def missing(request, report):
            report["source_state"] = {}
        for name, mutate in {
            "missing-status": missing, "null-status": state(None), "empty-string": state(""),
            "dirty-status": state([" M scripts/file"]), "false-status": state(False),
        }.items():
            with self.subTest(name=name):
                self.assertEqual(self.judged(mutate), ("unavailable", "candidate-dirty"))

    def test_fabricated_certificate_needs_owner_artifact_authority(self):
        # A well-formed certificate with invented producer fields, invented
        # PASS transcripts and copied source hashes proves only its content.
        for writer in ("operator", "system"):
            with self.subTest(writer=writer):
                self.assertEqual(self.verdict(self.native, writer)["status"], "passed")
        other = copy.deepcopy(self.native)
        other["producer"]["run_id"] = "456"
        cases = {
            # Agent-attached: the owner stamps no writer class.
            "agent-attached": dict(writer=None),
            "unknown-writer": dict(writer="agent"),
            # The authenticated artifact is another certificate than the row's.
            "different-certificate": dict(attached=other),
            "row-digest-not-authenticated": dict(row_digest="0" * 64),
            "row-digest-missing": dict(row_digest="not-a-digest"),
        }
        for name, options in cases.items():
            with self.subTest(name=name):
                verdict = self.verdict(self.native, **options)
                self.assertEqual(verdict["status"], "unavailable", verdict)
                self.assertEqual(verdict["reasons"][0]["code"], "platform-evidence-unauthenticated", verdict)

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
        # scenario_decision mutates the row list it is given. Assert that
        # copy: the browser row's own failure is the retained-evidence gap,
        # not any other failure the fixture also carries.
        producer_results = _copy.deepcopy(results)
        passed, failures = harness["scenario_decision"](
            _copy.deepcopy(inventory), producer_results, CANDIDATE)
        self.assertFalse(passed, failures)
        browser = next(row for row in producer_results if row["scenario"] == "browser-x")
        self.assertEqual(browser.get("outcome"), "FAIL", browser)
        self.assertIn("retained", browser.get("retained_evidence_error", ""), browser)
        self.assertTrue(any(item.startswith("browser-x:") for item in failures), failures)

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
        # The adapter judges the original PASS rows, which omit retained evidence.
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
        # browser-x is the first required scenario, so a missing retained
        # manifest is the verdict. Accepting the retired-case code here would
        # stay green if the browser check were removed.
        self.assertEqual(verdict["reasons"][0]["code"], "retained-evidence-missing", verdict)

        # An empty retained manifest is not retained evidence: the producer
        # rejects it (verify_retained_evidence), so the adapter must reject it
        # on the same browser shape rather than clear the gate.
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
        self.assertEqual(verdict["reasons"][0]["code"], "retained-evidence-missing", verdict)

        # A non-empty manifest clears the browser shape, so the retired shape
        # is still the verdict.
        for row in report["results"]:
            if row["scenario"] == "browser-x":
                row["retained_evidence"] = {"directory": "/retained",
                                            "files": [{"path": "browser.png", "sha256": "0" * 64}]}
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

    def test_breaking_classification_must_be_the_exact_boolean(self):
        for name, value in (("missing", ...), ("null", None), ("zero", 0), ("string", "false")):
            with self.subTest(name=name):
                request = self.request()
                survey = request["evidence"]["artifacts"]["release-prep-evidence.json"]["json"]["survey"]
                if value is ...:
                    del survey["breaking"]
                else:
                    survey["breaking"] = value
                verdict = self.verdict(request)
                self.assertEqual(verdict["status"], "unavailable", verdict)
                self.assertEqual(verdict["reasons"][0]["code"], "survey-mismatch", verdict)
        request = self.request(candidate={"breaking": True})
        request["trusted"]["release_survey"]["breaking"] = True
        request["evidence"]["artifacts"]["release-prep-evidence.json"]["json"]["survey"]["breaking"] = 1
        self.assertEqual(self.verdict(request)["reasons"][0]["code"], "survey-mismatch")

    def test_canonical_task_is_chosen_by_numeric_id(self):
        # ORB-100000 sorts before ORB-99999 as a string, but is the later task.
        def two_open(named):
            request = self.request()
            first = request["tasks"]["tagged"][0]
            request["tasks"]["tagged"] = [{**first, "id": "ORB-100000"}, {**first, "id": "ORB-99999"}]
            request["evidence"]["artifacts"]["release-prep-evidence.json"]["json"][
                "canonical_task_id"] = named
            return request
        self.assertEqual(self.verdict(two_open("ORB-99999"))["status"], "passed")
        verdict = self.verdict(two_open("ORB-100000"))
        self.assertEqual(verdict["status"], "unavailable", verdict)
        self.assertEqual(verdict["reasons"][0]["code"], "handoff-mismatch", verdict)

    def test_busy_board_and_empty_range_probes_stay_blocked(self):
        for scenario, code in (("blocked", "prep-board-busy"),
                               ("no_release", "prep-nothing-to-release")):
            with self.subTest(scenario=scenario):
                verdict = self.verdict(self.request(scenario=scenario, trusted_survey=None))
                self.assertEqual(verdict["status"], "blocked", verdict)
                self.assertEqual(verdict["reasons"][0]["code"], code, verdict)


if __name__ == "__main__":
    unittest.main()
