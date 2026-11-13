"""Command-boundary regressions for selection, execution and retained QA bytes."""

import copy
import hashlib
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import time


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
_mode = json.loads((repo / "mode.json").read_text())
if not _mode.startswith("builtin-"):
    qa.run_builtins = lambda *_: ([], None)
if _mode == "local-cleanup-unverified":
    _real_run = qa.run
    def _run_with_unverified_cleanup(*args, **kwargs):
        _evidence = _real_run(*args, **kwargs)
        _evidence["cleanup_verified"] = False
        return _evidence
    qa.run = _run_with_unverified_cleanup
if _mode == "local-supervisor-lost":
    qa._PROCESS_SUPERVISOR = "import os, sys; os.close(int(sys.argv[1]))"
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
elif "build" in sys.argv:
    import shutil
    target = Path(sys.argv[sys.argv.index("--target-dir") + 1]) / "debug/orbit"
    if mode != "build-failed":
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2("bin/orbit", target)
    print("fixture candidate build stdout")
    print("fixture candidate build stderr", file=sys.stderr)
    if mode in ("build-failed", "build-failed-with-binary"):
        raise SystemExit(42)
elif "--list" in sys.argv:
    print(("unrelated" if mode == "stale" else "boundary::contracts") + ": test")
elif mode.startswith("native-"):
    record = json.loads(Path("transcript.json").read_text())
    print(record["stdout"], end="")
    print(record["stderr"], end="", file=sys.stderr)
    raise SystemExit(record.get("exit_code", 0))
else:
    print("     Running tests/boundary.rs (target/debug/deps/boundary-fixture)", file=sys.stderr)
    failed = mode in ("nonzero-notice", "suite-notice", "builtin-probe-available-eperm")
    if mode == "builtin-probe-available-eperm":
        print("fixture write: Operation not permitted (os error 1)", file=sys.stderr)
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
# tree while scratch cleanup removes it (observed OSError 66,
# Directory not empty, from host-resource-pressure.json). The hook below
# plants those exact leftovers at cleanup time and, unless the harness
# ignores cleanup errors, raises the observed OSError like the real race.
RACY_CLEANUP_HOOK = '''
import os as _os
if _os.environ.get("QA_SWEEP_RACY_CLEANUP") == "1":
    _real_rmtree = qa.shutil.rmtree
    def _racy_rmtree(name, *args, **kwargs):
        if _os.path.basename(name).startswith("orbit-qa-full-"):
            leaf = Path(name) / "tmp/.tmpRACY/home/.orbit/cache/host-resource-pressure.json"
            leaf.parent.mkdir(parents=True, exist_ok=True)
            leaf.write_text('{"pressure": 1}')
            leaf.with_name(leaf.name + ".lock").write_text("lock")
            if not kwargs.get("ignore_errors", False):
                raise OSError(66, "Directory not empty")
            return
        return _real_rmtree(name, *args, **kwargs)
    qa.shutil.rmtree = _racy_rmtree
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


def report_output_self_test(harness, scratch):
    """Both public entrypoints keep default reports in ignored scratch storage."""
    with tempfile.TemporaryDirectory(prefix="qa-report-output-", dir=scratch) as tmp:
        repo = Path(tmp)
        (repo / "scripts").mkdir()
        (repo / "scripts/qa-full-sweep-inventory.json").write_text('{"scenarios": []}')
        wrapper = repo / "scripts/qa-full-sweep.sh"
        wrapper.write_bytes(harness.with_name("qa-full-sweep.sh").read_bytes())
        entrypoint = repo / "scripts/test-qa-full-sweep.py"
        entrypoint.write_text(f'''
import importlib.util
from pathlib import Path
spec = importlib.util.spec_from_file_location("qa", {str(harness)!r})
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
qa.__file__ = __file__
qa.validate_inventory = lambda *_: ([], {{"cli": []}})
qa.cargo_inventory_errors = lambda *_, **__: []
qa.candidate_source = lambda *_: {{"candidate_id": "fixture", "head": "a" * 40,
                                  "status": [], "diff_sha256": "fixture"}}
