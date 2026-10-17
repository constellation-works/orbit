#!/usr/bin/env python3
"""Exercise affected-crate test selection and execution at the script boundary."""

import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import unittest


SCRIPTS = Path(__file__).resolve().parent


def assert_phase_sums(testcase, summary):
    """compile_ms and test_ms equal the sum of the per-package values that were measured."""
    entries = summary["package_timings"]
    if entries is None:
        testcase.assertIsNone(summary["compile_ms"])
        testcase.assertIsNone(summary["test_ms"])
        return
    testcase.assertIsInstance(entries, list)
    names = [entry["package"] for entry in entries]
    testcase.assertEqual(names, sorted(set(names)))
    for entry in entries:
        testcase.assertEqual(set(entry), {"package", "compile_ms", "test_ms"})
        testcase.assertIsInstance(entry["package"], str)
        testcase.assertTrue(entry["package"])
    for key in ("compile_ms", "test_ms"):
        parts = [entry[key] for entry in entries]
        total = summary[key]
        if total is None:
            testcase.assertTrue(all(part is None for part in parts))
            continue
        testcase.assertIsInstance(total, int)
        testcase.assertGreaterEqual(total, 0)
        if all(part is not None for part in parts):
            testcase.assertEqual(sum(parts), total)
            testcase.assertTrue(all(isinstance(part, int) and part >= 0 for part in parts))
        else:
            testcase.assertTrue(all(part is None for part in parts))


