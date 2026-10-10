#!/usr/bin/env python3
"""Exercise affected-crate test selection and execution at the script boundary."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


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
                     "ORBIT_VALIDATION_SUMMARY", "ORBIT_VALIDATION_SELECTION"):
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
                                 targets=[dict(kind=["lib"], doctest=True)],
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
arguments = sys.argv[1:]
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(arguments) + "\\n")
if os.environ.get("GUARD_TEST_PROBE_WORKER_MARKER") and arguments[:2] in (["nextest", "run"], ["test", "--no-fail-fast"]):
    # Test processes lose only the worker-binding marker, not the rest of the run envelope.
    assert "ORBIT_WORKER_CONTEXT_REQUIRED" not in os.environ
    assert os.environ["ORBIT_RUN_ID"] == "jrun-fixture"
if arguments[0] == "metadata":
    print(open(os.environ["GUARD_TEST_METADATA"]).read())
elif arguments == ["nextest", "--version"]:
    sys.exit(int(os.environ.get("GUARD_TEST_NO_NEXTEST", "0")))
elif "--doc" in arguments:
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
            report.write(f'<testsuites name="nextest-run" tests="{len(selected)}"/>')
    if os.environ.get("GUARD_TEST_PROBE_TMP_GIT"):
        fixture = os.path.join(os.environ["TMPDIR"], "non-git-fixture")
        assert subprocess.run(["git", "-C", fixture, "rev-parse", "--is-inside-work-tree"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0
        assert os.environ["GUARD_TEST_EXISTING_CEILING"] in os.environ["GIT_CEILING_DIRECTORIES"].split(os.pathsep)
    sys.exit(int(os.environ.get("GUARD_TEST_EXIT", "0")))
''')
        self.write_executable(self.bin / "build-budget", '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["build-budget"] + sys.argv[1:]) + "\\n")
assert sys.argv[1] == "--"
os.execvp(sys.argv[2], sys.argv[2:])
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
        runtime = ["nextest", "run", "--no-fail-fast", "--success-output", "immediate", *package_flags, "--lib", "--bins", "--tests"]
        docs = ["test", "--no-fail-fast", *package_flags, "--doc"]
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
                package["targets"] = [dict(kind=["bin"], doctest=False)]
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
        self.assertEqual(summary, dict(schema_version=1, tests_run=len(self.core_dependents), selection=dict(
            packages=self.core_dependents, target_flags=["--lib", "--bins", "--tests"],
            doctest_packages=self.core_dependents)))
        # A failing run still reports what it selected and ran.
        result, failed = self.summarized(GUARD_TEST_EXIT="17")
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertEqual(failed, summary)
        # cargo test reports no count, so the summary claims none.
        result, fallback = self.summarized(GUARD_TEST_NO_NEXTEST="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNone(fallback["tests_run"])
        self.assertEqual(fallback["selection"], summary["selection"])

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
        self.assertEqual(rerun, candidate)
        self.assertTrue(any(call[:2] == ["nextest", "run"] for call in self.calls()))

    def test_a_selection_the_checkout_cannot_run_fails_without_a_summary(self):
        for selection in ('{"packages": ["orbit-removed"]}', '["orbit-core"]', "not json"):
            with self.subTest(selection=selection):
                result, summary = self.summarized(ORBIT_VALIDATION_SELECTION=selection)
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

    NOTICE = "DEFERRED: bubblewrap unavailable: fixture: No permissions to create a new namespace"

    def run_gate(self, test_body):
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
        result = subprocess.run(["python3", str(root / "scripts/ci-test-affected.py")], cwd=root,
                                env=environment, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout + result.stderr

    def notices(self, output):
        return [line.strip() for line in output.splitlines() if line.lstrip().startswith("DEFERRED:")]

    def test_a_passing_tests_deferral_notice_reaches_the_gate_output(self):
        for stream in ("eprintln", "println"):
            with self.subTest(stream=stream):
                output = self.run_gate(f'#[test]\nfn deferred() {{ {stream}!("{self.NOTICE}"); }}\n')
                self.assertEqual(self.notices(output), [self.NOTICE], output)

    def test_a_fully_executed_passing_gate_reports_no_deferral(self):
        output = self.run_gate("#[test]\nfn executed() { assert_eq!(1 + 1, 2); }\n")
        self.assertEqual(self.notices(output), [], output)
        self.assertIn("1 test run: 1 passed", output)


if __name__ == "__main__":
    unittest.main()