qa.main()
''')
        binary = repo / "orbit"
        binary.write_text("#!/bin/sh\necho 'fixture orbit'\n")
        binary.chmod(0o755)
        for entry in ([sys.executable, str(entrypoint)], ["bash", str(wrapper)]):
            for explicit in (False, True):
                output = repo / (".orbit/tmp/explicit/report.json" if explicit else
                                 ".orbit/tmp/qa-full-sweep/qa-full-sweep-report.json")
                child = subprocess.run(
                    [*entry, "--orbit-bin", str(binary),
                     *(["--output", str(output)] if explicit else [])],
                    cwd=repo, capture_output=True, text=True, timeout=60)
                assert child.returncode == 0 and output.is_file(), (
                    entry, explicit, child.returncode, child.stdout, child.stderr)
                report = json.loads(output.read_text())
                assert report["schema_version"] == 2, report
                assert not (repo / "qa-full-sweep-report.json").exists(), child.stdout
                output.unlink()


def builtin_readback_self_test(harness, scratch):
    """Drive the real builtins through independently corrupted command responses."""
    namespace = runpy.run_path(str(harness))
    builtins = namespace["run_builtins"]
    scope = builtins.__globals__
    original_finalize = scope["finalize_result"]

    class StopFixture(Exception):
        pass

    for mode in ("valid", "wrong-workspace", "malformed-workspace", "unchanged-artifact"):
        with tempfile.TemporaryDirectory(prefix="qa-readback-", dir=scratch) as tmp:
            root = Path(tmp)
            captured = []
            def finalize(*args, **kwargs):
                result = original_finalize(*args, **kwargs)
                captured.append(result)
                return result
            def run(argv, **kwargs):
                stdout = ""
                if "workspace" in argv and "show" in argv:
                    workspace = root / ("other-workspace" if mode == "wrong-workspace" else "workspace")
                    stdout = "{}" if mode == "malformed-workspace" else json.dumps({"checkout": {"repo_root": str(workspace)}})
                elif "task" in argv and "add" in argv:
                    stdout = json.dumps({"id": "QAF-1", "title": "QA behavioral fixture"})
                elif "task" in argv and "show" in argv:
                    stdout = json.dumps({"id": "QAF-1", "title": "QA behavioral fixture", "acceptance_criteria": ["fixture round trip"]})
                elif "artifact" in argv and "put" in argv:
                    stdout = "{}"
                elif "artifact" in argv and "get" in argv:
                    stdout = "old persisted bytes\n" if mode == "unchanged-artifact" else "qa evidence payload\n"
                elif "task" in argv and "list" in argv:
                    raise StopFixture()
                return {"command": argv, "exit_code": 0, "stdout": stdout, "stderr": "", "cleanup_verified": True}
            scope["run"] = run
            scope["finalize_result"] = finalize
            scope["scenario_sandbox_probe"] = lambda *args: None
            try:
                builtins(root, "orbit", root, {}, "fixture")
            except StopFixture:
                pass
            else:
                raise AssertionError("fixture did not reach artifact readback")
            workspace = next(row for row in captured if "workspace" in row["command"] and "show" in row["command"])
            artifact = next(row for row in captured if "artifact" in row["command"] and "get" in row["command"])
            assert workspace["outcome"] == ("FAIL" if mode in ("wrong-workspace", "malformed-workspace") else "PASS"), (mode, workspace)
            assert artifact["outcome"] == ("FAIL" if mode == "unchanged-artifact" else "PASS"), (mode, artifact)


def self_test():
    harness = Path(os.environ.get("QA_SWEEP_HARNESS_UNDER_TEST", Path(__file__).with_name("test-qa-full-sweep.py"))).resolve()
    scratch = Path(__file__).resolve().parent.parent / ".orbit/tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    builtin_readback_self_test(harness, scratch)
    report_output_self_test(harness, scratch)
    candidate_build_report_self_test(harness, scratch)
    native_emitter_cleanup_self_test(harness, scratch)
    native_recorder_prefix_self_test(harness, scratch)
    retired_cases_orphan_proof_self_test(harness, scratch)
    source_identity_self_test(harness, scratch)
    scratch_cleanup_race_self_test(harness, scratch)
    builtin_refusal_self_test(harness, scratch)
    builtin_refusal_self_test(harness, scratch, wire=True)
    builtin_refusal_self_test(harness, scratch, nested=True)
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
                     "command": [sys.executable, str(browser), name, "{evidence_dir}", "{playwright_module}"], "assertions": [name]}
                    for name in ["first", "second"]]

        def sweep(scenarios, mode, extra=()):
            (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps({"scenarios": scenarios}))
            (repo / "mode.json").write_text(json.dumps(mode))
            if mode.startswith("hosted") or mode.startswith("native-"):
                workflow = repo / ".github/workflows/ci-macos.yml"
                workflow.write_text("python3 scripts/test-qa-full-sweep.py --record-native-command " + " ".join(command) + "\n")
                execution = {"command": command, "executed_command": [*command, "--", "--show-output"] if "--" not in command else command, "source_revision": "a" * 40, "source_status": [], "source_stable": True, "exit_code": 0,
                             "cleanup_verified": True,
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
        # An exit-zero local command with unverified cleanup must not earn
        # sign-off through the public --run-commands route. The driver runs
        # the real bounded command, then injects cleanup_verified=False as
        # the supervisor observation; raw evidence stays on the row.
        local_started = time.perf_counter()
        local = {"id": "local-command", "required": True, "capability": "local", "kind": "command",
                 "command": [sys.executable, "-c",
                             "import sys; print('local-command stdout'); "
                             "print('local-command stderr', file=sys.stderr)"],
                 "assertions": ["local-command-ran"]}
        notice = "SKIP: sandbox-exec cannot apply a profile: Operation not permitted"
        local_notice = {**local, "command": [sys.executable, "-c",
                                             "import sys; print('local-command stdout'); "
                                             "print('local-command stderr', file=sys.stderr); "
                                             f"print({notice!r}, file=sys.stderr)"]}
        report = sweep([local], "complete")
        result = row(report, "local-command")
        assert report["decision"] == "PASS" and result["outcome"] == "PASS", report
        assert result["assertions"] == ["local-command-ran"], result
        assert result["exit_code"] == 0 and result["cleanup_verified"] is True, result
        assert result["stdout"] == "local-command stdout\n", result
        assert result["stderr"] == "local-command stderr\n", result
        assert report["scratch_cleanup"]["retained"] is False, report
        assert not Path(report["scratch_cleanup"]["root"]).exists(), report
        report = sweep([local], "local-supervisor-lost")
        result = row(report, "local-command")
        assert report["decision"] == "INCOMPLETE" and result["outcome"] == "FAIL", report
        assert result["exit_code"] is None and result["cleanup_verified"] is False, result
        assert result["open_descendants"] is None and result["detached_workers"] is None, result
        assert report["scratch_cleanup"]["retained"] is True, report
        assert Path(report["scratch_cleanup"]["root"]).is_dir(), report
        report = sweep([local], "local-cleanup-unverified")
        result = row(report, "local-command")
        assert report["decision"] == "INCOMPLETE" and result["outcome"] == "FAIL", report
        assert result["assertions"] == [], result
        assert result["exit_code"] == 0 and result["cleanup_verified"] is False, result
        assert result["stdout"] == "local-command stdout\n", result
        assert result["stderr"] == "local-command stderr\n", result
        assert report["scratch_cleanup"]["retained"] is True, report
        assert Path(report["scratch_cleanup"]["root"]).is_dir(), report
        # Cleanup refusal wins over a capability notice: still FAIL, never
        # PASS or NOT_RUN, with the notice retained as a capability gap.
        report = sweep([local_notice], "local-cleanup-unverified")
        result = row(report, "local-command")
        assert report["decision"] == "INCOMPLETE" and result["outcome"] == "FAIL", report
        assert result["assertions"] == [], result
        assert result["exit_code"] == 0 and result["cleanup_verified"] is False, result
        assert result["stdout"] == "local-command stdout\n", result
        assert "local-command stderr" in result["stderr"] and notice in result["stderr"], result
        assert notice in result["missing_capabilities"], result
        # The notice alone, with verified cleanup, remains NOT_RUN.
        report = sweep([local_notice], "complete")
        result = row(report, "local-command")
        assert report["decision"] == "INCOMPLETE" and result["outcome"] == "NOT_RUN", report
        assert result["assertions"] == [], result
        assert result["exit_code"] == 0 and result["cleanup_verified"] is True, result
        assert notice in result["missing_capabilities"], result
        print(f"qa local-command cleanup rows: {time.perf_counter() - local_started:.3f}s", flush=True)
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
        report = sweep([builtin], "builtin-skip", ["--build-candidate"])
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
                     "ambiguous", "truncated", "seatbelt", "ps", "unstable", "wrong-revision", "missing-status",
                     "unverified-cleanup"]:
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
            elif mode == "missing-status":
                execution["exit_code"] = None
                execution["outcome"] = "FAIL"
            elif mode == "unverified-cleanup":
                execution["cleanup_verified"] = False
                execution["stderr"] = execution["stderr"] + "detached worker settlement failed: fixture settlement refusal"
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
            for key in ["output_truncated", "source_stable", "source_revision", "cleanup_verified"]:
                if key in execution:
                    recorded[key] = execution[key]
            if mode == "missing-status":
                recorded.update(exit_code=None, outcome="FAIL")
            if mode == "unverified-cleanup":
                recorded["stderr"] = execution["stderr"]
            (repo / "transcript.json").write_text(json.dumps(recorded))
            report = sweep([native], f"native-{mode}")
            assert (report["decision"] == "PASS") == (mode in ["success", "multiple-success", "nocapture", "directory-target"]), (mode, report)
            if mode == "unverified-cleanup":
                assert "cleanup was not verified" in row(report, "platform-evidence-import")["stderr"], (mode, report)
                assert recorded["exit_code"] == 0 and recorded["stdout"], recorded
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
        # Re-import the unchanged producer report with browser execution disabled.
        # Keep the declared local provenance row: it is emitted even when the
        # external fixture binary has no source-build attestation.
        provenance = {"id": "source-binary-provenance", "required": False,
                      "capability": "local", "kind": "builtin", "assertions": []}
        report = sweep([*browsers, provenance], "complete", browser_args)
        imported_path = repo / ".orbit/tmp/reimport.json"
        def reimport(body, scenarios):
            imported_path.write_text(json.dumps(body))
            return sweep([*scenarios, provenance], "complete",
                         ["--platform-evidence", str(imported_path)])

        replay = reimport(report, browsers)
        assert replay["decision"] == "PASS", replay
        assert all(row(replay, name).get("imported_from") for name in ["first", "second"]), replay
        for mode in ["candidate", "row-candidate", "unknown", "command", "module-command", "directory-command", "assertions",
                     "missing-manifest", "empty-manifest", "missing-bytes", "tampered-bytes"]:
            altered = copy.deepcopy(report)
            result = row(altered, "first")
            if mode == "candidate":
                altered["candidate"]["id"] = "stale-candidate"
            elif mode == "row-candidate":
                result["candidate_id"] = "stale-candidate"
            elif mode == "unknown":
                altered["results"].append({**result, "scenario": "undeclared-behavior"})
            elif mode == "command":
                result["command"][2] = "different-behavior"
            elif mode == "module-command":
                result["command"][-1] = "different-browser-module"
            elif mode == "directory-command":
                result["command"][-2] = "different-evidence-directory"
            elif mode == "assertions":
                result["assertions"] = ["different-guarantee"]
            elif mode == "missing-manifest":
                result.pop("retained_evidence")
            elif mode == "empty-manifest":
                result["retained_evidence"]["files"] = []
            else:
                retained = result["retained_evidence"]
                target = Path(retained["directory"]) / retained["files"][0]["path"]
                original = target.read_bytes()
                if mode == "missing-bytes":
                    target.unlink()
                else:
                    target.write_text("tampered retained bytes")
            replay = reimport(altered, browsers)
            assert replay["decision"] == "INCOMPLETE", (mode, replay)
            assert not any(item["outcome"] == "PASS" and item["scenario"] == "first"
                           for item in replay["results"]), (mode, replay)
            if mode in ["missing-bytes", "tampered-bytes"]:
                target.write_bytes(original)
        # Native schema-v2 rows retain the schema-v1 provenance validation.
        native_report = sweep([native, provenance], "hosted-complete")
        replay = reimport(native_report, [native])
        assert replay["decision"] == "PASS" and row(replay, "macos-platform")["imported_from"], replay
        for mode in ["wrong-command", "missing-provenance"]:
            altered = copy.deepcopy(native_report)
            result = row(altered, "macos-platform")
            if mode == "wrong-command":
                result["command"] = ["./wrong-command"]
            else:
                result["native_evidence"]["producer"]["run_id"] = ""
            replay = reimport(altered, [native])
            assert replay["decision"] == "INCOMPLETE", (mode, replay)
        for mode in ["overwrite", "missing"]:
            report = sweep(browsers, mode, browser_args)
            assert report["decision"] == "INCOMPLETE" and row(report, "first")["retained_evidence_error"], report
        report = sweep(browsers, "seatbelt", browser_args)
        assert all(row(report, name)["outcome"] == "NOT_RUN" for name in ["first", "second"]), report
        # Repeated invocations also retain earlier report bytes.
        assert all(Path(directory).is_dir() for directory in directories), directories
        # UX measurement selection: a Node exit alone is not measurement
        # evidence. Only an executed (not skipped/missing/failed) required
        # browser case earns PASS; absent kit stays NOT_RUN.
        (repo / "ux-fixture.mjs").write_text(
            "import { readFileSync } from 'node:fs';\n"
            "import test from 'node:test';\n"
            "const mode = JSON.parse(readFileSync('mode.json', 'utf8'));\n"
            "test('mock case runs without a browser', () => {});\n"
            "if (mode === 'ux-executed') test('ux browser case executes', () => {});\n"
            "else if (mode === 'ux-skipped') test('ux browser case executes', { skip: true }, () => {});\n"
            "else if (mode === 'ux-missing') test('renamed browser case', () => {});\n"
            "else if (mode === 'ux-failed') test('ux browser case executes', () => { throw new Error('genuine failure'); });\n"
            "else if (mode === 'ux-launch-failed') test('ux browser case executes', (t) => { t.skip('Chromium launch failed'); });\n"
            "else if (mode === 'ux-truncated') {\n"
            "  test('ux browser case executes', { skip: true }, () => {});\n"
            "  test('large diagnostics', () => console.log('x'.repeat(1_048_576 + 64)));\n"
            "}\n")
        ux = {"id": "ux-measurement-browser", "required": True, "capability": "browser", "kind": "command",
              "command": ["node", "--test", "--test-reporter=tap", "ux-fixture.mjs"],
              "assertions": ["ux-browser-case-executed"], "required_cases": ["ux browser case executes"],
              "behavior": ["normal", "failure"]}
        report = sweep([ux], "ux-executed", browser_args)
        ux_row = row(report, "ux-measurement-browser")
        assert report["decision"] == "PASS" and ux_row["outcome"] == "PASS", report
        assert ux_row["executed_cases"] == ["mock case runs without a browser", "ux browser case executes"], ux_row
        assert ux_row["retained_evidence"]["files"], ux_row
        for mode, expected in [("ux-skipped", "NOT_RUN"), ("ux-launch-failed", "NOT_RUN"),
                               ("ux-missing", "FAIL"), ("ux-failed", "FAIL"), ("ux-truncated", "FAIL")]:
            report = sweep([ux], mode, browser_args)
            ux_row = row(report, "ux-measurement-browser")
            assert report["decision"] == "INCOMPLETE" and ux_row["outcome"] == expected, (mode, ux_row)
            assert not ux_row["assertions"], (mode, ux_row)
            if mode in ("ux-skipped", "ux-launch-failed"):
                assert ux_row["skipped_cases"] == ["ux browser case executes"], (mode, ux_row)
                assert ux_row["missing_capabilities"], (mode, ux_row)
            if mode == "ux-missing":
                assert "did not execute the required browser cases" in ux_row["stderr"], ux_row
            if mode == "ux-failed":
                assert ux_row["exit_code"] != 0 and ux_row["failed_cases"] == ["ux browser case executes"], ux_row
            if mode == "ux-truncated":
                assert ux_row["exit_code"] == 0 and ux_row["output_truncated"], ux_row
                assert "command output was truncated" in ux_row["stderr"], ux_row
        # A capability skip cannot hide a second required case that disappeared.
        report = sweep([{**ux, "required_cases": ["ux browser case executes", "absent browser case"]}],
                       "ux-skipped", browser_args)
        ux_row = row(report, "ux-measurement-browser")
        assert report["decision"] == "INCOMPLETE" and ux_row["outcome"] == "FAIL", report
        assert ux_row["exit_code"] == 0 and not ux_row["assertions"], ux_row
        assert "did not execute the required browser cases: ['absent browser case']" in ux_row["stderr"], ux_row
        assert ux_row["skipped_cases"] == ["ux browser case executes"], ux_row
        retained = Path(ux_row["retained_evidence"]["directory"]) / "harness-result.json"
        raw = json.loads(retained.read_text())
        assert raw["exit_code"] == 0 and raw["stdout"] == ux_row["stdout"], raw
        assert "ux browser case executes # SKIP" in raw["stdout"], raw
        report = sweep([ux], "ux-executed")
        assert row(report, "ux-measurement-browser")["outcome"] == "NOT_RUN", report
        assert report["decision"] == "INCOMPLETE", report



def builtin_refusal_self_test(harness, scratch, wire=False, cases=None, nested=False):
    """Public main rejects incomplete refusals and fabricated wire/index evidence."""
    with tempfile.TemporaryDirectory(prefix="qa-builtin-refusal-", dir=scratch) as tmp:
        repo = Path(tmp)
        for directory in ["bin", "scripts", ".orbit/tmp"]:
            (repo / directory).mkdir(parents=True, exist_ok=True)
        driver = repo / "driver.py"
        hook = """
from unittest import mock
real_run = qa.run
mode = json.loads((repo / "mode.json").read_text())
def injected_run(argv, **kwargs):
    if mode.startswith("builtin-probe-"):
        capability = ("sandbox-apply" if argv[0] == "/usr/bin/sandbox-exec" else
                      "protected-fixture-write" if len(argv) > 2 and argv[1] == "-c"
                      and "qa-protected-write-" in argv[2] else None)
        if capability and mode != "builtin-probe-native":
            unavailable = mode == "builtin-probe-unavailable"
            return {"command": argv, "exit_code": (71 if capability == "sandbox-apply" else 77) if unavailable else 0,
                    "stdout": json.dumps({"denied": [{"errno": 1}]} if unavailable else {"denied": []}),
                    "stderr": "sandbox_apply: Operation not permitted" if unavailable and capability == "sandbox-apply" else "",
                    "outcome": "FAIL", "cleanup_verified": True}
    refusal = (("task" in argv and "show" in argv and "--workspace" in argv
                and Path(argv[argv.index("--workspace") + 1]).name == "other-workspace")
               or ("config" in argv and "set" in argv
                   and any(value in argv for value in ["not-a-real-mode", "hm_forged", "ZZ",
                                                       "workspace-scoped", "invalid/name"])))
    if refusal:
        if mode == "builtin-timeout":
            return real_run(argv, **{**kwargs, "timeout": 0})
        if mode == "builtin-lost-result":
            with mock.patch.object(qa, "_PROCESS_SUPERVISOR",
                                   "import os, sys; os.close(int(sys.argv[1]))"):
                return real_run(argv, **kwargs)
        if mode == "builtin-unverified-cleanup":
            with mock.patch("os.killpg", side_effect=PermissionError("fixture cleanup refusal")):
                return real_run(argv, **kwargs)
    return real_run(argv, **kwargs)
qa.run = injected_run
if mode.startswith("builtin-wire-") and mode != "builtin-wire-valid":
    # A wire row varies only `mcp serve` and `search reindex`, which keep the real
    # supervisor and fixture process. The other fixture CLI commands answer in
    # process; the parent compares their results with the fully supervised valid
    # row's, so main, the validators and the finalizer still see the same inputs.
    import contextlib, io
    fixture_code = compile((repo / "bin/orbit").read_text(), "bin/orbit", "exec")
    def answer_in_process(argv, *, cwd, env, timeout=180, input_text=None):
        if (Path(argv[0]).name != "orbit" or "mcp" in argv
                or ("search" in argv and "reindex" in argv)):
            return injected_run(argv, cwd=cwd, env=env, timeout=timeout, input_text=input_text)
        stdout, stderr = io.StringIO(), io.StringIO()
        exit_code = 0
        with (mock.patch.dict("os.environ", env, clear=True), mock.patch("sys.argv", list(argv)),
              contextlib.chdir(cwd), contextlib.redirect_stdout(stdout),
              contextlib.redirect_stderr(stderr)):
            try:
                exec(fixture_code, {"__name__": "__main__"})
            except SystemExit as stop:
                exit_code = stop.code or 0
        return {"command": argv, "exit_code": exit_code, "stdout": stdout.getvalue(),
                "stderr": stderr.getvalue(), "outcome": "FAIL", "output_truncated": False,
                "cleanup_verified": True, "detached_workers": [], "open_descendants": 0,
                "subreaper": False}
    qa.run = answer_in_process
"""
        driver.write_text(DRIVER.replace("qa.main()", hook + "\nqa.main()", 1))
        cargo = repo / "bin/cargo"
        cargo.write_text(f"#!{sys.executable}\n" + CARGO)
        cargo.chmod(0o755)
        git = repo / "bin/git"
        git.write_text(f"#!{sys.executable}\nprint('" + "a" * 40 + "')\n")
        git.chmod(0o755)
        orbit = repo / "bin/orbit"
        orbit.write_text(f"#!{sys.executable}\n" + r"""
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
mode = json.loads(Path(os.environ["QA_REFUSAL_REPO"]).joinpath("mode.json").read_text())
root = Path(args[args.index("--root") + 1]) if "--root" in args else Path.home() / ".orbit"
work = Path(args[args.index("--workspace") + 1]) if "--workspace" in args else Path.cwd()
refusal = (("task" in args and "show" in args and work.name == "other-workspace")
           or ("config" in args and "set" in args and any(value in args for value in
               ["not-a-real-mode", "hm_forged", "ZZ", "workspace-scoped", "invalid/name"])))
if refusal:
    print("fixture refusal", file=sys.stderr)
    raise SystemExit(0 if mode == "builtin-zero" else 7)
body = {}
if args[:2] == ["mcp", "serve"]:
    [json.loads(line) for line in sys.stdin if line.strip()]  # Consume the request stream.
    task = {"id": "QAF-1", "title": "QA behavioral fixture", "acceptance_criteria": ["fixture round trip"]}
    responses = {
        1: {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}, "resources": {}}},
        2: {"tools": [{"name": "orbit_task_show"}]},
        3: {"structuredContent": {"workspaces": [{"name": "qa-primary"}]}},
        4: {"structuredContent": task},
        5: {"isError": True, "structuredContent": {"code": "not_found", "message": "Task QAF-1 not found"}},
    }
    envelopes = {key: {"jsonrpc": "2.0", "id": key, "result": value} for key, value in responses.items()}
    case = mode.removeprefix("builtin-wire-")
    if case == "missing-init":
        del envelopes[1]
    elif case in ("init-error", "task-rpc-error", "cross-rpc-refusal"):
        key = 1 if case == "init-error" else 4 if case == "task-rpc-error" else 5
        envelopes[key] = {"jsonrpc": "2.0", "id": key, "error": {"code": -32602, "message": "QAF-1 refused"}}
    elif case == "task-error":
        responses[4].update(responses[5])
    elif case == "task-missing":
        responses[4] = {"content": [{"type": "text", "text": "QAF-1"}]}
        envelopes[4]["result"] = responses[4]
    elif case in ("wrong-id", "wrong-title", "wrong-criteria"):
        task[{"wrong-id": "id", "wrong-title": "title", "wrong-criteria": "acceptance_criteria"}[case]] = "wrong QAF-1"
    elif case == "bad-version":
        responses[1]["protocolVersion"] = "1900-01-01"
    elif case == "missing-capabilities":
        del responses[1]["capabilities"]
    elif case == "bad-capabilities":
        responses[1]["capabilities"] = {"tools": None}
    elif case == "bad-jsonrpc":
        envelopes[1]["jsonrpc"] = "1.0"
    elif case == "task-bad-jsonrpc":
        envelopes[4]["jsonrpc"] = "1.0"
    elif case == "duplicate-init":
        print(json.dumps(envelopes[1]))
    elif case == "cross-success":
        responses[5].pop("isError")
    elif case == "wrong-init-id":
        envelopes[1]["id"] = 99
    elif case == "both-result-error":
        envelopes[1]["error"] = {"code": -32603, "message": "failed"}
    for value in envelopes.values():
        print(json.dumps(value))
    raise SystemExit(0)
if "search" in args and "reindex" in args:
    case = mode.removeprefix("builtin-wire-")
    body = {"chunks": 3, "tasks": 1}
    if case == "empty-reindex": body = {}
    elif case == "unrelated-reindex": body = {"unrelated": 1}
    elif case == "zero-reindex": body = {"chunks": 0, "tasks": 0}
    elif case == "boolean-reindex": body = {"chunks": True, "tasks": True}
    elif case == "string-reindex": body = {"chunks": "3", "tasks": "1"}
    elif case == "empty-stdout": raise SystemExit(0)
    print(json.dumps(body))
    raise SystemExit(0)
if "init" in args and "workspace" not in args:
    if root.name == "migration-root" and not mode.startswith(("builtin-wire-", "builtin-probe-")):
        raise SystemExit(1)  # Stop after all shared refusal callers under test.
    root.mkdir(parents=True, exist_ok=True)
    (root / "config.toml").write_text("identity")
    (root / "workspaces.json").write_text("registry")
elif "workspace" in args and "show" in args:
    body = {"checkout": {"repo_root": str(work)}}
elif "task" in args:
    if "artifact" in args and "get" in args:
        print("qa evidence payload")
        raise SystemExit(0)
    body = {"id": "QAF-1", "title": "QA behavioral fixture", "acceptance_criteria": ["fixture round trip"]}
    if "list" in args:
        body = [body]
elif "config" in args:
    if "set" in args and "workflow.base_branch" in args:
        path = work / ".orbit/config.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("base_branch = 'qa-candidate'")
    elif "get" in args:
        body = {"key": "workflow.base_branch", "value": "qa-candidate", "scope": "workspace",
                "path": str(work / ".orbit/config.toml")}
    elif "set" in args and "qa-renamed" in args:
        (root / "renamed").write_text("renamed")
    elif "show" in args:
        body = {"settings": {"machine.id": "hm_fixture", "machine.task_prefix": "QAF",
                             "machine.name": "qa-renamed" if (root / "renamed").exists() else "qa-machine"}}
elif "search" in args:
    body = {"id": "QAF-1", "title": "QA behavioral fixture"}
elif "friction" in args:
    body = {"description": "qa isolated friction marker"}
elif "recommend" in args or "qashapes.recommend" in args:
    print("sandbox_apply: Operation not permitted", file=sys.stderr)
    raise SystemExit(71)
elif "fs-access" in args:
    body = {"read": {"allowed": True}, "modify": {"allowed": False}}
elif "providers" in args:
    body = [{"sandbox": "fixture", "cli_available": True}]
elif "log" in args:
    if "orbit.qa.absent" not in args:
        print(Path(args[args.index("--path") + 1]).read_text().splitlines()[-1])
    raise SystemExit(0)
print(json.dumps(body))
""")
        orbit.write_text(orbit.read_text().replace('os.environ["QA_REFUSAL_REPO"]', repr(str(repo))))
        orbit.chmod(0o755)
        (repo / "scripts/test-qa-full-sweep.py").write_bytes(harness.read_bytes())
        inventory = json.loads(harness.with_name("qa-full-sweep-inventory.json").read_text())
        names = ({"mcp-wire-boundary"} if wire else
                 {"workspace-boundary", "config-transaction", "machine-identity-transaction"})
        scenarios = [item for item in inventory["scenarios"] if item["id"] in names]
        if nested:
            names = {"definition-policy-boundary", "distributed-drain-owner-roundtrip", "workflow-process-boundary"}
            scenarios = [item for item in inventory["scenarios"] if item["id"] == "definition-policy-boundary"]
            scenarios.extend({"id": name, "required": True, "kind": "cargo-test", "capability": "local",
                              "command": ["cargo", "test", "-p", "fixture", "--test", "boundary", "boundary::"],
                              "assertions": ["original-boundary"], "required_tests": ["boundary::contracts"],
                              "required_rows": ["original_boundary"]}
                             for name in sorted(names - {"definition-policy-boundary"}))
        (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps({"scenarios": scenarios}))
        if wire:
            # Keep unrelated built-ins out of the inventory while executing the
            # real main -> run_builtins -> finalizer -> scenario decision path.
            search = next(item for item in inventory["scenarios"] if item["id"] == "search-observability-boundary")
            scenarios.append(search)
            (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps({"scenarios": scenarios}))
        modes = (["wire-" + case for case in [
            "valid", "cross-rpc-refusal", "task-error", "task-rpc-error", "task-missing",
            "wrong-id", "wrong-title", "wrong-criteria", "missing-init", "init-error",
            "bad-version", "missing-capabilities", "bad-capabilities", "bad-jsonrpc",
            "task-bad-jsonrpc", "duplicate-init", "wrong-init-id", "both-result-error",
            "cross-success", "empty-reindex", "unrelated-reindex", "zero-reindex",
            "boolean-reindex", "string-reindex", "empty-stdout"]] if wire else
            ["complete", "timeout", "lost-result", "unverified-cleanup", "zero"])
        valid_unrelated = None
        if nested:
            modes = ["probe-unavailable", "probe-available-eperm"]
            if os.environ.get("QA_SWEEP_NATIVE_PROBES") == "1":
                modes = ["probe-native"]
        for mode in (modes if cases is None else ["wire-" + case for case in cases]):
            (repo / "mode.json").write_text(json.dumps("builtin-" + mode))
            child = subprocess.run([sys.executable, str(driver), str(harness), str(repo), "--build-candidate",
                                    *(["--run-commands"] if nested else [])],
                                   cwd=repo, env={**os.environ,
                                                 "PATH": f"{cargo.parent}:{os.environ['PATH']}"},
                                   capture_output=True, text=True, timeout=60)
            assert child.returncode == 0, (mode, child.stdout, child.stderr)
            report = json.loads((repo / ".orbit/tmp/report.json").read_text())
            if nested:
                unavailable = mode != "probe-available-eperm"
                rows = [item for item in report["results"] if item["scenario"] in names]
                assert {item["scenario"] for item in rows} == names, report
                assert report["decision"] == "INCOMPLETE", report
                assert all(item["outcome"] == ("NOT_RUN" if unavailable else "FAIL")
                           for item in rows if unavailable or "capability_probe" in item
                           and ("recommend" in item["command"] or "qashapes.recommend" in item["command"]
                                or item["scenario"] != "definition-policy-boundary")), report
                if unavailable:
                    assert all(not item["assertions"] and item["exit_code"] is None for item in rows), report
                    assert {item["scenario"] for item in report["capability_gaps"]} == names, report
                    assert all(item["capability_probe"]["outcome"] == "NOT_RUN" for item in rows), report
                else:
                    failures = [item for item in rows if item["outcome"] == "FAIL"]
                    assert {item["scenario"] for item in failures} == names, report
                    assert all(item["capability_probe"]["outcome"] == "PASS" for item in failures), report
                    assert all("Operation not permitted" in item["stderr"] for item in failures), report
                    assert not report["capability_gaps"], report
                if destination := os.environ.get("QA_SWEEP_PROBE_REPORT"):
                    Path(destination).write_text(json.dumps(report, indent=2) + "\n")
                print("nested-sandbox: " + mode + " passed", flush=True)
                continue
            if wire:
                case = mode.removeprefix("wire-")
                reindex_bad = case in {"empty-reindex", "unrelated-reindex", "zero-reindex", "boolean-reindex", "string-reindex", "empty-stdout"}
                mcp_good = case in {"valid", "cross-rpc-refusal"} or reindex_bad
                mcp = next(item for item in report["results"] if item["scenario"] == "mcp-wire-boundary")
                search = next(item for item in report["results"] if item["scenario"] == "search-observability-boundary"
                              and "reindex" in item.get("command", []))
                assert (mcp["outcome"] == "PASS") == mcp_good, (case, mcp)
                assert bool(mcp["assertions"]) == mcp_good, (case, mcp)
                assert (search["outcome"] == "PASS") != reindex_bad, (case, search)
                assert bool(search["assertions"]) != reindex_bad, (case, search)
                assert (report["decision"] == "PASS") == (case in {"valid", "cross-rpc-refusal"}), (case, report)
                unrelated = [(item["scenario"], item["outcome"], item["exit_code"], item["assertions"],
                              item.get("cleanup_verified")) for item in report["results"]
                             if item["scenario"] != "mcp-wire-boundary" and "reindex" not in item.get("command", [])]
                valid_unrelated = valid_unrelated or unrelated
                assert unrelated == valid_unrelated, (case, unrelated, valid_unrelated)
                print("builtin-wire: " + case + " passed", flush=True)
                continue
            refusals = [item for item in report["results"] if "fixture refusal" in item.get("stderr", "")
                        or item.get("exit_code") is None and item["scenario"] in names]
            assert len(refusals) == 6, (mode, report)
            assert (report["decision"] == "PASS") == (mode == "complete"), (mode, report)
            for result in refusals:
                assert result["outcome"] == ("PASS" if mode == "complete" else "FAIL"), (mode, result)
                assert bool(result["assertions"]) == (mode == "complete"), (mode, result)
                assert result["exit_code"] == (7 if mode == "complete" else 0 if mode == "zero" else None), (mode, result)
            positives = [item for item in report["results"] if item["scenario"] in names and item not in refusals]
            assert positives and all(item["outcome"] == "PASS" and item["assertions"] for item in positives), (mode, positives)

def candidate_build_report_self_test(harness, scratch):
    """Failed builds and unavailable binaries retain reports without running built-ins."""
    with tempfile.TemporaryDirectory(prefix="qa-candidate-build-", dir=scratch) as tmp:
        repo = Path(tmp)
        for directory in ["bin", "scripts", ".orbit/tmp"]:
            (repo / directory).mkdir(parents=True, exist_ok=True)
        driver = repo / "driver.py"
        driver.write_text(DRIVER.replace("qa.main()", '''
