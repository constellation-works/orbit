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
elif mode.startswith("native-"):
    record = json.loads(Path("transcript.json").read_text())
    print(record["stdout"], end="")
    print(record["stderr"], end="", file=sys.stderr)
    raise SystemExit(record.get("exit_code", 0))
else:
    print("     Running tests/boundary.rs (target/debug/deps/boundary-fixture)", file=sys.stderr)
    failed = mode in ("nonzero-notice", "suite-notice")
    print("running 1 test")
    if failed:
        print("test boundary::contracts ... FAILED")
        print("")
        print("failures:")
        print("    boundary::contracts")
        print("")
        print("test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s")
    else:
        print("test boundary::contracts ... ok")
        print("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s")
    if mode != "missing-row" and not failed:
        print("qa-scenario-pass: original_boundary", file=sys.stderr)
    if mode in ("seatbelt", "nonzero-notice", "suite-notice", "truncated-notice"):
        print("SKIP: sandbox-exec cannot apply a profile: Operation not permitted", file=sys.stderr)
    if mode == "ps":
        print("DEFERRED: ps denied: Operation not permitted", file=sys.stderr)
    if mode == "truncated-notice":
        print("x" * (1_048_576 + 64))
    if mode == "nonzero-notice":
        raise SystemExit(101)
'''

LIBTEST_FRAMES = r'''
mod boundary {
    #[test]
    fn contracts() {
        use std::io::Write;
        if std::env::var_os("QA_FRAME_CHILD").is_some() {
            eprintln!("qa-scenario-pass: original_boundary");
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "boundary::contracts", "--nocapture", "--test-threads=1"])
            .env("QA_FRAME_CHILD", "1")
            .output().unwrap();
        assert!(output.status.success());
        // Use the runtime scenario producer's locked stdout protocol, twice
        // to cover tables that forward several children before returning.
        for _ in 0..2 {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "qa-child-output-begin: boundary::contracts").unwrap();
            stdout.write_all(&output.stdout).unwrap();
            writeln!(stdout, "\nqa-child-output-end: boundary::contracts").unwrap();
        }
        std::io::stderr().write_all(&output.stderr).unwrap();
        assert!(std::env::var_os("QA_FRAME_FAIL").is_none(), "real outer failure");
    }
}
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


# A fixture child that outlives its test can write into the harness scratch
# tree while TemporaryDirectory cleanup removes it (observed OSError 66,
# Directory not empty, from host-resource-pressure.json). The hook below
# plants those exact leftovers at cleanup time and, unless the harness
# ignores cleanup errors, raises the observed OSError like the real race.
RACY_CLEANUP_HOOK = '''
import os as _os
if _os.environ.get("QA_SWEEP_RACY_CLEANUP") == "1":
    _RealTemporaryDirectory = qa.tempfile.TemporaryDirectory
    class _RacyCleanupTemporaryDirectory(_RealTemporaryDirectory):
        def cleanup(self):
            name = getattr(self, "name", None)
            if name and _os.path.basename(name).startswith("orbit-qa-full-"):
                leaf = Path(name) / "tmp/.tmpRACY/home/.orbit/cache/host-resource-pressure.json"
                leaf.parent.mkdir(parents=True, exist_ok=True)
                leaf.write_text('{"pressure": 1}')
                leaf.with_name(leaf.name + ".lock").write_text("lock")
                if not getattr(self, "_ignore_cleanup_errors", False):
                    raise OSError(66, "Directory not empty")
                return
            super().cleanup()
    qa.tempfile.TemporaryDirectory = _RacyCleanupTemporaryDirectory
'''