class AffectedTestGateTests(unittest.TestCase):
    """Exercise the gate against real Git diffs and a non-compiling Cargo fixture."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(SCRIPTS / "ci-test-affected.py", self.root / "scripts/ci-test-affected.py")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "cargo.log"
        self.metadata = self.root / "metadata.json"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        GUARD_TEST_LOG=str(self.log), GUARD_TEST_METADATA=str(self.metadata),
                        BUILD_BUDGET=str(self.bin / "build-budget"), CARGO="cargo")
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "CI_TEST_BASE",
                     "ORBIT_VALIDATION_SUMMARY", "ORBIT_VALIDATION_SELECTION", "ORBIT_VALIDATION_GATE"):
            self.env.pop(name, None)
        packages = []
        # Renamed, build and dev edges must all contribute to the transitive
        # reverse closure: engine -> core -> cmd -> web -> cli; tools stays independent.
        dependencies = {
            "orbit-engine": [],
            "orbit-core": [("orbit-engine", None, None)],
            "orbit-cmd": [("orbit-core", None, "renamed_core")],
            "orbit-web": [("orbit-cmd", "build", None)],
            "orbit-cli": [("orbit-web", "dev", None)],
            "orbit-tools": [],
        }
        self.names = sorted(dependencies)
        self.core_dependents = ["orbit-cli", "orbit-cmd", "orbit-core", "orbit-web"]
        for name, edges in dependencies.items():
            crate = self.root / "crates" / name
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n')
            (crate / "src/lib.rs").write_text("// fixture\n")
            packages.append(dict(id=name, name=name, manifest_path=str(crate / "Cargo.toml"),
                                 targets=[dict(name=name.replace("-", "_"), kind=["lib"], doctest=True)],
                                 dependencies=[dict(name=target, kind=kind, rename=rename,
                                                    path=str(self.root / "crates" / target))
                                               for target, kind, rename in edges]))
        self.metadata.write_text(json.dumps(dict(packages=packages, workspace_members=self.names)))
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        (self.root / ".gitignore").write_text("bin/\ncargo.log\nmetadata.json\n.scratch/\n")
        (self.root / "docs").mkdir()
        (self.root / "docs/guide.md").write_text("Fixture docs\n")
        for path in ("crates/orbit-core/assets/jobs/pipeline.yaml",
                     "crates/orbit-core/assets/activities/examples/reference.yaml",
                     "plugin/hooks/check.sh", "server.json", "scripts/build-budget.py"):
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            (self.root / path).write_text("fixture\n")
        self.write_executable(self.bin / "cargo", '''#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
arguments = sys.argv[1:]

def emit_timings(units):
    directory = Path(os.environ["GUARD_TEST_TIMING_DIR"])
    directory.mkdir(parents=True, exist_ok=True)
    report = directory / ("cargo-timing-%s.html" % os.getpid())
    report.write_text("const UNIT_DATA = %s;\\n" % json.dumps(units))
    print("Timing report saved to %s" % report, flush=True)

with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(arguments) + "\\n")
if os.environ.get("GUARD_TEST_PROBE_WORKER_MARKER") and (
        arguments[:2] == ["nextest", "run"] or (arguments[:1] == ["test"] and "--no-fail-fast" in arguments)):
    # Test processes lose only the worker-binding marker, not the rest of the run envelope.
    assert "ORBIT_WORKER_CONTEXT_REQUIRED" not in os.environ
    assert os.environ["ORBIT_RUN_ID"] == "jrun-fixture"
if arguments[0] == "metadata":
    print(open(os.environ["GUARD_TEST_METADATA"]).read())
elif arguments == ["nextest", "--version"]:
    sys.exit(int(os.environ.get("GUARD_TEST_NO_NEXTEST", "0")))
elif "--doc" in arguments:
    if os.environ.get("GUARD_TEST_TIMINGS"):
        selected = [arguments[index + 1] for index, argument in enumerate(arguments) if argument == "-p"]
        metadata = json.load(open(os.environ["GUARD_TEST_METADATA"]))
        seconds_for = {"orbit-core": "0.02", "orbit-cli": "0.03", "orbit_tools": "0.04"}
        if os.environ.get("GUARD_TEST_DOC_NO_REPORT"):
            print("    Finished `test` profile in 0.09s", flush=True)
        else:
            emit_timings([{"name": name, "duration": 0.05} for name in selected if name in seconds_for])
        for package in metadata["packages"]:
            name = package["name"]
            if name in selected and name in seconds_for:
                seconds = seconds_for[name]
                target = next(target for target in package["targets"] if target["doctest"])
                print(f"   Doc-tests {target['name']}", flush=True)
                print("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in %ss" % seconds, flush=True)
    sys.exit(int(os.environ.get("GUARD_TEST_DOC_EXIT", "0")))
else:
    metadata = json.load(open(os.environ["GUARD_TEST_METADATA"]))
    selected = [arguments[index + 1] for index, argument in enumerate(arguments) if argument == "-p"]
    if "--lib" in arguments and not any(target["kind"] == ["lib"]
            for package in metadata["packages"] if package["name"] in selected
            for target in package["targets"]):
        sys.exit(101)  # Cargo rejects --lib when the selection has no library.
    if "--tool-config-file" in arguments:
        # nextest writes the JUnit report the tool config names: one test per package.
        assert "ORBIT_VALIDATION_SUMMARY" not in os.environ
        config = arguments[arguments.index("--tool-config-file") + 1].split(":", 1)[1]
        junit = json.loads(open(config).read().split("path = ", 1)[1])
        with open(junit, "w") as report:
            if os.environ.get("GUARD_TEST_JUNIT"):
                report.write(os.environ["GUARD_TEST_JUNIT"])
            elif os.environ.get("GUARD_TEST_TIMINGS"):
                seconds_for = {"orbit-cli": "0.01", "orbit-cmd": "0.02", "orbit-core": "0.03", "orbit-web": "0.04",
                               "orbit-engine": "0.05", "orbit-tools": "0.06"}
                suites = []
                for name in selected:
                    seconds = seconds_for.get(name, "0.01")
                    suites.append('<testsuite name="%s" tests="1" time="%s"><testcase name="a" classname="%s" time="%s"/></testsuite>' % (name, seconds, name, seconds))
                report.write('<testsuites name="nextest-run" tests="%d">%s</testsuites>' % (len(selected), "".join(suites)))
            else:
                suites = ''.join('<testsuite name="%s"><testcase name="a"/></testsuite>' % name for name in selected)
                report.write(f'<testsuites name="nextest-run" tests="{len(selected)}">{suites}</testsuites>')
    if os.environ.get("GUARD_TEST_NOTICE") and arguments[:2] == ["nextest", "run"]:
        print("    " + os.environ["GUARD_TEST_NOTICE"], flush=True)
        print("ordinary output", flush=True)
    if os.environ.get("GUARD_TEST_PROBE_TMP_GIT"):
        fixture = os.path.join(os.environ["TMPDIR"], "non-git-fixture")
        assert subprocess.run(["git", "-C", fixture, "rev-parse", "--is-inside-work-tree"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0
        assert os.environ["GUARD_TEST_EXISTING_CEILING"] in os.environ["GIT_CEILING_DIRECTORIES"].split(os.pathsep)
    if os.environ.get("GUARD_TEST_TIMINGS"):
        weights = {"orbit-cli": 0.1, "orbit-cmd": 0.2, "orbit-core": 0.3, "orbit-web": 0.4,
                   "orbit-engine": 0.5, "orbit-tools": 0.6}
        emit_timings([{"name": name, "duration": weights.get(name, 0.1)} for name in selected])
        if arguments[0] == "test":
            print("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s", flush=True)
    sys.exit(int(os.environ.get("GUARD_TEST_EXIT", "0")))
''')
        self.write_executable(self.bin / "build-budget", '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["build-budget"] + sys.argv[1:]) + "\\n")
assert sys.argv[1] == "--"
os.execvp(sys.argv[2], sys.argv[2:])
''')
        self.write_executable(self.root / "scripts/check-ci-macos.sh", '''#!/usr/bin/env bash
printf '["check-ci-macos"]\\n' >> "$GUARD_TEST_LOG"
exit "${GUARD_TEST_MACOS_EXIT:-0}"
''')
        self.git("init", "--initial-branch=agent-main")
        self.git("add", ".")
        self.git("commit", "-m", "fixture baseline")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("checkout", "-b", "candidate")

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def git(self, *arguments):
        return subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                               "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *arguments],
                              cwd=self.root, env=self.env, text=True, capture_output=True, check=True)

    def baseline_test_file(self, path, content):
        """Put a tracked test file on the comparison base before the candidate change."""
        self.git("checkout", "agent-main")
        test_file = self.root / path
        test_file.parent.mkdir(parents=True, exist_ok=True)
        test_file.write_text(content)
        self.git("add", path)
        self.git("commit", "-m", "add a baseline test file")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("checkout", "-B", "candidate")
        return test_file

    def gate(self, *arguments, **environment):
        return subprocess.run(["python3", str(self.root / "scripts/ci-test-affected.py"), *arguments],
                              cwd=self.root, env=dict(self.env, **environment), text=True, capture_output=True)

    def selected(self, *arguments):
        result = self.gate("--list", *arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.splitlines()

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def touch_core(self):
        (self.root / "crates/orbit-core/src/lib.rs").write_text("// changed\n")

    def test_core_diff_includes_all_reverse_dependents_before_and_after_commit(self):
        self.touch_core()
        self.assertEqual(self.selected(), self.core_dependents)
        self.git("add", "crates/orbit-core")
        self.assertEqual(self.selected(), self.core_dependents)
        self.git("commit", "-m", "candidate change")
        self.assertEqual(self.selected(), self.core_dependents)
        self.assertEqual(self.selected("--base", self.base), self.core_dependents)
        self.assertEqual(self.selected("--base", "HEAD"), [])

    def test_docs_only_passes_without_running_or_admitting_rust_tests(self):
        (self.root / "docs/guide.md").write_text("Changed docs\n")
        (self.root / "docs/new.md").write_text("Untracked docs\n")
        self.assertEqual(self.selected(), [])
        result = self.gate(GUARD_TEST_EXIT="17")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(call[0] == "metadata" for call in self.calls()))

    def macos_checks(self):
        return sum(1 for call in self.calls() if call == ["check-ci-macos"])

    def test_a_crate_test_file_change_runs_the_macos_workflow_check_before_any_test(self):
        (self.root / "crates/orbit-core/tests").mkdir(parents=True)
        (self.root / "crates/orbit-core/tests/fixture.rs").write_text("// new test\n")
        result = self.gate(GUARD_TEST_MACOS_EXIT="7")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.macos_checks(), 1)
        self.assertFalse(any("--no-fail-fast" in call for call in self.calls()),
                         "a failing macOS workflow check must stop the gate before tests run")

    def test_a_moved_test_file_runs_the_macos_workflow_check_and_then_the_tests(self):
        # The shape of ORB-15852: a test file leaves src/**/tests and lands in a tests/ binary.
        self.baseline_test_file("crates/orbit-tools/src/plugin/tests/backend.rs", "// moved\n")
        (self.root / "crates/orbit-tools/tests/tools").mkdir(parents=True)
        self.git("mv", "crates/orbit-tools/src/plugin/tests/backend.rs", "crates/orbit-tools/tests/tools/backend.rs")
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.macos_checks(), 1)
        self.assertTrue(any("--no-fail-fast" in call for call in self.calls()))

    def test_a_deleted_crate_test_file_runs_the_macos_workflow_check(self):
        test_file = self.baseline_test_file("crates/orbit-tools/src/plugin/tests/backend.rs", "// removed\n")
        test_file.unlink()

        result = self.gate(GUARD_TEST_MACOS_EXIT="7")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.macos_checks(), 1)
        self.assertFalse(any("--no-fail-fast" in call for call in self.calls()),
                         "a deleted test must be checked before any remaining tests run")

    def test_a_rename_out_of_a_crate_tests_directory_runs_the_macos_workflow_check(self):
        self.baseline_test_file("crates/orbit-tools/src/plugin/tests/backend.rs", "// moved out\n")
        self.git("mv", "crates/orbit-tools/src/plugin/tests/backend.rs",
                 "crates/orbit-tools/src/plugin/backend.rs")

        result = self.gate(GUARD_TEST_MACOS_EXIT="7")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.macos_checks(), 1)
        self.assertFalse(any("--no-fail-fast" in call for call in self.calls()),
                         "a renamed test must be checked before any remaining tests run")

    def test_a_workflow_edit_is_checked_even_when_no_crate_is_selected(self):
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / ".github/workflows/ci-macos.yml").write_text("name: macOS Platform\n")
        self.assertEqual(self.selected(), [])
        result = self.gate(GUARD_TEST_MACOS_EXIT="7")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.macos_checks(), 1)

    def test_diffs_that_touch_no_test_file_or_the_workflow_skip_the_macos_workflow_check(self):
        self.touch_core()
        (self.root / "docs/guide.md").write_text("Changed docs\n")
        result = self.gate(GUARD_TEST_MACOS_EXIT="7")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.macos_checks(), 0)

    def test_untracked_and_deleted_crate_paths_are_selected(self):
        (self.root / "crates/orbit-core/src/new.rs").write_text("// new\n")
        self.assertEqual(self.selected(), self.core_dependents)
        (self.root / "crates/orbit-core/src/lib.rs").unlink()
        self.assertEqual(self.selected(), self.core_dependents)

    def test_cross_crate_asset_reads_select_the_reading_crate_and_its_dependents(self):
        # orbit-engine's tests read orbit-core's assets, though core depends on engine.
        engine_dependents = ["orbit-cli", "orbit-cmd", "orbit-core", "orbit-engine", "orbit-web"]
        for path in ("crates/orbit-core/assets/jobs/pipeline.yaml",
                     "crates/orbit-core/assets/activities/examples/reference.yaml"):
            with self.subTest(path=path):
                (self.root / path).write_text("changed\n")
                self.assertEqual(self.selected(), engine_dependents)
                self.git("checkout", "--", path)
        self.assertEqual(self.selected(), [])

    def test_repository_root_file_reads_select_the_reading_crate(self):
        for path in ("plugin/hooks/check.sh", "server.json"):
            with self.subTest(path=path):
                (self.root / path).write_text("changed\n")
                self.assertEqual(self.selected(), ["orbit-cli"])
                self.git("checkout", "--", path)
        (self.root / "scripts/build-budget.py").write_text("changed\n")
        self.assertEqual(self.selected(), sorted(set(self.core_dependents + ["orbit-engine"])))

    def test_unknown_declared_reader_fails_closed(self):
        metadata = json.loads(self.metadata.read_text())
        metadata["packages"] = [package for package in metadata["packages"]
                                if package["name"] != "orbit-engine"]
        metadata["workspace_members"].remove("orbit-engine")
        self.metadata.write_text(json.dumps(metadata))
        (self.root / "server.json").write_text("changed\n")
        self.assertEqual(self.selected(), ["orbit-cli"])
        (self.root / "crates/orbit-core/assets/jobs/pipeline.yaml").write_text("changed\n")
        self.assertNotEqual(self.gate("--list").returncode, 0)

    def test_move_between_crates_selects_both_sides(self):
        self.git("mv", "crates/orbit-tools/src/lib.rs", "crates/orbit-core/src/moved.rs")
        self.assertEqual(self.selected(), sorted({*self.core_dependents, "orbit-tools"}))

    def test_shared_build_inputs_and_removed_members_select_the_workspace(self):
        for path in ("Cargo.toml", ".cargo/config.toml", "crates/removed/src/lib.rs"):
            with self.subTest(path=path):
                file = self.root / path
                file.parent.mkdir(parents=True, exist_ok=True)
                old = file.read_text() if file.exists() else None
                file.write_text("// changed\n")
                self.assertEqual(self.selected(), self.names)
                if old is None:
                    file.unlink()
                else:
                    file.write_text(old)

    def test_full_test_targets_and_doctests_run_under_build_admission(self):
        self.touch_core()
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        package_flags = [argument for name in self.core_dependents for argument in ("-p", name)]
        runtime = ["nextest", "run", "--timings", "--no-fail-fast", "--success-output", "immediate", *package_flags, "--lib", "--bins", "--tests"]
        docs = ["test", "--timings", "--no-fail-fast", *package_flags, "--doc"]
        calls = self.calls()
        self.assertEqual(calls[1:], [["nextest", "--version"],
                                    ["build-budget", "--", "cargo", *runtime], runtime,
                                    ["build-budget", "--", "cargo", *docs], docs])

    def test_test_failures_propagate_including_cargo_fallback_and_doctests(self):
        self.touch_core()
        for environment, code in ((dict(GUARD_TEST_EXIT="17"), 17),
                                  (dict(GUARD_TEST_EXIT="18", GUARD_TEST_NO_NEXTEST="1"), 18),
                                  (dict(GUARD_TEST_DOC_EXIT="19"), 19)):
            with self.subTest(environment=environment):
                self.log.write_text("")
                result = self.gate(**environment)
                self.assertEqual(result.returncode, code, result.stderr)
                tests = [call for call in self.calls() if call[0] in {"test", "nextest"}
                         and call != ["nextest", "--version"]]
                self.assertTrue(tests)
                if "GUARD_TEST_EXIT" in environment:
                    self.assertTrue(all("--doc" not in call for call in tests))
                if "GUARD_TEST_NO_NEXTEST" in environment:
                    self.assertEqual(tests[0][0], "test")

    def test_bin_only_selection_skips_inapplicable_doctest_command(self):
        metadata = json.loads(self.metadata.read_text())
        for package in metadata["packages"]:
            if package["name"] == "orbit-cli":
                package["targets"] = [dict(name="orbit-cli", kind=["bin"], doctest=False)]
        self.metadata.write_text(json.dumps(metadata))
        (self.root / "crates/orbit-cli/src/lib.rs").write_text("// bin-only fixture change\n")
        result = self.gate(GUARD_TEST_DOC_EXIT="19")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(any(call[:2] == ["nextest", "run"] for call in self.calls()))
        self.assertFalse(any("--doc" in call for call in self.calls()))

    def test_nested_temp_fixtures_cannot_discover_the_managed_checkout(self):
        self.touch_core()
        temporary_root = self.root / ".scratch"
        temporary_root.mkdir()
        (temporary_root / "non-git-fixture").mkdir()
        existing_ceiling = str(self.root / "other-boundary")
        result = self.gate(TMPDIR=str(temporary_root), GIT_CEILING_DIRECTORIES=existing_ceiling,
                           GUARD_TEST_PROBE_TMP_GIT="1", GUARD_TEST_EXISTING_CEILING=existing_ceiling)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_test_processes_do_not_inherit_the_worker_binding_marker(self):
        self.touch_core()
        for environment in (dict(), dict(GUARD_TEST_NO_NEXTEST="1")):
            with self.subTest(environment=environment):
                self.log.write_text("")
                result = self.gate(ORBIT_WORKER_CONTEXT_REQUIRED="1", ORBIT_RUN_ID="jrun-fixture",
                                   GUARD_TEST_PROBE_WORKER_MARKER="1", **environment)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(any("--doc" in call for call in self.calls()))

    def summarized(self, **environment):
        summary = self.root / ".scratch/summary.json"
        summary.parent.mkdir(exist_ok=True)
        summary.unlink(missing_ok=True)
        result = self.gate(ORBIT_VALIDATION_SUMMARY=str(summary), **environment)
        return result, json.loads(summary.read_text()) if summary.exists() else None

    def test_summary_reports_the_selection_and_the_tests_nextest_executed(self):
        self.touch_core()
        result, summary = self.summarized()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(summary, dict(schema_version=1, tests_run=len(self.core_dependents), executed_tests=[f"{name} a" for name in self.core_dependents], selection=dict(
            packages=self.core_dependents, target_flags=["--lib", "--bins", "--tests"],
            doctest_packages=self.core_dependents), command=None, base=self.base, exit_code=0, notices=[],
            compile_ms=None, test_ms=None, package_timings=None))
        # A failing run still reports what it selected and ran.
        result, failed = self.summarized(GUARD_TEST_EXIT="17")
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertEqual(failed, dict(summary, exit_code=17))
        # cargo test reports no count, so the summary claims none.
        result, fallback = self.summarized(GUARD_TEST_NO_NEXTEST="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNone(fallback["tests_run"])
        self.assertEqual(fallback["selection"], summary["selection"])

    def test_summary_records_phase_timings_that_sum_per_package(self):
        # Nothing selected did no compile and no test work.
        (self.root / "docs/guide.md").write_text("Changed docs\n")
        result, summary = self.summarized()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((summary["compile_ms"], summary["test_ms"], summary["package_timings"]), (0, 0, []))
        assert_phase_sums(self, summary)

        self.touch_core()
        directory = str(self.root / "target" / "cargo-timings")
        timed = dict(GUARD_TEST_TIMINGS="1", GUARD_TEST_TIMING_DIR=directory)
        result, summary = self.summarized(**timed)
        self.assertEqual(result.returncode, 0, result.stderr)
        assert_phase_sums(self, summary)
        self.assertEqual(summary["compile_ms"], 1100)
        self.assertEqual(summary["test_ms"], 150)
        self.assertEqual(summary["package_timings"], [
            dict(package="orbit-cli", compile_ms=150, test_ms=40),
            dict(package="orbit-cmd", compile_ms=200, test_ms=20),
            dict(package="orbit-core", compile_ms=350, test_ms=50),
            dict(package="orbit-web", compile_ms=400, test_ms=40),
        ])
        # A failing run still reports the timings of the tests it did run.
        result, failed = self.summarized(GUARD_TEST_EXIT="17", **timed)
        self.assertEqual(result.returncode, 17, result.stderr)
        assert_phase_sums(self, failed)
        self.assertEqual((failed["compile_ms"], failed["test_ms"]), (1000, 100))
        self.assertEqual(sum(entry["compile_ms"] for entry in failed["package_timings"]), failed["compile_ms"])
        self.assertEqual(sum(entry["test_ms"] for entry in failed["package_timings"]), failed["test_ms"])
        # cargo test has no per-package test report. Compile units still sum.
        metadata = json.loads(self.metadata.read_text())
        for package in metadata["packages"]:
            for target in package["targets"]:
                target["doctest"] = False
        self.metadata.write_text(json.dumps(metadata))
        result, fallback = self.summarized(GUARD_TEST_NO_NEXTEST="1", **timed)
        self.assertEqual(result.returncode, 0, result.stderr)
        assert_phase_sums(self, fallback)
        self.assertEqual((fallback["compile_ms"], fallback["test_ms"]), (1000, 50))
        self.assertTrue(all(entry["test_ms"] is None for entry in fallback["package_timings"]))
        self.assertEqual(sum(entry["compile_ms"] for entry in fallback["package_timings"]), fallback["compile_ms"])

    def test_cargo_fallback_keeps_libtest_time_when_doctests_follow(self):
        # cargo test cannot attribute its libtest time to a package, so no row
        # claims it. The doctests that follow are still per package, and they
        # add to the total without replacing the libtest time.
        self.touch_core()
        directory = str(self.root / "target" / "cargo-timings")
        result, summary = self.summarized(GUARD_TEST_NO_NEXTEST="1", GUARD_TEST_TIMINGS="1",
                                          GUARD_TEST_TIMING_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        assert_phase_sums(self, summary)
        # 50 ms of libtest runtime, plus 30 ms (orbit-cli) and 20 ms (orbit-core) of doctests.
        self.assertEqual(summary["test_ms"], 100)
        self.assertTrue(all(entry["test_ms"] is None for entry in summary["package_timings"]))

    def test_doctest_target_names_resolve_to_package_rows(self):
        metadata = json.loads(self.metadata.read_text())
        for package in metadata["packages"]:
            if package["name"] == "orbit-cli":
                package["targets"][0]["name"] = "custom_documentation"
                manifest = Path(package["manifest_path"])
                manifest.write_text(manifest.read_text() + '\n[lib]\nname = "custom_documentation"\n')
            elif package["name"] == "orbit-tools":
                # Underscores in package names are valid; replacing them with
                # hyphens would assign this duration to a different identity.
                package["name"] = "orbit_tools"
        self.metadata.write_text(json.dumps(metadata))
        selected = ["orbit-cli", "orbit-core", "orbit_tools"]
        for no_report in ("", "1"):
            with self.subTest(doctest_compile_report=not bool(no_report)):
                result, summary = self.summarized(
                    ORBIT_VALIDATION_SELECTION=json.dumps(dict(packages=selected)),
                    GUARD_TEST_TIMINGS="1", GUARD_TEST_TIMING_DIR=str(self.root / "target/cargo-timings"),
                    GUARD_TEST_DOC_NO_REPORT=no_report)
                self.assertEqual(result.returncode, 0, result.stderr)
                assert_phase_sums(self, summary)
                self.assertEqual(summary["package_timings"], [
                    dict(package="orbit-cli", compile_ms=130 if no_report else 150, test_ms=40),
                    dict(package="orbit-core", compile_ms=330 if no_report else 350, test_ms=50),
                    dict(package="orbit_tools", compile_ms=130 if no_report else 150, test_ms=50),
                ])

    def test_summary_records_what_a_deferral_handoff_derives_from(self):
        # [ORB-15516] Orbit derives an implementer's deferred-sandbox record
        # from the summary: the gate's command, its base, its final exit
        # status (doctests included) and every notice its tests printed.
        self.touch_core()
        notice = "DEFERRED: bubblewrap unavailable: fixture: No permissions to create new namespace"
        result, summary = self.summarized(ORBIT_VALIDATION_GATE="make ci-test-affected",
                                          GUARD_TEST_NOTICE=notice)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((summary["command"], summary["base"], summary["exit_code"], summary["notices"]),
                         ("make ci-test-affected", self.base, 0, [notice]))
        result, failed = self.summarized(GUARD_TEST_NOTICE=notice, GUARD_TEST_DOC_EXIT="19")
        self.assertEqual(result.returncode, 19, result.stderr)
        self.assertEqual((failed["exit_code"], failed["notices"]), (19, [notice]))
        result, explicit = self.summarized(CI_TEST_BASE="agent-main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(explicit["base"], self.base)

    def test_a_base_checkout_runs_the_candidate_selection_it_is_given(self):
        # [ORB-15131] On the base itself the diff is empty: left to select,
        # the run tests nothing, and its summary says so.
        self.touch_core()
        _, candidate = self.summarized()
        self.git("checkout", "--", "crates/orbit-core")
        self.git("checkout", "--quiet", "--detach", self.base)
        self.log.write_text("")
        result, nothing = self.summarized()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((nothing["tests_run"], nothing["selection"]["packages"]), (0, []))
        self.assertTrue(all(call[0] == "metadata" for call in self.calls()))
        # Given the candidate's selection, it tests the same packages and targets.
        result, rerun = self.summarized(ORBIT_VALIDATION_SELECTION=json.dumps(candidate["selection"]))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(rerun, dict(candidate, base=None))
        self.assertTrue(any(call[:2] == ["nextest", "run"] for call in self.calls()))

    def test_a_selection_the_checkout_cannot_run_fails_without_a_summary(self):
        for selection in ('{"packages": ["orbit-removed"]}', '["orbit-core"]', "not json"):
            with self.subTest(selection=selection):
                result, summary = self.summarized(ORBIT_VALIDATION_SELECTION=selection)
                self.assertNotEqual(result.returncode, 0)
                self.assertIsNone(summary)
                self.assertTrue(all(call[0] == "metadata" for call in self.calls()))

    def test_a_failing_tests_selection_replays_only_those_tests(self):
        # [ORB-15328] A base replay of a failed candidate's tests builds only
        # the packages their binary ids name, filters to those tests, skips
        # doctests, and still reports the selection it was handed.
        self.touch_core()
        _, candidate = self.summarized()
        tests = ["orbit-core::runtime suite::broken", "orbit-cli suite::other"]
        handed = dict(candidate["selection"], failing_tests=tests)
        self.log.write_text("")
        result, replay = self.summarized(ORBIT_VALIDATION_SELECTION=json.dumps(handed), GUARD_TEST_EXIT="100")
        self.assertEqual(result.returncode, 100, result.stderr)
        self.assertEqual(replay, dict(schema_version=1, selection=handed, tests_run=2, executed_tests=["orbit-cli a", "orbit-core a"], command=None,
                                      base=None, exit_code=100, notices=[],
                                      compile_ms=None, test_ms=None, package_timings=None))
        [run] = [call for call in self.calls() if call[:2] == ["nextest", "run"]]
        self.assertEqual([run[index + 1] for index, argument in enumerate(run) if argument == "-p"],
                         ["orbit-cli", "orbit-core"])
        self.assertEqual(run[run.index("-E") + 1],
                         "(binary_id(=orbit-core::runtime) & test(=suite::broken))"
                         " | (binary_id(=orbit-cli) & test(=suite::other))")
        self.assertEqual(run[run.index("--test-threads") + 1], "1")
        self.assertEqual(run[run.index("--retries") + 1], "0")
        self.assertFalse(any("--doc" in call for call in self.calls()))
        # The echoed selection and count remain identical even when the
        # actual report contains only a subset, skipped cases or invalid XML.
        for junit, executed in (
                ('<testsuites tests="2"><testsuite name="orbit-core::runtime">'
                 '<testcase name="suite::broken"/><testcase name="suite::ignored"><skipped/></testcase>'
                 '</testsuite></testsuites>', ["orbit-core::runtime suite::broken"]),
                ('<testsuites tests="2"><testsuite><testcase name="suite::broken"/>'
                 '</testsuite></testsuites>', None),
                ('malformed XML', None)):
            with self.subTest(junit=junit):
                result, proof = self.summarized(ORBIT_VALIDATION_SELECTION=json.dumps(handed),
                                                GUARD_TEST_JUNIT=junit)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(proof["selection"], handed)
                self.assertEqual(proof["executed_tests"], executed)

        # A bare test name could be in any package, so the whole selection builds;
        # cargo test filters by exact name.
        self.log.write_text("")
        bare = dict(candidate["selection"], failing_tests=["suite::broken"])
        result, replay = self.summarized(ORBIT_VALIDATION_SELECTION=json.dumps(bare), GUARD_TEST_NO_NEXTEST="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(replay["selection"], bare)
        [run] = [call for call in self.calls() if call[:1] == ["test"] and "--doc" not in call]
        self.assertEqual([run[index + 1] for index, argument in enumerate(run) if argument == "-p"],
                         self.core_dependents)
        self.assertEqual(run[run.index("--"):], ["--", "--exact", "suite::broken", "--test-threads", "1"])

    def test_failing_tests_that_cannot_be_replayed_fail_without_a_summary(self):
        self.touch_core()
        _, candidate = self.summarized()
        for tests in ([], "suite::broken", ["suite::broken)|all()"], ["orbit-tools suite::other"]):
            with self.subTest(tests=tests):
                self.log.write_text("")
                handed = dict(candidate["selection"], failing_tests=tests)
                result, summary = self.summarized(ORBIT_VALIDATION_SELECTION=json.dumps(handed))
                self.assertNotEqual(result.returncode, 0)
                self.assertIsNone(summary)
                self.assertTrue(all(call[0] == "metadata" for call in self.calls()))

    def test_missing_base_fails_closed(self):
        result = self.gate("--base", "missing-delivery-base")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.log.exists())
        self.git("branch", "-D", "agent-main")
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.log.exists())


def nextest_available():
    try:
        return subprocess.run(["cargo", "nextest", "--version"], stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL).returncode == 0
    except OSError:
        return False


@unittest.skipUnless(nextest_available(), "the transport under test is real cargo-nextest")
class NextestOutputTransportTests(unittest.TestCase):
    """[ORB-15161] Run the gate over a real crate under real nextest.

    The host that verifies a required check judges the gate's complete output.
    nextest hides a passing test's output, so a test that returned without
    running its sandboxed path (`DEFERRED: ...`) must still reach that output.
    """

    NOTICE = "DEFERRED: bubblewrap unavailable: fixture: No permissions to create new namespace"

    def run_gate(self, test_body, returncode=0, selection=None, report_failure=False):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name) / "repo"
        (root / "scripts").mkdir(parents=True)
        (root / "crates/suite/src").mkdir(parents=True)
        (root / "crates/suite/tests").mkdir()
        shutil.copy2(SCRIPTS / "ci-test-affected.py", root / "scripts/ci-test-affected.py")
        (root / "Cargo.toml").write_text('[workspace]\nresolver = "2"\nmembers = ["crates/suite"]\n')
        (root / "crates/suite/Cargo.toml").write_text(
            '[package]\nname = "suite"\nversion = "0.1.0"\nedition = "2021"\n')
        (root / "crates/suite/src/lib.rs").write_text("// before\n")
        (root / "crates/suite/tests/fixture.rs").write_text(test_body)
        (root / ".gitignore").write_text("Cargo.lock\n")
        environment = {name: value for name, value in os.environ.items()
                       if not name.startswith(("GIT_", "ORBIT_", "CI_TEST_"))}
        environment.update(CARGO_TARGET_DIR=str(Path(temporary.name) / "target"), CARGO_NET_OFFLINE="true",
                           BUILD_BUDGET="env", CI_TEST_BASE="HEAD",
                           # CI exports CARGO_TERM_COLOR=always; the assertions read plain text.
                           CARGO_TERM_COLOR="never", NO_COLOR="1")
        for arguments in (("init", "--initial-branch=main"), ("add", "."), ("commit", "-m", "base")):
            subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                            "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *arguments],
                           cwd=root, env=environment, capture_output=True, check=True)
        (root / "crates/suite/src/lib.rs").write_text("// changed\n")
        summary = Path(temporary.name) / "summary.json"
        environment["ORBIT_VALIDATION_SUMMARY"] = str(summary)
        evidence = Path(temporary.name) / "evidence"
        if report_failure:
            evidence.write_text("not a directory")
        else:
            evidence.mkdir()
        environment["ORBIT_COMMAND_EVIDENCE_DIR"] = str(evidence)
        environment["ORBIT_COMMAND_EVIDENCE_INVOCATION"] = "fixture-invocation"
        self.evidence = evidence
        if selection is not None:
            environment["ORBIT_VALIDATION_SELECTION"] = json.dumps(selection)
        result = subprocess.run(["python3", str(root / "scripts/ci-test-affected.py")], cwd=root,
                                env=environment, text=True, capture_output=True)
        self.assertEqual(result.returncode, returncode, result.stderr)
        self.summary = json.loads(summary.read_text())
        logs = list((root / ".orbit/tmp").glob("ci-test-affected-nextest-*.log"))
        self.assertEqual(len(logs), 1, result.stderr)
        # The log is the runner's raw bytes, including a cargo progress CR.
        # Text mode would turn that CR into LF and disagree with the evidence copy.
        self.full_output = logs[0].read_text(newline="")
        return result.stdout + result.stderr

    def notices(self, output):
        return [line.strip() for line in output.splitlines() if line.lstrip().startswith("DEFERRED:")]

    def test_a_passing_tests_deferral_notice_reaches_the_gate_output(self):
        for stream in ("eprintln", "println"):
            with self.subTest(stream=stream):
                output = self.run_gate(f'#[test]\nfn deferred() {{ {stream}!("{self.NOTICE}"); }}\n')
                self.assertEqual(self.notices(output), [self.NOTICE], output)
                self.assertEqual(self.summary["notices"], [self.NOTICE], output)

    def test_passing_noise_is_archived_and_all_host_refusal_markers_are_relayed(self):
        # A bounded fixture reproduces the output-volume incident without load
        # or timing races. Both nextest capture streams must retain notices.
        notices = [self.NOTICE, "SKIP: fixture", "skipping: fixture", "skipping fixture",
                   "fixture: sandbox_apply: Operation not permitted",
                   "fixture: sandbox-exec cannot apply a profile",
                   "fixture: No permissions to create new namespace"]
        body = ('#[test]\nfn noisy() { println!("FAIL [fixture sample]\\nSummary [fixture sample]");'
                'println!("{}", "ordinary-noise\\n".repeat(100_000));')
        for stream in ("println", "eprintln"):
            for notice in notices:
                body += f'{stream}!("\\x1b[31m{{}}\\x1b[0m", {json.dumps(notice)});'
        output = self.run_gate(body + '}\n')
        self.assertGreater(len(self.full_output.encode()), 1024 * 1024)
        self.assertLess(len(output.encode()), 16 * 1024)
        self.assertNotIn("ordinary-noise", output)
        self.assertNotIn("fixture sample", output)
        self.assertIn("ordinary-noise", self.full_output)
        for notice in notices:
            self.assertEqual([line.strip() for line in output.splitlines()].count(notice), 2, output)
        self.assertEqual(self.summary["tests_run"], 1, output)

    def test_failure_output_is_relayed_and_archived(self):
        output = self.run_gate('#[test]\nfn broken() { println!("{}", "head-noise\\n".repeat(20000)); println!("failure-body"); println!("{}", "tail-noise\\n".repeat(20000)); panic!("failure-detail"); }',
                               returncode=100)
        for diagnostic in ("failure-body", "failure-detail"):
            self.assertIn(diagnostic, output)
            self.assertIn(diagnostic, self.full_output)
        report = json.loads((self.evidence / "manifest.json").read_text())
        self.assertEqual(report["invocation"], "fixture-invocation")
        [entry] = report["files"]
        retained = (self.evidence / entry["name"]).read_bytes()
        self.assertEqual(retained.decode(), self.full_output)
        self.assertEqual(entry["bytes"], len(retained))
        self.assertEqual(entry["sha256"], hashlib.sha256(retained).hexdigest())
        self.assertTrue(entry["complete"])
        self.assertTrue(entry["source"].startswith(".orbit/tmp/ci-test-affected-nextest-"))
        denied = self.run_gate('#[test]\nfn broken() { panic!("failure-detail"); }',
                               returncode=100, report_failure=True)
        self.assertIn("command evidence report unavailable", denied)
        self.assertIn("failure-detail", denied)
        self.run_gate('#[test]\nfn broken() { println!("{}", "prefix-noise\\n".repeat(100000)); panic!("oversized-detail"); }',
                      returncode=100)
        [partial] = json.loads((self.evidence / "manifest.json").read_text())["files"]
        prefix = (self.evidence / partial["name"]).read_bytes()
        self.assertEqual(len(prefix), 1024 * 1024)
        self.assertGreater(len(self.full_output.encode()), len(prefix))
        self.assertEqual(prefix, self.full_output.encode()[:len(prefix)])
        self.assertEqual(partial["sha256"], hashlib.sha256(prefix).hexdigest())
        self.assertFalse(partial["complete"], "the reported prefix cannot claim complete raw failure evidence")

    def test_a_failing_tests_selection_runs_only_the_named_tests(self):
        # [ORB-15328] The identities a failed run printed select exactly those
        # tests under real nextest's filterset grammar.
        body = ("#[test]\nfn broken() { panic!(\"red\"); }\n#[test]\nfn fine() {}\n"
                "mod inner {\n#[test]\nfn also_broken() { panic!(\"red\"); }\n}\n")
        output = self.run_gate(body, returncode=100)
        self.assertIn("FAIL", output)
        self.assertEqual(self.summary["tests_run"], 3, output)
        handed = dict(self.summary["selection"], failing_tests=["suite::fixture inner::also_broken"])
        output = self.run_gate(body, returncode=100, selection=handed)
        timings = {key: self.summary.pop(key) for key in ("compile_ms", "test_ms", "package_timings")}
        self.assertEqual(self.summary, dict(schema_version=1, selection=handed, tests_run=1, executed_tests=["suite::fixture inner::also_broken"], command=None,
                                            base=None, exit_code=100, notices=[]), output)
        assert_phase_sums(self, timings)
        self.assertIn("suite", {entry["package"] for entry in timings["package_timings"]})
        self.assertIsInstance(timings["compile_ms"], int)
        self.assertIsInstance(timings["test_ms"], int)
        self.assertIn("suite::fixture inner::also_broken", output)
        self.assertNotIn("suite::fixture broken", output)

    def test_a_fully_executed_passing_gate_reports_no_deferral(self):
        output = self.run_gate("#[test]\nfn executed() { assert_eq!(1 + 1, 2); }\n")
        self.assertEqual(self.notices(output), [], output)
        self.assertIn("1 test run: 1 passed", output)


class ObservedPipeDrainTests(unittest.TestCase):
    """A partial line must not stall the other captured pipe.

    The child fills stdout without a newline, then writes one more byte. That
    byte blocks until the reader consumes the pipe, which is when a blocking
    line read is stuck on stdout. Only then does the child write more than a
    pipe of stderr. Independent readers finish; the old shared readline loop
    deadlocks. This deadline kills the process group if that deadlock returns.
    """

    DEADLINE_SECONDS = 10

    def test_executor_sigterm_stops_the_separate_child_session(self):
        scratch = SCRIPTS.parent / ".orbit" / "tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as temporary:
            root = Path(temporary)
            helper = root / "sigterm.py"
            helper.write_text(textwrap.dedent(r'''
                import importlib.util, json, os, signal, subprocess, sys
                from pathlib import Path
                spec = importlib.util.spec_from_file_location("affected", sys.argv[1])
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                root = Path(sys.argv[2])
                children = []
                real_popen = subprocess.Popen
                def recording_popen(*args, **kwargs):
                    child = real_popen(*args, **kwargs)
                    children.append(child)
                    (root / "child.pid").write_text(str(child.pid))
                    return child
                module.subprocess.Popen = recording_popen
                previous = signal.getsignal(signal.SIGTERM)
                try:
                    module.run_observed([sys.executable, "-u", "-c",
                        "import os, signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                        "os.write(1, b'ready\\n'); signal.pause()"], os.environ)
                except SystemExit as caught:
                    (root / "result.json").write_text(json.dumps({
                        "code": caught.code, "returncode": children[0].poll(),
                        "signal_restored": signal.getsignal(signal.SIGTERM) == previous}))
            ''').lstrip("\n"))
            proc = subprocess.Popen(
                [sys.executable, str(helper), str(SCRIPTS / "ci-test-affected.py"), str(root)],
                start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                ready, _, _ = select.select([proc.stdout], [], [], self.DEADLINE_SECONDS)
                self.assertTrue(ready, "observed child did not become ready")
                self.assertEqual(proc.stdout.readline(), b"ready\n")
                proc.send_signal(signal.SIGTERM)
                _, stderr = proc.communicate(timeout=5)
                self.assertEqual(proc.returncode, 0, stderr.decode(errors="replace"))
                result = json.loads((root / "result.json").read_text())
                self.assertEqual(result, {"code": 128 + signal.SIGTERM,
                                          "returncode": -signal.SIGKILL, "signal_restored": True})
            finally:
                # A helper that exited early may still have a live child in
                # its separate session. Clean that session regardless of the
                # helper's status before discarding its PID record.
                pid_path = root / "child.pid"
                if pid_path.exists():
                    try:
                        os.killpg(int(pid_path.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if proc.poll() is None:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.communicate()

    def test_relay_failures_stop_backpressured_child_and_pipe_holding_grandchild(self):
        scratch = SCRIPTS.parent / ".orbit" / "tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as temporary:
            root = Path(temporary)
            fixture = root / "relay_failure.py"
            fixture.write_text(textwrap.dedent(r'''
                import fcntl, importlib.util, io, json, os, signal, subprocess, sys, time
                from pathlib import Path
                from types import SimpleNamespace

                role = sys.argv[1]
                if role == "grandchild":
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    os.write(int(sys.argv[2]), b"ready")
                    signal.pause()
                    raise SystemExit("grandchild unexpectedly survived")

                if role == "child":
                    fd, ack, ready = map(int, sys.argv[2:5])
                    root = Path(sys.argv[5])
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    read_ready, write_ready = os.pipe()
                    grandchild = subprocess.Popen(
                        [sys.executable, __file__, "grandchild", str(write_ready)],
                        pass_fds=(write_ready,))
                    (root / "grandchild.pid").write_text(str(grandchild.pid))
                    os.close(write_ready)
                    assert os.read(read_ready, 5) == b"ready"
                    os.close(read_ready)
                    os.write(fd, b"trigger\n")
                    assert os.read(ack, 1) == b"!"
                    # The relay is waiting here, so its reader cannot drain
                    # this pipe. Measure/fill that exact pipe without load or
                    # timing assumptions, then attempt one byte past capacity.
                    os.set_blocking(fd, False)
                    filled = 0
                    while True:
                        try:
                            filled += os.write(fd, b"X" * 4096)
                        except BlockingIOError:
                            break
                    assert filled > 0
                    if hasattr(fcntl, "F_GETPIPE_SZ"):
                        assert filled == fcntl.fcntl(fd, fcntl.F_GETPIPE_SZ)
                    (root / "backpressure.json").write_text(json.dumps(
                        {"filled": filled, "attempted": filled + 1}))
                    os.set_blocking(fd, True)
                    os.write(ready, b"!")
                    os.write(fd, b"X")
                    raise SystemExit("abandoned pipe unexpectedly drained")

                spec = importlib.util.spec_from_file_location("affected", sys.argv[2])
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                root, stream, operation, cleanup_error = Path(sys.argv[3]), sys.argv[4], sys.argv[5], sys.argv[6]
                # Reap orphaned grandchildren ourselves on Linux, even on a
                # host whose init does not reap them. Other hosts use init.
                if sys.platform == "linux":
                    import ctypes
                    assert ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0
                ack_read, ack_write = os.pipe()
                ready_read, ready_write = os.pipe()
                original = BrokenPipeError("injected relay " + operation)
                children = []
                real_popen = subprocess.Popen
                def recording_popen(*args, **kwargs):
                    kwargs["pass_fds"] = (ack_read, ready_write)
                    child = real_popen(*args, **kwargs)
                    children.append(child)
                    (root / "child.pid").write_text(str(child.pid))
                    return child
                module.subprocess.Popen = recording_popen
                real_killpg = os.killpg
                def failing_cleanup(pid, sig):
                    if sig == signal.SIGTERM:
                        raise OSError("injected cleanup error")
                    return real_killpg(pid, sig)
                if cleanup_error == "yes":
                    module.os.killpg = failing_cleanup

                class Relay:
                    def fail(self):
                        os.write(ack_write, b"!")
                        assert os.read(ready_read, 1) == b"!"
                        raise original
                    def write(self, raw):
                        assert raw == b"trigger\n"
                        if operation == "write":
                            self.fail()
                    def flush(self):
                        if operation == "flush":
                            self.fail()

                module.sys.stdout = SimpleNamespace(buffer=Relay() if stream == "stdout" else io.BytesIO())
                module.sys.stderr = SimpleNamespace(buffer=Relay() if stream == "stderr" else io.BytesIO())
                previous_sigterm = signal.getsignal(signal.SIGTERM)
                started = time.monotonic()
                try:
                    module.run_observed(
                        [sys.executable, __file__, "child", "1" if stream == "stdout" else "2",
                         str(ack_read), str(ready_write), str(root)], os.environ)
                except BaseException as caught:
                    elapsed = time.monotonic() - started
                    result = {"same_exception": caught is original, "elapsed": elapsed,
                              "returncode": children[0].poll(),
                              "signal_restored": signal.getsignal(signal.SIGTERM) == previous_sigterm}
                else:
                    raise AssertionError("relay error was swallowed")
                if sys.platform == "linux":
                    grandchild_pid = int((root / "grandchild.pid").read_text())
                    reaped, status = os.waitpid(grandchild_pid, 0)
                    result["grandchild_killed"] = reaped == grandchild_pid and os.waitstatus_to_exitcode(status) == -signal.SIGKILL
                (root / "result.json").write_text(json.dumps(result))
                # Restore real streams before Python's final flush.
                sys.stdout, sys.stderr = sys.__stdout__, sys.__stderr__
            ''').lstrip("\n"))
            for stream, operation, cleanup_error in (
                    ("stdout", "write", "no"), ("stderr", "write", "no"),
                    ("stdout", "flush", "no"), ("stdout", "write", "yes")):
                with self.subTest(stream=stream, operation=operation, cleanup_error=cleanup_error):
                    proc = subprocess.Popen(
                        [sys.executable, str(fixture), "helper", str(SCRIPTS / "ci-test-affected.py"),
                         str(root), stream, operation, cleanup_error],
                        start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    try:
                        stdout, stderr = proc.communicate(timeout=self.DEADLINE_SECONDS)
                    except subprocess.TimeoutExpired:
                        # The child's session is separate from the helper's.
                        # Clean both before reporting a regression; this timer
                        # is independent of the measured propagation latency.
                        for pid_path in (root / "child.pid", root / "grandchild.pid"):
                            if pid_path.exists():
                                try:
                                    os.killpg(int(pid_path.read_text()), signal.SIGKILL)
                                except ProcessLookupError:
                                    # The pre-repair implementation left the
                                    # child in the helper's group. Kill it by
                                    # PID so the helper can still reap it.
                                    try:
                                        os.kill(int(pid_path.read_text()), signal.SIGKILL)
                                    except ProcessLookupError:
                                        pass
                        # Killing the child lets even the broken implementation
                        # finish joining and the helper reap its grandchild.
                        try:
                            proc.communicate(timeout=self.DEADLINE_SECONDS)
                        except subprocess.TimeoutExpired:
                            os.killpg(proc.pid, signal.SIGKILL)
                            proc.communicate()
                        self.fail("run_observed hung after a relay failure with a full pipe")
                    self.assertEqual(proc.returncode, 0, (stdout + stderr).decode(errors="replace"))
                    result = json.loads((root / "result.json").read_text())
                    self.assertTrue(result["same_exception"], result)
                    self.assertLess(result["elapsed"], 5, result)
                    self.assertEqual(result["returncode"], -signal.SIGKILL, result)
                    self.assertTrue(result["signal_restored"], result)
                    if sys.platform == "linux":
                        self.assertTrue(result["grandchild_killed"], result)
                    pressure = json.loads((root / "backpressure.json").read_text())
                    self.assertGreater(pressure["filled"], 0)
                    self.assertGreater(pressure["attempted"], pressure["filled"])

    def test_partial_stdout_and_stderr_past_pipe_capacity_preserve_status_and_streams(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        child = root / "child.py"
        result_path = root / "result.json"
        child.write_text(textwrap.dedent(r'''
            import fcntl, os, sys

            def pipe_capacity(fd):
                try:
                    size = fcntl.fcntl(fd, fcntl.F_GETPIPE_SZ)
                except (AttributeError, OSError):
                    size = 0
                if isinstance(size, int) and size > 0:
                    return size
                read_fd, write_fd = os.pipe()
                try:
                    os.set_blocking(write_fd, False)
                    chunk = b"P" * 4096
                    measured = 0
                    while True:
                        try:
                            wrote = os.write(write_fd, chunk)
                        except BlockingIOError:
                            break
                        if wrote <= 0:
                            break
                        measured += wrote
                    return measured
                finally:
                    os.close(read_fd)
                    os.close(write_fd)

            def write_all(fd, data):
                view = memoryview(data)
                while view:
                    wrote = os.write(fd, view)
                    if wrote <= 0:
                        raise SystemExit("pipe write stalled")
                    view = view[wrote:]

            stdout = sys.stdout.fileno()
            stderr = sys.stderr.fileno()
            stdout_cap = pipe_capacity(stdout)
            marker = b"PARTIAL_STDOUT|"
            pad = stdout_cap - len(marker)
            if pad < 1:
                raise SystemExit("stdout pipe is smaller than the partial-line marker")
            # Exactly one pipe of bytes and no newline, so a line read cannot return yet.
            write_all(stdout, marker + b"S" * pad)
            # Unblocks only after that pipe is consumed.
            write_all(stdout, b"X")
            stderr_cap = pipe_capacity(stderr)
            write_all(stderr, b"STDERR_MARK|" + b"E" * (stderr_cap + 4096) + b"STDERR_END")
            write_all(stdout, (" DONE stdout_cap=%d stderr_cap=%d\n" % (stdout_cap, stderr_cap)).encode())
            write_all(stderr, b"\n")
            raise SystemExit(23)
        ''').lstrip("\n"))
        helper = root / "helper.py"
        helper.write_text(textwrap.dedent(r'''
            import importlib.util, json, os, sys
            from pathlib import Path
            spec = importlib.util.spec_from_file_location("ci_test_affected", sys.argv[1])
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            real_popen = module.subprocess.Popen
            def recording_popen(*args, **kwargs):
                child = real_popen(*args, **kwargs)
                Path(sys.argv[3]).with_suffix(".pid").write_text(str(child.pid))
                return child
            module.subprocess.Popen = recording_popen
            code, output = module.run_observed([sys.executable, "-u", sys.argv[2]], os.environ)
            Path(sys.argv[3]).write_text(json.dumps({"exit_code": code, "output": output}))
        ''').lstrip("\n"))
        proc = subprocess.Popen(
            [sys.executable, str(helper), str(SCRIPTS / "ci-test-affected.py"), str(child), str(result_path)],
            start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            stdout, stderr = proc.communicate(timeout=self.DEADLINE_SECONDS)
        except subprocess.TimeoutExpired:
            pid_path = result_path.with_suffix(".pid")
            if pid_path.exists():
                pid = int(pid_path.read_text())
                try:
                    os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            # Allow the helper to reap the separate child session first.
            try:
                proc.communicate(timeout=self.DEADLINE_SECONDS)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.communicate()
            self.fail("run_observed blocked past its deadline on a partial stdout line "
                      "while stderr held more than a pipe of data")
        self.assertEqual(proc.returncode, 0, stderr.decode("utf-8", errors="replace"))
        recorded = json.loads(result_path.read_text())
        output = recorded["output"]
        self.assertEqual(recorded["exit_code"], 23, output)
        marker = "DONE stdout_cap="
        self.assertIn(marker, output)
        caps = output.split(marker, 1)[1].splitlines()[0]
        stdout_cap = int(caps.split()[0])
        stderr_cap = int(caps.split("stderr_cap=", 1)[1])
        self.assertGreater(stdout_cap, 0)
        self.assertGreater(stderr_cap, 0)
        self.assertGreaterEqual(output.count("S"), stdout_cap - len("PARTIAL_STDOUT|"))
        self.assertIn("PARTIAL_STDOUT|", output)
        self.assertIn("X DONE", output)
        self.assertIn("STDERR_MARK|", output)
        self.assertIn("STDERR_END", output)
        self.assertGreater(output.count("E"), stderr_cap)
        relayed_out = stdout.decode("utf-8", errors="replace")
        relayed_err = stderr.decode("utf-8", errors="replace")
        self.assertIn("PARTIAL_STDOUT|", relayed_out)
        self.assertIn("X DONE", relayed_out)
        self.assertNotIn("STDERR_MARK|", relayed_out)
        self.assertIn("STDERR_MARK|", relayed_err)
        self.assertIn("STDERR_END", relayed_err)
        self.assertNotIn("PARTIAL_STDOUT|", relayed_err)


if __name__ == "__main__":
    unittest.main()