def builtins(*args):
    (repo / "builtin-invoked").write_text("ran")
    return ([{"scenario": "fixture-builtin", "outcome": "PASS",
              "assertions": ["fixture-builtins-ran"], "candidate_id": "fixture-candidate"}], None)
qa.run_builtins = builtins
qa.main()
''', 1))
        cargo = repo / "bin/cargo"
        cargo.write_text(f"#!{sys.executable}\n" + CARGO)
        cargo.chmod(0o755)
        orbit = repo / "bin/orbit"
        orbit.write_text(f"#!{sys.executable}\nprint('fixture orbit')\n")
        orbit.chmod(0o755)
        (repo / "scripts/test-qa-full-sweep.py").write_bytes(harness.read_bytes())
        assertions = ["binary-built-from-current-source", "source-stable-through-build", "binary-hash-recorded"]
        inventory = {"scenarios": [
            {"id": "source-binary-provenance", "kind": "builtin", "capability": "local",
             "required": True, "assertions": assertions},
            {"id": "fixture-builtin", "kind": "builtin", "capability": "local",
             "required": True, "assertions": ["fixture-builtins-ran"]},
        ]}
        (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps(inventory))
        for mode, options in [
            ("build-failed", ["--build-candidate"]),
            ("build-failed-with-binary", ["--build-candidate"]),
            ("external-missing", ["--orbit-bin", str(repo / "missing-orbit")]),
            ("external-unverified", []),
            ("complete", ["--build-candidate"]),
        ]:
            (repo / "mode.json").write_text(json.dumps(mode))
            # Distinct outputs ensure an earlier report cannot hide a crash.
            output = repo / f".orbit/tmp/{mode}.json"
            child = subprocess.run(
                [sys.executable, str(driver), str(harness), str(repo), "--output", str(output), *options],
                cwd=repo, env={**os.environ, "PATH": f"{cargo.parent}:{os.environ['PATH']}",
                               "TMPDIR": str(repo / ".orbit/tmp")},
                capture_output=True, text=True, timeout=60)
            assert output.is_file(), (mode, child.returncode, child.stdout, child.stderr)
            report = json.loads(output.read_text())
            assert child.returncode == 0, (mode, child.stderr)
            provenance = next(row for row in report["results"] if row["scenario"] == "source-binary-provenance")
            builtin = next(row for row in report["results"] if row["scenario"] == "fixture-builtin")
            if mode == "complete":
                assert report["decision"] == "PASS" and provenance["outcome"] == "PASS", report
                assert provenance["assertions"] == assertions, provenance
                assert report["binary"]["sha256"] == hashlib.sha256(orbit.read_bytes()).hexdigest(), report
                assert report["binary"]["version"] == "fixture orbit", report
                assert builtin["outcome"] == "PASS" and (repo / "builtin-invoked").is_file(), report
            else:
                assert report["decision"] == "INCOMPLETE" and provenance["outcome"] == "FAIL", report
                assert not provenance["assertions"], provenance
                assert builtin["outcome"] == "NOT_RUN" and not (repo / "builtin-invoked").exists(), report
                if mode.startswith("build-failed"):
                    command = provenance["command"]
                    assert command[:-1] == ["cargo", "build", "--locked", "-p", "orbit-cli", "--bin", "orbit", "--target-dir"], command
                    assert command[-1].endswith("/candidate-target"), command
                    assert provenance["exit_code"] == 42, provenance
                    assert provenance["stdout"] == "fixture candidate build stdout\n", provenance
                    assert provenance["stderr"].startswith("fixture candidate build stderr\n"), provenance
                    assert report["binary"]["sha256"] is None, report
                elif mode == "external-missing":
                    assert provenance["exit_code"] == 127 and report["binary"]["sha256"] is None, report


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


SOURCE_MUTATION = '''
import os, sys
kind = sys.argv[1]
if kind == "chmod":
    os.chmod("probe.sh", 0o755)