def scratch_cleanup_race_self_test(harness, scratch):
    """A cleanup failure in the harness scratch dir must not lose the report."""
    with tempfile.TemporaryDirectory(prefix="qa-scratch-cleanup-", dir=scratch) as tmp:
        repo = Path(tmp)
        for directory in ["bin", "scripts", ".orbit/tmp"]:
            (repo / directory).mkdir(parents=True, exist_ok=True)
        driver = repo / "driver.py"
        driver.write_text(DRIVER.replace("qa.main()", RACY_CLEANUP_HOOK + "qa.main()", 1))
        cargo = repo / "bin/cargo"
        cargo.write_text(f"#!{sys.executable}\n" + CARGO)
        cargo.chmod(0o755)
        orbit = repo / "bin/orbit"
        orbit.write_text(f"#!{sys.executable}\nprint('fixture orbit')\n")
        orbit.chmod(0o755)
        git = repo / "bin/git"
        git.write_text(f"#!{sys.executable}\nprint('" + "a" * 40 + "')\n")
        git.chmod(0o755)
        (repo / "scripts/test-qa-full-sweep.py").write_bytes(harness.read_bytes())
        command = ["cargo", "test", "-p", "fixture", "--test", "boundary", "boundary::", "--", "--show-output"]
        rust = {"id": "rust", "required": True, "capability": "local", "kind": "cargo-test",
                "command": command, "assertions": ["original-boundary"],
                "required_tests": ["boundary::contracts"], "required_rows": ["original_boundary"]}
        (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps({"scenarios": [rust]}))
        (repo / "mode.json").write_text(json.dumps("complete"))
        child = subprocess.run([sys.executable, str(driver), str(harness), str(repo), "--run-commands"],
                               cwd=repo,
                               env={**os.environ, "PATH": f"{cargo.parent}:{os.environ['PATH']}",
                                    "QA_SWEEP_RACY_CLEANUP": "1"},
                               capture_output=True, text=True, timeout=60)
        report_path = repo / ".orbit/tmp/report.json"
        if not report_path.is_file():
            raise AssertionError(f"scratch cleanup failure lost the report: {child.stdout}\n{child.stderr}")
        report = json.loads(report_path.read_text())
        cleanup = report.get("scratch_cleanup", {})
        expected = "tmp/.tmpRACY/home/.orbit/cache/host-resource-pressure.json"
        if expected not in cleanup.get("leftover_paths", []):
            raise AssertionError(f"report omits scratch leftovers: {cleanup}")
        if not cleanup.get("warning"):
            raise AssertionError(f"report records no scratch cleanup warning: {cleanup}")
        result = next(item for item in report["results"] if item["scenario"] == "rust")
        if child.returncode != 0 or report["decision"] != "PASS" or result["outcome"] != "PASS":
            raise AssertionError(f"scratch cleanup warning failed a passing sweep: {child.returncode} {report['decision']} {result}")


