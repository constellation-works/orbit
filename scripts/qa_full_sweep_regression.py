"""Command-boundary regressions for selection, execution and retained QA bytes."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


# Substitute the repository/capability adapters, then invoke the actual public
# sweep entrypoint. Command execution, classification, import, retention and
# final sign-off all run in the harness under test.
DRIVER = '''
import importlib.util, json, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("qa", sys.argv[1])
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
repo = Path(sys.argv[2])
qa.__file__ = str(repo / "scripts/test-qa-full-sweep.py")
qa.validate_inventory = lambda *_: ([], {"cli": []})
qa.candidate_source = lambda *_: {"candidate_id": "fixture-candidate", "head": "a" * 40,
                                "status": [], "diff_sha256": "fixture"}
if json.loads((repo / "mode.json").read_text()) != "builtin-skip":
    qa.run_builtins = lambda *_: ([], None)
qa.platform.system = lambda: "Darwin"
qa.inspect_browser_capability = lambda *_: ({"command": ["probe"], "exit_code": 0,
                                            "stdout": "fixture browser", "stderr": ""}, "fixture browser")
sys.argv = [sys.argv[1], "--repo-root", str(repo), "--orbit-bin", str(repo / "bin/orbit"),
            "--output", str(repo / ".orbit/tmp/report.json"), *sys.argv[3:]]
qa.main()
'''

CARGO = '''
import json, sys
from pathlib import Path
mode = json.loads(Path("mode.json").read_text())
if "metadata" in sys.argv:
    print(json.dumps({"packages": [{"name": "fixture", "targets": [{"name": "boundary", "kind": ["test"]}]}]}))
elif "--list" in sys.argv:
    print(("unrelated" if mode == "stale" else "boundary::contracts") + ": test")
else:
    print("test boundary::contracts ... ok")
    print("test result: ok. 1 passed; 0 failed; 0 ignored;")
    if mode != "missing-row":
        print("qa-scenario-pass: original_boundary", file=sys.stderr)
    if mode == "seatbelt":
        print("SKIP: sandbox-exec cannot apply a profile: Operation not permitted", file=sys.stderr)
    if mode == "ps":
        print("DEFERRED: ps denied: Operation not permitted", file=sys.stderr)
'''

BROWSER = '''
import json, sys
from pathlib import Path
mode = json.loads(Path("mode.json").read_text())
evidence = Path(sys.argv[2])
evidence.mkdir(parents=True, exist_ok=True)
(evidence / "result.json").write_text(json.dumps({"scenario": sys.argv[1]}))
if sys.argv[1] == "second" and mode in ["overwrite", "missing"]:
    for path in evidence.parent.rglob("harness-result.json"):
        if mode == "missing":
            path.unlink()
        else:
            path.write_text("overwritten by a later producer")
print(json.dumps({"scenario": sys.argv[1]}))
if mode == "seatbelt":
    print("SKIP: sandbox-exec cannot apply a profile: browser launch refused", file=sys.stderr)
'''


def self_test():
    harness = Path(os.environ.get("QA_SWEEP_HARNESS_UNDER_TEST", Path(__file__).with_name("test-qa-full-sweep.py"))).resolve()
    scratch = Path(__file__).resolve().parent.parent / ".orbit/tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="qa-evidence-regression-", dir=scratch) as tmp:
        repo = Path(tmp)
        for directory in ["bin", "scripts", ".orbit/tmp", "browsers"]:
            (repo / directory).mkdir(parents=True, exist_ok=True)
        driver = repo / "driver.py"
        driver.write_text(DRIVER)
        cargo = repo / "bin/cargo"
        cargo.write_text(f"#!{sys.executable}\n" + CARGO)
        cargo.chmod(0o755)
        orbit = repo / "bin/orbit"
        orbit.write_text(f"#!{sys.executable}\nimport sys\nif 'init' in sys.argv:\n    print('SKIP: sandbox-exec cannot apply a profile: init refused', file=sys.stderr)\n    raise SystemExit(1)\nprint('fixture orbit')\n")
        orbit.chmod(0o755)
        npm = repo / "bin/npm"
        npm.write_text(f"#!{sys.executable}\nimport sys\nprint('prepared')\nif 'ci' in sys.argv:\n    print('SKIP: sandbox-exec cannot apply a profile: preparation refused', file=sys.stderr)\n")
        npm.chmod(0o755)
        (repo / "website").mkdir()
        (repo / "website/package.json").write_text('{}')
        (repo / "CHANGELOG.md").write_text('fixture changelog')
        browser = repo / "browser.py"
        browser.write_text(BROWSER)
        git = repo / "bin/git"
        git.write_text(f"#!{sys.executable}\nprint('" + "a" * 40 + "')\n")
        git.chmod(0o755)
        (repo / ".github/workflows").mkdir(parents=True)
        (repo / "scripts/test-qa-full-sweep.py").write_bytes(harness.read_bytes())
        module = repo / "module.js"
        module.write_text("fixture browser module")
        listing = repo / "scripts/check-ci-macos.sh"
        listing.write_text("#!/bin/sh\nprintf 'check-ci-macos: matched 2 tests\\n'\n")
        listing.chmod(0o755)
        command = ["cargo", "test", "-p", "fixture", "--test", "boundary", "boundary::", "--", "--show-output"]
        rust = {"id": "rust", "required": True, "capability": "local", "kind": "cargo-test",
                "command": command, "assertions": ["original-boundary"],
                "required_tests": ["boundary::contracts"], "required_rows": ["original_boundary"]}
        native = {"id": "macos-platform", "required": True, "capability": "macos", "kind": "command",
                  "command": ["./scripts/check-ci-macos.sh"],
                  "assertions": ["macos-required-workflow-is-current", "macos-check-ran-on-matching-revision"]}
        browsers = [{"id": name, "required": True, "capability": "browser", "kind": "command",
                     "command": [sys.executable, str(browser), name, "{evidence_dir}"], "assertions": [name]}
                    for name in ["first", "second"]]

        def sweep(scenarios, mode, extra=()):
            (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps({"scenarios": scenarios}))
            (repo / "mode.json").write_text(json.dumps(mode))
            if mode.startswith("hosted"):
                workflow = repo / ".github/workflows/ci-macos.yml"
                workflow.write_text("python3 scripts/test-qa-full-sweep.py --record-native-command " + " ".join(command) + "\n")
                execution = {"command": command, "executed_command": command, "source_revision": "a" * 40, "source_status": [], "source_stable": True, "exit_code": 0,
                             "stdout": "test boundary::contracts ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;", "stderr": ""}
                if mode == "hosted-skip":
                    execution["stderr"] = "DEFERRED: ps denied: Operation not permitted"
                if mode == "hosted-listing":
                    execution["stdout"] = "boundary::contracts: test\n1 test, 0 benchmarks"
                hosted = {"schema_version": 1, "evidence_type": "orbit-macos-platform", "platform": "macos",
                          "source_revision": "a" * 40, "command": native["command"], "outcome": "PASS",
                          "assertions": native["assertions"], "execution": [execution],
                          "producer": {"system": "github-actions", "repository": "fixture/orbit", "run_id": "123",
                                       "run_attempt": "1", "workflow_ref": "fixture/orbit/.github/workflows/ci-macos.yml@refs/heads/agent-main",
                                       "workflow_sha": "b" * 40},
                          "source_identity": {str(path.relative_to(repo)): hashlib.sha256(path.read_bytes()).hexdigest()
                                              for path in [listing, repo / "scripts/qa-full-sweep-inventory.json",
                                                           repo / "scripts/test-qa-full-sweep.py", workflow]}}
                evidence = repo / ".orbit/tmp/hosted.json"
                evidence.write_text(json.dumps(hosted))
                extra = [*extra, "--platform-evidence", str(evidence)]
            child = subprocess.run([sys.executable, str(driver), str(harness), str(repo),
                                    "--run-commands", *extra], cwd=repo,
                                   env={**os.environ, "PATH": f"{cargo.parent}:{os.environ['PATH']}"},
                                   capture_output=True, text=True, timeout=60)
            report_path = repo / ".orbit/tmp/report.json"
            if not report_path.is_file():
                raise AssertionError(f"sweep did not produce a report: {child.stdout}\n{child.stderr}")
            return json.loads(report_path.read_text())

        def row(report, name):
            return next(result for result in report["results"] if result["scenario"] == name)

        report = sweep([native], "complete")
        assert row(report, "macos-platform")["outcome"] == "NOT_RUN" and not row(report, "macos-platform")["assertions"], report
        report = sweep([rust], "complete")
        assert report["decision"] == "PASS" and row(report, "rust")["passing_rows"] == ["original_boundary"], report
        for mode in ["seatbelt", "ps"]:
            report = sweep([rust], mode)
            result = row(report, "rust")
            assert report["decision"] == "INCOMPLETE" and result["outcome"] == "NOT_RUN", report
            assert result["missing_capabilities"] and not result["assertions"], result
        report = sweep([rust], "missing-row")
        assert report["decision"] == "INCOMPLETE" and row(report, "rust")["outcome"] == "FAIL", report
        report = sweep([rust], "stale")
        assert row(report, "inventory-guard")["outcome"] == "FAIL" and report["decision"] == "INCOMPLETE", report
        report = sweep([native], "complete")
        assert row(report, "macos-platform")["outcome"] == "NOT_RUN" and not row(report, "macos-platform")["assertions"], report
        builtin = {"id": "isolated-cli-lifecycle", "required": True, "kind": "builtin", "capability": "local",
                   "assertions": ["global-init-persists-isolated-root"]}
        report = sweep([builtin], "builtin-skip")
        assert row(report, "isolated-cli-lifecycle")["outcome"] == "NOT_RUN" and report["decision"] == "INCOMPLETE", report
        website = {"id": "website-build", "required": True, "kind": "command", "capability": "website-build",
                   "command": ["npm", "--prefix", "website", "run", "build"], "assertions": ["website-built"]}
        report = sweep([website], "website-preparation-skip", ["--website-build"])
        assert row(report, "website-build")["outcome"] == "NOT_RUN" and report["decision"] == "INCOMPLETE", report
        report = sweep([native], "hosted-complete")
        assert report["decision"] == "PASS" and row(report, "macos-platform")["assertions"] == native["assertions"], report
        for mode in ["hosted-skip", "hosted-listing"]:
            report = sweep([native], mode)
            assert report["decision"] == "INCOMPLETE", report
        # Capture a real subprocess through the workflow recorder CLI. The
        # fixture Cargo/Git executables provide deterministic host adapters.
        (repo / "mode.json").write_text(json.dumps("complete"))
        records = repo / ".orbit/tmp/native-records"
        child = subprocess.run([sys.executable, str(driver), str(harness), str(repo),
                                "--native-execution-dir", str(records), "--record-native-command", *command],
                               cwd=repo, env={**os.environ, "PATH": f"{cargo.parent}:{os.environ['PATH']}"},
                               capture_output=True, text=True, timeout=60)
        assert child.returncode == 0, child.stderr
        recorded = json.loads(next(records.glob("*.json")).read_text())
        assert recorded["command"] == command and recorded["source_revision"] == "a" * 40, recorded
        assert recorded["stdout"].splitlines()[-1].startswith("test result: ok. 1 passed;"), recorded
        browser_args = ["--playwright-module", str(module), "--playwright-browsers-path", str(repo / "browsers")]
        report = sweep(browsers, "complete", browser_args)
        assert report["decision"] == "PASS", report
        directories = [row(report, name)["retained_evidence"]["directory"] for name in ["first", "second"]]
        assert len(set(directories)) == 2, directories
        for name in ["first", "second"]:
            result = row(report, name)
            for item in result["retained_evidence"]["files"]:
                path = Path(result["retained_evidence"]["directory"]) / item["path"]
                assert hashlib.sha256(path.read_bytes()).hexdigest() == item["sha256"], result
        for mode in ["overwrite", "missing"]:
            report = sweep(browsers, mode, browser_args)
            assert report["decision"] == "INCOMPLETE" and row(report, "first")["retained_evidence_error"], report
        report = sweep(browsers, "seatbelt", browser_args)
        assert all(row(report, name)["outcome"] == "NOT_RUN" for name in ["first", "second"]), report
        # Repeated invocations also retain earlier report bytes.
        assert all(Path(directory).is_dir() for directory in directories), directories