elif kind == "new-dangling":
    os.symlink("missing-target", "fresh-dangling")
elif kind == "dangling-retarget":
    os.unlink("dangling")
    os.symlink("gone-2", "dangling")
elif kind == "retarget":
    os.unlink("link")
    os.symlink("b.txt", "link")
elif kind == "file-to-symlink":
    os.unlink("copy.txt")
    os.symlink("a.txt", "copy.txt")
elif kind == "excluded-output":
    os.makedirs("out/report.browser-evidence", exist_ok=True)
    artifact = "out/report.browser-evidence/artifact.sh"
    open(artifact, "w").write("echo evidence\\n")
    os.chmod(artifact, 0o755)
print("mutated " + kind)
'''


def source_identity_self_test(harness, scratch):
    """Untracked mode, entry kind and symlink text bind the candidate through the public sweep.

    Each mutation runs inside the scenario (or its retired-case command) of a
    real sweep over a disposable Git checkout, using the real `candidate_source`.
    The same-byte `link` retarget and the file-to-symlink swap keep every
    followed byte identical, so only the entry's own identity can fail them.
    """
    stub = ('qa.candidate_source = lambda *_: {"candidate_id": "fixture-candidate", "head": "a" * 40,\n'
            '                                "status": [], "diff_sha256": "fixture"}\n')
    assert stub in DRIVER, "driver no longer stubs candidate_source"
    with tempfile.TemporaryDirectory(prefix="qa-source-identity-", dir=scratch) as tmp:
        root = Path(tmp)
        repo = root / "repo"
        for directory in ["bin", "scripts", ".orbit/tmp"]:
            (repo / directory).mkdir(parents=True)
        driver = root / "driver.py"
        driver.write_text(DRIVER.replace(stub, ""))
        mutation = root / "mutate.py"
        mutation.write_text(SOURCE_MUTATION)
        orbit = repo / "bin/orbit"
        orbit.write_text(f"#!{sys.executable}\nprint('fixture orbit')\n")
        orbit.chmod(0o755)
        (repo / "mode.json").write_text(json.dumps("complete"))
        (repo / ".gitignore").write_text(".orbit/\nbin/\nmode.json\n")
        (repo / "scripts/test-qa-full-sweep.py").write_bytes(harness.read_bytes())
        (repo / "scripts/qa-full-sweep-inventory.json").write_text("{}")

        def git(*argv):
            subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                            *argv], cwd=repo, check=True, capture_output=True)

        git("init", "-q")
        git("add", "-A")
        git("commit", "-q", "-m", "baseline")

        def reset_untracked_inputs():
            git("clean", "-fdq")
            (repo / "probe.sh").write_text("echo probe\\n")
            (repo / "probe.sh").chmod(0o644)
            for name in ["a.txt", "b.txt", "copy.txt"]:
                (repo / name).write_text("same bytes\\n")
            for name, target in [("link", "a.txt"), ("dangling", "gone-1")]:
                (repo / name).symlink_to(target)

        def sweep(kind, where="scenario"):
            reset_untracked_inputs()
            probe = [sys.executable, str(mutation), kind]
            scenario = {"id": "probe", "required": True, "capability": "local", "kind": "command",
                        "command": probe if where == "scenario" else [sys.executable, "-c", "pass"],
                        "assertions": ["probe-ran"]}
            if where == "retired-case":
                scenario["retired_cases"] = [{
                    "retired": "old::probe", "guarantee": "Probe still runs",
                    "disposition": "verified-equivalent", "reason": "The command mutates its inputs",
                    "command": probe}]
            (repo / "scripts/qa-full-sweep-inventory.json").write_text(json.dumps({
                "surface_contracts": {"cli_top_level": [], "job_assets": [], "sha256": {}},
                "feature_families": [], "scenarios": [scenario], "required_capability_gaps": {}}))
            report_path = repo / "out/report.json"
            report_path.parent.mkdir(exist_ok=True)
            child = subprocess.run([sys.executable, str(driver), str(harness), str(repo), "--run-commands",
                            "--output", str(report_path)],
                           cwd=repo, env={**os.environ}, capture_output=True, text=True, timeout=120)
            if not report_path.is_file():
                raise AssertionError(f"sweep did not produce a report: {child.stdout}\n{child.stderr}")
            report = json.loads(report_path.read_text())
            rows = [row for row in report["results"] if row["scenario"] == "probe"]
            return report, rows

        for where in ["scenario", "retired-case"]:
            report, rows = sweep("control", where)
            assert report["decision"] == "PASS" and all(row["outcome"] == "PASS" for row in rows), (where, report)
            report, rows = sweep("excluded-output", where)
            assert report["decision"] == "PASS" and all(row["outcome"] == "PASS" for row in rows), (
                "declared report/evidence output must not change the candidate", where, report)
            for kind in ["chmod", "new-dangling", "dangling-retarget", "retarget", "file-to-symlink"]:
                report, rows = sweep(kind, where)
                changed = [row for row in rows if row["outcome"] == "FAIL"
                           and "source candidate changed while the scenario ran" in row["stderr"]]
                assert changed and report["decision"] != "PASS", (kind, where, report)


def native_emitter_cleanup_self_test(harness, scratch):
    """Hosted emission refuses unverified cleanup and keeps the raw record.

    scripts/check-ci-macos.sh loads the harness with runpy and emits PASS only
    after validate_native_execution returns. This is that call. Import uses
    the same verifier. Exit 0 with missing or false cleanup is a refusal.
    Failed, zero-test, self-skip and truncated records still fail on their
    own guards, and the retained exit code and stdout are not rewritten.
    """
    with tempfile.TemporaryDirectory(prefix="qa-emitter-cleanup-", dir=scratch) as tmp:
        repo = Path(tmp)
        (repo / ".github/workflows").mkdir(parents=True)
        command = ["cargo", "test", "-p", "native", "--lib"]
        workflow = "python3 scripts/test-qa-full-sweep.py --record-native-command " + " ".join(command) + "\n"
        (repo / ".github/workflows/ci-macos.yml").write_text(workflow)
        head = "a" * 40
        passing = (
            "running 1 test\n"
            "test native_boundary ... ok\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n")
        announced = "     Running unittests src/lib.rs (target/debug/deps/native)\n"
        base = {
            "command": command,
            "executed_command": [*command, "--", "--show-output"],
            "source_revision": head, "source_status": [], "source_stable": True,
            "exit_code": 0, "cleanup_verified": True, "stdout": passing, "stderr": announced,
        }
        loaded = runpy.run_path(str(harness))

        def emit(record):
            try:
                loaded["validate_native_execution"]([dict(record)], repo, head)
            except (ValueError, KeyError, TypeError) as error:
                return 1, "check-ci-macos: incomplete native execution: " + str(error)
            return 0, "PASS"

        status, message = emit(base)
        assert status == 0 and message == "PASS", (status, message)
        evidence_path = repo / "execution.json"
        unverified = dict(base, cleanup_verified=False, stderr=announced + (
            "detached worker settlement failed: fixture settlement refusal\n"))
        evidence_path.write_text(json.dumps(unverified))
        retained = json.loads(evidence_path.read_text())
        status, message = emit(retained)
        assert status == 1 and "cleanup was not verified" in message, (status, message)
        assert retained["exit_code"] == 0 and retained["stdout"] == passing, retained
        on_disk = json.loads(evidence_path.read_text())
        assert on_disk["exit_code"] == 0 and on_disk["stdout"] == passing, on_disk
        missing = dict(base)
        missing.pop("cleanup_verified")
        status, message = emit(missing)
        assert status == 1 and "cleanup was not verified" in message, (status, message)
        failed = dict(base, exit_code=101)
        status, message = emit(failed)
        assert status == 1 and "failed or names a different revision" in message, (status, message)
        assert "cleanup was not verified" not in message, message
        zero = dict(base, stdout=(
            "running 0 tests\n"
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"))
        status, message = emit(zero)
        assert status == 1 and "zero passing tests" in message, (status, message)
        skipped = dict(base, stderr=announced + "SKIP: sandbox-exec cannot apply a profile: Operation not permitted\n")
        status, message = emit(skipped)
        assert status == 1 and "self-skipped" in message, (status, message)
        truncated = dict(base, output_truncated=True)
        status, message = emit(truncated)
        assert status == 1 and "truncated" in message, (status, message)


def native_recorder_prefix_self_test(harness, scratch):
    """Exercise recorder arguments, shell status and retained execution evidence."""
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

        driver = repo / "status-driver.py"
        driver.write_text('''