def self_test():
    harness = Path(os.environ.get("QA_SWEEP_HARNESS_UNDER_TEST", Path(__file__).with_name("test-qa-full-sweep.py"))).resolve()
    scratch = Path(__file__).resolve().parent.parent / ".orbit/tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    native_recorder_prefix_self_test(harness, scratch)
    retired_cases_orphan_proof_self_test(harness, scratch)
    scratch_cleanup_race_self_test(harness, scratch)
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
            if mode.startswith("hosted") or mode.startswith("native-"):
                workflow = repo / ".github/workflows/ci-macos.yml"
                workflow.write_text("python3 scripts/test-qa-full-sweep.py --record-native-command " + " ".join(command) + "\n")
                execution = {"command": command, "executed_command": [*command, "--", "--show-output"] if "--" not in command else command, "source_revision": "a" * 40, "source_status": [], "source_stable": True, "exit_code": 0,
                             "stdout": "running 1 test\ntest boundary::contracts ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s", "stderr": "     Running tests/boundary.rs (target/debug/deps/boundary-fixture)\n"}
                if mode.startswith("native-"):
                    execution.update(json.loads((repo / "transcript.json").read_text()))
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
            assert result["exit_code"] == 0, result
        for mode in ["nonzero-notice", "suite-notice", "truncated-notice"]:
            report = sweep([rust], mode)
            result = row(report, "rust")
            assert report["decision"] == "INCOMPLETE" and result["outcome"] == "FAIL", (mode, result)
            assert result["missing_capabilities"] and not result["assertions"], result
            notice = "SKIP: sandbox-exec cannot apply a profile: Operation not permitted"
            assert notice in result["missing_capabilities"] and notice in result["stderr"], result
            if mode == "nonzero-notice":
                assert result["exit_code"] == 101, result
                assert "test boundary::contracts ... FAILED" in result["stdout"], result
                assert "test result: FAILED. 0 passed; 1 failed;" in result["stdout"], result
            elif mode == "suite-notice":
                assert result["exit_code"] == 0, result
                assert "test boundary::contracts ... FAILED" in result["stdout"], result
                assert "cargo test reported failing outer tests" in result["stderr"], result
            else:
                assert result["exit_code"] == 0 and result["output_truncated"], result
                assert "command output was truncated" in result["stderr"], result
        report = sweep([rust], "missing-row")
        assert report["decision"] == "INCOMPLETE" and row(report, "rust")["outcome"] == "FAIL", report
        report = sweep([rust], "stale")
        assert row(report, "inventory-guard")["outcome"] == "FAIL" and report["decision"] == "INCOMPLETE", report
        report = sweep([native], "complete")
        assert row(report, "macos-platform")["outcome"] == "NOT_RUN" and not row(report, "macos-platform")["assertions"], report
        builtin = {"id": "isolated-cli-lifecycle", "required": True, "kind": "builtin", "capability": "local",
                   "assertions": ["global-init-persists-isolated-root"]}
        report = sweep([builtin], "builtin-skip")
        builtin_row = row(report, "isolated-cli-lifecycle")
        assert builtin_row["outcome"] == "FAIL" and builtin_row["exit_code"] == 1, report
        assert builtin_row["missing_capabilities"] and report["decision"] == "INCOMPLETE", builtin_row
        assert "SKIP: sandbox-exec cannot apply a profile: init refused" in builtin_row["stderr"], builtin_row
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
        # Retain and replay the successful hosted command's full test_env
        # stdout, including the failed child and the parent's refusal checks.
        actual = json.loads(Path(__file__).with_name("fixtures").joinpath("qa-native-test-env.json").read_text())
        original_command = command
        command = ["cargo", "test", "--no-fail-fast", "-p", "orbit-common", "--test", "test_env", "--locked"]
        for mode in ["success", "multiple-success", "nocapture", "directory-target", "outer-failure", "early-target-failure", "nonzero", "missing-target",
                     "wrong-target", "extra-target", "zero", "incomplete-rows", "child-only",
                     "ambiguous", "truncated", "seatbelt", "ps", "unstable", "wrong-revision"]:
            execution = {"stdout": actual["stdout"], "stderr": actual["stderr"], "exit_code": 0,
                         "executed_command": [*command, "--", "--show-output"]}
            if mode == "outer-failure":
                execution["stdout"] = execution["stdout"].replace(
                    "test reexec_guard_rejects_missing_ignored_and_failed_children ... ok",
                    "test reexec_guard_rejects_missing_ignored_and_failed_children ... FAILED")
                before, table = execution["stdout"].rsplit("successes:\n", 1)
                table = table.replace("    reexec_guard_rejects_missing_ignored_and_failed_children\n", "").replace(
                    "test result: ok. 11 passed; 0 failed; 6 ignored;",
                    "failures:\n    reexec_guard_rejects_missing_ignored_and_failed_children\n\ntest result: FAILED. 10 passed; 1 failed; 6 ignored;")
                execution["stdout"] = before + "successes:\n" + table
            elif mode == "multiple-success":
                execution["stdout"] = "running 1 test\ntest earlier ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n" + execution["stdout"]
                execution["stderr"] = "     Running tests/earlier.rs (target/debug/deps/earlier)\n" + execution["stderr"]
                command = ["cargo", "test", "-p", "orbit-common", "--locked"]
                execution["executed_command"] = [*command, "--", "--show-output"]
            elif mode == "nocapture":
                command = [*command, "--", "--nocapture"]
                execution["executed_command"] = [*command[:-1], "--show-output"]
            elif mode == "early-target-failure":
                execution["stdout"] = "running 1 test\ntest earlier ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n" + execution["stdout"]
                execution["stderr"] = "     Running tests/earlier.rs (target/debug/deps/earlier)\n" + execution["stderr"]
                command = ["cargo", "test", "-p", "orbit-common", "--locked"]
                execution["executed_command"] = [*command, "--", "--show-output"]
            elif mode == "nonzero":
                execution["exit_code"] = 101
            elif mode == "directory-target":
                execution["stderr"] = execution["stderr"].replace("tests/test_env.rs", "tests/test_env/main.rs")
            elif mode == "missing-target":
                execution["stderr"] = ""
            elif mode == "wrong-target":
                execution["stderr"] = execution["stderr"].replace("tests/test_env.rs", "tests/unrelated.rs").replace("/test_env-", "/unrelated-")
            elif mode == "extra-target":
                execution["stderr"] += "     Running tests/unreported.rs (target/debug/deps/unreported)\n"
            elif mode == "zero":
                execution["stdout"] = "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
            elif mode == "incomplete-rows":
                execution["stdout"] = execution["stdout"].replace("test inherited_authority_is_removed_from_a_child_command ... ok\n", "")
            elif mode == "child-only":
                execution["stdout"] = execution["stdout"].rsplit("test result: ok. 11 passed;", 1)[0]
            elif mode == "ambiguous":
                execution["stdout"] = "running 1 test\nrunning 1 test\ntest unrelated ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
            elif mode == "truncated":
                execution["output_truncated"] = True
            elif mode == "seatbelt":
                execution["stdout"] += "SKIP: sandbox-exec cannot apply a profile: actual refusal\n"
            elif mode == "ps":
                execution["stderr"] += "DEFERRED: ps denied: Operation not permitted\n"
            elif mode == "unstable":
                execution["source_stable"] = False
            elif mode == "wrong-revision":
                execution["source_revision"] = "c" * 40
            (repo / "transcript.json").write_text(json.dumps(execution))
            # Run the public recorder first; feed its actual retained command
            # bytes to the public platform import, which shares emitter rules.
            records = repo / f".orbit/tmp/native-{mode}"
            (repo / "mode.json").write_text(json.dumps(f"native-{mode}"))
            child = subprocess.run([sys.executable, str(driver), str(harness), str(repo),
                                    "--native-execution-dir", str(records), "--record-native-command", *command],
                                   cwd=repo, env={**os.environ, "PATH": f"{cargo.parent}:{os.environ['PATH']}"},
                                   capture_output=True, text=True, timeout=60)
            recorded = json.loads(next(records.glob("*.json")).read_text())
            assert recorded["stdout"] == execution["stdout"], recorded
            # Adapt only fields whose negative control concerns the emitter's
            # completeness/identity, rather than recorder subprocess behavior.
            for key in ["output_truncated", "source_stable", "source_revision"]:
                if key in execution:
                    recorded[key] = execution[key]
            (repo / "transcript.json").write_text(json.dumps(recorded))
            report = sweep([native], f"native-{mode}")
            assert (report["decision"] == "PASS") == (mode in ["success", "multiple-success", "nocapture", "directory-target"]), (mode, report)
            if mode in ["outer-failure", "early-target-failure"]:
                assert "failing outer tests" in row(report, "platform-evidence-import")["stderr"], (mode, report)
            if mode in ["success", "multiple-success", "nocapture", "directory-target"]:
                retained = row(report, "macos-platform")["native_evidence"]["execution"][0]
                assert "test result: FAILED. 0 passed; 1 failed;" in retained["stdout"], retained
                assert retained["test_counts"] == {"passed": 12 if mode == "multiple-success" else 11, "failed": 0, "ignored": 6, "suites": 2 if mode == "multiple-success" else 1}, retained
                assert "reexec_guard_rejects_missing_ignored_and_failed_children" in retained["passing_tests"], retained
            command = ["cargo", "test", "--no-fail-fast", "-p", "orbit-common", "--test", "test_env", "--locked"]
        forwarded = actual["forwarded_output"]
        command = forwarded["command"]
        owner = "baseline_hold_tick::a_hold_check_runs_off_the_tick_and_ticks_meanwhile_fire_routines_and_auto_tasks_scenarios"
        for mode in ["complete", "unframed", "unowned", "missing-end", "wrong-end", "notice"]:
            execution = {"stdout": forwarded["stdout"], "stderr": forwarded["stderr"], "exit_code": 0,
                         "executed_command": command}
            if mode == "unframed":
                execution["stdout"] = forwarded["unframed_stdout"]
            elif mode == "unowned":
                execution["stdout"] = execution["stdout"].replace("qa-child-output-begin: " + owner, "qa-child-output-begin: unrelated").replace("qa-child-output-end: " + owner, "qa-child-output-end: unrelated")
            elif mode == "missing-end":
                execution["stdout"] = execution["stdout"].rsplit("qa-child-output-end:", 1)[0]
            elif mode == "wrong-end":
                execution["stdout"] = execution["stdout"].replace("qa-child-output-end: " + owner, "qa-child-output-end: unrelated")
            elif mode == "notice":
                execution["stdout"] = execution["stdout"].replace("qa-child-output-begin: " + owner, "qa-child-output-begin: " + owner + "\nSKIP: sandbox-exec cannot apply a profile: real child refusal")
            (repo / "transcript.json").write_text(json.dumps(execution))
            report = sweep([native], "native-forwarded-" + mode)
            assert (report["decision"] == "PASS") == (mode == "complete"), (mode, report)
            if mode == "complete":
                retained = row(report, "macos-platform")["native_evidence"]["execution"][0]
                assert retained["test_counts"] == {"passed": 1, "failed": 0, "ignored": 0, "suites": 1}, retained
                assert retained["stdout"] == forwarded["stdout"], retained
        command = original_command
        # Compile once, then capture actual libtest bytes with one and two
        # threads. Replay through both the public recorder/import and sweep.
        source = repo / "frames.rs"
        source.write_text(LIBTEST_FRAMES)
        binary = repo / "frames"
        compiled = subprocess.run(["rustc", "--test", str(source), "-o", str(binary)],
                                  capture_output=True, text=True, timeout=60)
        assert compiled.returncode == 0, compiled.stderr
        for threads in [1, 2]:
            captures = {}
            for failure in [False, True]:
                env = {key: value for key, value in os.environ.items()
                       if key not in ["QA_FRAME_CHILD", "QA_FRAME_FAIL"]}
                env["RUST_TEST_THREADS"] = str(threads)
                if failure:
                    env["QA_FRAME_FAIL"] = "1"
                child = subprocess.run([str(binary), "--show-output"], env=env,
                                       capture_output=True, text=True, timeout=60)
                assert child.returncode == (101 if failure else 0), child.stderr
                serial_prefix = "test boundary::contracts ... qa-child-output-begin:"
                assert (serial_prefix in child.stdout) == (threads == 1), child.stdout
                captures[failure] = child
            for mode in ["complete", "outer-failure", "wrong-owner", "missing-end", "wrong-end",
                         "missing-outer-row", "missing-row", "notice", "truncated", "unstable"]:
                child = captures[mode == "outer-failure"]
                execution = {"stdout": child.stdout,
                             "stderr": "     Running tests/boundary.rs (target/debug/deps/boundary-fixture)\n" + child.stderr,
                             # Even a claimed zero exit must not hide a real failed outer test.
                             "exit_code": 0, "executed_command": command}
                if mode == "wrong-owner":
                    execution["stdout"] = execution["stdout"].replace("output-begin: boundary::contracts", "output-begin: unrelated").replace("output-end: boundary::contracts", "output-end: unrelated")
                elif mode == "missing-end":
                    execution["stdout"] = execution["stdout"].replace("qa-child-output-end: boundary::contracts", "")
                elif mode == "wrong-end":
                    execution["stdout"] = execution["stdout"].replace("output-end: boundary::contracts", "output-end: unrelated")
                elif mode == "missing-outer-row":
                    # Remove only the final outer completion, keeping child results intact.
                    completion = "ok\n" if threads == 1 else "test boundary::contracts ... ok\n"
                    offset = execution["stdout"].rfind(completion)
                    assert offset >= 0, execution
                    execution["stdout"] = execution["stdout"][:offset] + execution["stdout"][offset + len(completion):]
                elif mode == "missing-row":
                    execution["stderr"] = execution["stderr"].replace("qa-scenario-pass: original_boundary\n", "")
                elif mode == "notice":
                    execution["stdout"] = execution["stdout"].replace("qa-child-output-begin: boundary::contracts", "qa-child-output-begin: boundary::contracts\nSKIP: sandbox-exec cannot apply a profile: real child refusal")
                elif mode == "truncated":
                    execution["output_truncated"] = True
                elif mode == "unstable":
                    execution["source_stable"] = False
                (repo / "transcript.json").write_text(json.dumps(execution))
                (repo / "mode.json").write_text(json.dumps("native-libtest"))
                records = repo / f".orbit/tmp/libtest-{threads}-{mode}"
                recorded_child = subprocess.run([sys.executable, str(driver), str(harness), str(repo),
                                                "--native-execution-dir", str(records), "--record-native-command", *command],
                                               cwd=repo, env={**os.environ, "PATH": f"{cargo.parent}:{os.environ['PATH']}"},
                                               capture_output=True, text=True, timeout=60)
                recorded = json.loads(next(records.glob("*.json")).read_text())
                assert recorded["stdout"] == execution["stdout"] and recorded["stderr"] == execution["stderr"], recorded
                for key in ["output_truncated", "source_stable"]:
                    if key in execution:
                        recorded[key] = execution[key]
                (repo / "transcript.json").write_text(json.dumps(recorded))
                report = sweep([rust, native], "native-libtest")
                assert (report["decision"] == "PASS") == (mode == "complete"), (threads, mode, report)
                if mode == "complete":
                    assert recorded_child.returncode == 0, recorded_child.stderr
                    retained = row(report, "macos-platform")["native_evidence"]["execution"][0]
                    assert retained["stdout"] == child.stdout, retained
                    assert retained["passing_tests"] == ["boundary::contracts"], retained
                    assert retained["outer_suites"][0]["child_owners"] == ["boundary::contracts"], retained
                    assert retained["test_counts"] == {"passed": 1, "failed": 0, "ignored": 0, "suites": 1}, retained
                    assert row(report, "rust")["stdout"] == child.stdout, report
                    assert row(report, "rust")["passing_rows"] == ["original_boundary"], report
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