import importlib.util, json, os, signal, sys, time
from pathlib import Path
from unittest import mock
spec = importlib.util.spec_from_file_location("qa", sys.argv[1])
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
mode = sys.argv[2]
real_run = qa.run
def injected_run(*args, **kwargs):
    if mode == "timeout":
        kwargs["timeout"] = 0
    if mode == "lost-result":
        # Close the result pipe without a status; no command child is started.
        with mock.patch.object(qa, "_PROCESS_SUPERVISOR",
                               "import os, sys; os.close(int(sys.argv[1]))"):
            return real_run(*args, **kwargs)
    if mode in ("unreadable-result", "unfinished-result", "incomplete-result", "invalid-workers"):
        payload = {"exit_code": 0, "detached_workers": [], "open_descendants": 0, "subreaper": False}
        if mode == "incomplete-result":
            payload.pop("open_descendants")
        if mode == "invalid-workers":
            payload["detached_workers"] = None
        raw = "{broken}\\n" if mode == "unreadable-result" else json.dumps(payload)
        if mode != "unfinished-result" and mode != "unreadable-result":
            raw += "\\n"
        program = "import os, sys; os.write(int(sys.argv[1]), " + repr(raw.encode()) + "); os.close(int(sys.argv[1]))"
        with mock.patch.object(qa, "_PROCESS_SUPERVISOR", program):
            return real_run(*args, **kwargs)
    if mode in ("missing-result-timeout", "delayed-result"):
        # A real no-command supervisor waits for owner cancellation. Model a
        # record available after the old ten-second result window, without
        # wall-clock delay or live escaped workers. The public recorder still
        # performs its actual group sweep, wait and empty cwd settlement.
        ready = Path(sys.argv[sys.argv.index("--repo-root") + 1]) / (mode + "-ready")
        program = ("import json, os, signal, sys; from pathlib import Path; "
                   "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                   "Path(" + repr(str(ready)) + ").touch(); "
                   "print('settlement stdout', flush=True); "
                   "os.read(int(sys.argv[2]), 1); ")
        if mode == "delayed-result":
            program += ("os.write(int(sys.argv[1]), "
                        "json.dumps(dict(exit_code=0, detached_workers=[], open_descendants=0, subreaper=False)).encode() + bytes([10])); ")
        program += "os.close(int(sys.argv[1]))"
        real_select = qa.select.select
        calls = 0
        def settlement_select(read, write, error, timeout):
            nonlocal calls
            calls += 1
            if calls == 1:
                deadline = time.monotonic() + 5
                while not ready.exists():
                    assert time.monotonic() < deadline, "fixture supervisor did not become ready"
                    time.sleep(.01)
                return [], [], []
            if mode == "delayed-result" and timeout < 16:
                return [], [], []
            return real_select(read, write, error, timeout)
        with mock.patch.object(qa, "_PROCESS_SUPERVISOR", program), mock.patch.object(qa.select, "select", settlement_select):
            return real_run(*args, **{**kwargs, "timeout": 0})
    if mode == "unverified-cleanup":
        # Closing the owner pipe still lets the supervisor sweep and join its
        # group. The caller cannot verify its own sweep and must retain FAIL.
        with mock.patch("os.killpg", side_effect=PermissionError("fixture cleanup refusal")):
            return real_run(*args, **kwargs)
    if mode == "unverified-settlement":
        # The child exits 0. Settlement then fails, so cleanup is unverified
        # while the raw exit and output stay on the record.
        repo = Path(sys.argv[sys.argv.index("--repo-root") + 1])
        settle_root = repo / "settle-tmp"
        settle_root.mkdir(parents=True, exist_ok=True)
        os.environ["TMPDIR"] = str(settle_root)
        if kwargs.get("env") is not None:
            kwargs["env"]["TMPDIR"] = str(settle_root)
        with mock.patch.object(qa, "settle_detached_workers_under",
                                side_effect=OSError("fixture settlement refusal")):
            return real_run(*args, **kwargs)
    return real_run(*args, **kwargs)