def retired_cases_orphan_proof_self_test(harness, scratch):
    """Reject retired-case command_tests/command_rows without an executable command through the public sweep entrypoint."""
    with tempfile.TemporaryDirectory(prefix="qa-retired-cases-", dir=scratch) as tmp:
        repo = Path(tmp)
        for directory in ["bin", "scripts", ".orbit/tmp"]:
            (repo / directory).mkdir(parents=True, exist_ok=True)
        driver = repo / "driver.py"
        driver.write_text(DRIVER.replace(
            'qa.validate_inventory = lambda *_: ([], {"cli": []})',
            'qa.source_contracts = lambda *_: {kind: set() for kind in ["cli", "job", "activity", "mcp", "api", "dashboard"]}'
        ))
        (repo / "mode.json").write_text(json.dumps("complete"))
        cargo = repo / "bin/cargo"
        cargo.write_text(f"#!{sys.executable}\n" + CARGO)
        cargo.chmod(0o755)
        orbit = repo / "bin/orbit"
        orbit.write_text(f"#!{sys.executable}\nprint('fixture orbit')\n")
        orbit.chmod(0o755)
        command = ["cargo", "test", "-p", "fixture", "--test", "boundary", "boundary::", "--", "--show-output"]

        def run_sweep(case, mode="complete"):
            (repo / "mode.json").write_text(json.dumps(mode))
            scenario = {
                "id": "rust", "required": True, "capability": "local", "kind": "cargo-test",
                "command": command, "assertions": ["original-boundary"],
                "required_tests": ["boundary::contracts"], "required_rows": ["original_boundary"],
                "behavior": ["normal", "failure"], "retired_cases": [case],
            }
            inventory = {
                "surface_contracts": {"cli_top_level": [], "job_assets": [], "sha256": {}},
                "feature_families": [],
                "scenarios": [scenario],
                "required_capability_gaps": {},
            }
            (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps(inventory))
            child = subprocess.run(
                [sys.executable, str(driver), str(harness), str(repo), "--run-commands"],
                cwd=repo,
                env={**os.environ, "PATH": f"{cargo.parent}:{os.environ['PATH']}"},
                capture_output=True, text=True, timeout=60,
            )
            report = json.loads((repo / ".orbit/tmp/report.json").read_text())
            guard = next(r for r in report["results"] if r["scenario"] == "inventory-guard")
            return child, report, guard

        orphan_case = {
            "retired": "old::admission",
            "guarantee": "Generation admission still refuses incompatible participants",
            "disposition": "verified-equivalent",
            "reason": "The separately named command checks generation admission",
            "command_tests": ["admission::never_executed"],
        }
        child, report, guard = run_sweep(orphan_case)
        assert child.returncode == 1, (child.returncode, child.stderr)
        assert report["decision"] == "INCOMPLETE", report
        assert guard["outcome"] == "FAIL", guard
        assert "command_tests or command_rows require an executable command" in guard["stderr"], guard

        null_command_case = {
            "retired": "old::admission",
            "guarantee": "Generation admission still refuses incompatible participants",
            "disposition": "verified-equivalent",
            "reason": "The separately named command checks generation admission",
            "command": None,
            "command_tests": ["admission::never_executed"],
        }
        child, report, guard = run_sweep(null_command_case)
        assert child.returncode == 1, (child.returncode, child.stderr)
        assert report["decision"] == "INCOMPLETE", report
        assert guard["outcome"] == "FAIL", guard
        assert "command_tests or command_rows require an executable command" in guard["stderr"], guard

        commandless_case = {
            "retired": "old::admission",
            "guarantee": "Generation admission still refuses incompatible participants",
            "disposition": "verified-equivalent",
            "reason": "The same behavioral row runs inside the primary scenario",
            "required_rows": ["original_boundary"],
        }
        child, report, guard = run_sweep(commandless_case)
        assert child.returncode == 0, (child.returncode, child.stderr)
        assert report["decision"] == "PASS", report
        assert guard["outcome"] == "PASS", guard

        valid_command_case = {
            "retired": "old::admission",
            "guarantee": "Generation admission still refuses incompatible participants",
            "disposition": "verified-equivalent",
            "reason": "The separately named command checks generation admission",
            "command": command,
            "command_tests": ["boundary::contracts"],
        }
        child, report, guard = run_sweep(valid_command_case)
        assert child.returncode == 0, (child.returncode, child.stderr)
        assert report["decision"] == "PASS", report
        assert guard["outcome"] == "PASS", guard

        child, report, guard = run_sweep(valid_command_case, mode="seatbelt")
        assert report["decision"] == "INCOMPLETE", report


def native_recorder_prefix_self_test(harness, scratch):
    """Wrapper delimiters must not consume Cargo's separator or libtest options."""
    with tempfile.TemporaryDirectory(prefix="qa-native-prefix-", dir=scratch) as tmp:
        repo = Path(tmp)
        (repo / "Cargo.toml").write_text(
            '[package]\nname = "recorder-prefix"\nversion = "0.0.0"\nedition = "2021"\n'
            '[workspace]\n[[test]]\nname = "boundary"\npath = "boundary.rs"\n')
        (repo / "boundary.rs").write_text(
            '#[test]\nfn selected() { println!("selected test diagnostics"); }\n'
            '#[test]\nfn unrelated() { panic!("the recorder must preserve the test filter"); }\n')
        wrapper = repo / "wrapper.py"
        wrapper.write_text(
            'import os, sys\n'
            'start = 3 if sys.argv[1] == "-p" else sys.argv.index("--") + 1\n'
            'command = sys.argv[start:]\n'
            'os.execvp(command[0], command)\n')
        cargo = ["cargo", "test", "--offline", "--test", "boundary", "selected"]
        prefix = [sys.executable, str(wrapper), "--"]
        options = ["--exact", "--test-threads=1", "--nocapture", "--show-output"]
        cases = [
            ("direct", [], [], ["--show-output"]),
            ("direct-options", [], options, [flag for flag in options if flag != "--nocapture"]),
            ("wrapper", prefix, [], ["--show-output"]),
            ("wrapper-options", prefix, options, [flag for flag in options if flag != "--nocapture"]),
            ("sandbox-style-prefix", [sys.executable, str(wrapper), "-p", "(version 1)(allow default)"],
             ["--exact", "--nocapture"], ["--exact", "--show-output"]),
            ("prefix-options", [sys.executable, str(wrapper), "--show-output", "--nocapture", "--"],
             ["--exact", "--nocapture"], ["--exact", "--show-output"]),
        ]
        env = {**os.environ, "CARGO_TARGET_DIR": str(repo / "target"),
               "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": ""}
        for name, wrapper_args, libtest_args, expected_args in cases:
            print(f"native recorder prefix regression: {name}", flush=True)
            command = [*wrapper_args, *cargo, *(["--", *libtest_args] if libtest_args else [])]
            records = repo / name
            child = subprocess.run(
                [sys.executable, str(harness), "--repo-root", str(repo),
                 "--native-execution-dir", str(records), "--record-native-command", *command],
                cwd=repo, env=env, capture_output=True, text=True, timeout=60)
            assert child.returncode == 0, (name, child.stdout, child.stderr)
            recorded = json.loads(next(records.glob("*.json")).read_text())
            assert recorded["command"] == command, (name, recorded)
            assert recorded["executed_command"] == [*wrapper_args, *cargo, "--", *expected_args], (name, recorded)
            assert recorded["exit_code"] == 0 and recorded["cleanup_verified"], (name, recorded)
            assert "test selected ... ok" in recorded["stdout"], (name, recorded)
            assert "selected test diagnostics" in recorded["stdout"], (name, recorded)
            assert "test result: ok. 1 passed; 0 failed;" in recorded["stdout"], (name, recorded)