qa.run = injected_run
sys.argv = [sys.argv[1], *sys.argv[3:]]
qa.main()
''')
        for mode, code, expected_status in [
            ("numeric-zero", 0, 0), ("numeric-seven", 7, 7), ("numeric-127", 127, 127),
            ("timeout", 0, 1), ("lost-result", 0, 1), ("unverified-cleanup", 0, 1),
            ("unverified-settlement", 0, 1),
            ("unreadable-result", 0, 1), ("unfinished-result", 0, 1),
            ("incomplete-result", 0, 1), ("invalid-workers", 0, 1),
            ("missing-result-timeout", 0, 1), ("delayed-result", 0, 1),
        ]:
            print(f"native recorder status regression: {mode}", flush=True)
            command = [sys.executable, "-c",
                       "import sys; print('retained stdout'); print('retained stderr', file=sys.stderr); "
                       f"raise SystemExit({code})", "cargo", "test"]
            records = repo / mode
            child = subprocess.run(
                [sys.executable, str(driver), str(harness), mode, "--repo-root", str(repo),
                 "--native-execution-dir", str(records), "--record-native-command", *command],
                cwd=repo, env=env, capture_output=True, text=True, timeout=60)
            assert list(records.glob("*.json")), (mode, child.returncode, child.stdout, child.stderr)
            recorded = json.loads(next(records.glob("*.json")).read_text())
            assert child.returncode == expected_status, (mode, child.returncode, recorded)
            expected_code = (0 if mode in ("unverified-settlement", "incomplete-result", "invalid-workers")
                             else code if mode.startswith("numeric-") else None)
            assert recorded["exit_code"] == expected_code, (mode, recorded)
            expect_pass = mode == "numeric-zero"
            assert recorded["outcome"] == ("PASS" if expect_pass else "FAIL"), (mode, recorded)
            assert child.stdout == recorded["stdout"] and child.stderr == recorded["stderr"], (mode, recorded)
            assert recorded["command"] == command, (mode, recorded)
            if mode.startswith("numeric-") or mode in ("unverified-cleanup", "unverified-settlement"):
                assert recorded["stdout"] == "retained stdout\n", (mode, recorded)
                assert recorded["stderr"].startswith("retained stderr\n"), (mode, recorded)
            if mode == "timeout":
                assert "timeout after 0s" in recorded["stderr"], recorded
            elif mode == "lost-result":
                assert "supervisor exited without a command result" in recorded["stderr"], recorded
                assert recorded["cleanup_verified"] is False, recorded
            elif mode == "unverified-cleanup":
                assert not recorded["cleanup_verified"] and "fixture cleanup refusal" in recorded["stderr"], recorded
            elif mode == "unverified-settlement":
                assert recorded["cleanup_verified"] is False, recorded
                assert "fixture settlement refusal" in recorded["stderr"], recorded
                assert recorded["exit_code"] == 0 and recorded["stdout"] == "retained stdout\n", recorded
            elif mode == "delayed-result":
                assert recorded["cleanup_verified"] is True, recorded
                assert recorded["stdout"] == "settlement stdout\n", recorded
            elif mode in ("unreadable-result", "unfinished-result", "incomplete-result", "invalid-workers",
                          "missing-result-timeout"):
                assert recorded["cleanup_verified"] is False, recorded
                assert recorded["open_descendants"] is None and recorded["detached_workers"] is None, recorded
                assert "supervisor cleanup record" in recorded["stderr"], recorded
