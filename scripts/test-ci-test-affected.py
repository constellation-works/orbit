#!/usr/bin/env python3
"""Exercise affected-crate test selection and execution at the script boundary."""

import contextlib
import hashlib
import json
import os
from pathlib import Path
import select
import socket
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock


SCRIPTS = Path(__file__).resolve().parent


# The anchor is the owned child's session leader. A private socket lets the
# independent watchdog ask that still-live leader to kill its own group; no
# numeric PID or group ID is signalled after the identity can be recycled.
ANCHOR = r"""
import json, os, signal, subprocess, sys, threading
control = int(sys.argv[1])
fds = json.loads(sys.argv[2])
command = json.loads(sys.argv[3])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
def cancel():
    if os.read(control, 1):
        os.killpg(os.getpgrp(), signal.SIGKILL)
threading.Thread(target=cancel, daemon=True).start()
def reset():
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
child = subprocess.Popen(command, pass_fds=tuple(fds), preexec_fn=reset)
code = child.wait()
if code < 0:
    os.kill(os.getpid(), -code)
else:
    sys.exit(code)
"""


def spawn_fixture_helper(command, pass_fds=()):
    watcher, child_socket = socket.socketpair()
    env = dict(os.environ, ORBIT_FIXTURE_WATCHDOG_FD=str(child_socket.fileno()),
               ORBIT_FIXTURE_ANCHOR=ANCHOR)
    try:
        proc = subprocess.Popen(command, start_new_session=True, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, env=env,
                                pass_fds=(child_socket.fileno(), *pass_fds))
    finally:
        child_socket.close()
    proc.fixture_watchdog = watcher
    return proc


def stop_owned_helper(proc, *, sigterm_timeout=5, watchdog_timeout=5):
    """Cancel owned descendants through their live anchor and reap the helper."""
    if proc.returncode is None:
        with contextlib.suppress(ProcessLookupError):
            proc.send_signal(signal.SIGTERM)
        try:
            proc.communicate(timeout=sigterm_timeout)
        except subprocess.TimeoutExpired:
            with contextlib.suppress(OSError):
                proc.fixture_watchdog.sendall(b"x")
            try:
                proc.communicate(timeout=watchdog_timeout)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.communicate()
    # If a broken supervisor already exited, any surviving anchor still owns
    # the socket. It alone decides whether it can signal its group safely.
    with contextlib.suppress(OSError):
        proc.fixture_watchdog.sendall(b"x")
    proc.fixture_watchdog.close()


def reap_adopted_descendant(pid, *, timeout=5, poll_interval=0.01,
                            waitpid=os.waitpid, monotonic=time.monotonic, sleep=time.sleep):
    """Wait boundedly for an adopted descendant PID to be reparented and reaped."""
    deadline = monotonic() + timeout
    while True:
        try:
            reaped, status = waitpid(pid, os.WNOHANG)
        except (ChildProcessError, InterruptedError):
            reaped = 0
        if reaped == pid:
            return os.waitstatus_to_exitcode(status)
        if monotonic() >= deadline:
            raise TimeoutError("watchdog descendant was not adopted and reaped")
        sleep(poll_interval)


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
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
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
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR",
                     "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                     "CI_TEST_BASE", "ORBIT_VALIDATION_SUMMARY", "ORBIT_VALIDATION_SELECTION",
                     "ORBIT_VALIDATION_GATE", "ORBIT_COMMAND_EVIDENCE_DIR",
                     "ORBIT_COMMAND_EVIDENCE_INVOCATION"):
            self.env.pop(name, None)
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
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
                                 targets=[dict(name=name.replace("-", "_"), kind=["lib"], doctest=False)],
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
elif arguments[0] == "tree":
    # Selected crates' dev-dependencies add features: GUARD_TEST_TREE_FEATURES
    # maps a crate to the `tokio` features it requests; the workspace has all,
    # plus GUARD_TEST_TREE_WORKSPACE_ONLY, which no selection reaches.
    requested = json.loads(os.environ.get("GUARD_TEST_TREE_FEATURES", "{}"))
    selected = ([name for name in json.load(open(os.environ["GUARD_TEST_METADATA"]))["workspace_members"]]
                if "--workspace" in arguments else
                [arguments[index + 1] for index, argument in enumerate(arguments) if argument == "-p"])
    features = {"rt", *(feature for name in selected for feature in requested.get(name, []))}
    if "--workspace" in arguments and os.environ.get("GUARD_TEST_TREE_WORKSPACE_ONLY"):
        features.add(os.environ["GUARD_TEST_TREE_WORKSPACE_ONLY"])
    features = sorted(features)
    print("tokio v1.0.0\t" + ",".join(features))
    # A repeat prints only when another crate reaches the dependency. Colored
    # output wraps its marker in escapes, as CI's CARGO_TERM_COLOR=always does.
    colored = os.environ.get("CARGO_TERM_COLOR") == "always" and "--color" not in arguments
    if len(selected) > 1:
        print("tokio v1.0.0\t" + ",".join(features) + (" \x1b[33m\x1b[2m(*)\x1b[39m\x1b[22m" if colored else " (*)"))
elif arguments == ["nextest", "--version"]:
    sys.exit(int(os.environ.get("GUARD_TEST_NO_NEXTEST", "0")))
elif "--doc" in arguments:
    # Doctests are disabled workspace-wide; the gate must never request --doc.
    # Fail the fixture loudly so a regression cannot pass silently.
    print("ci-test-affected fixture: --doc must never be requested (doctests disabled)", flush=True)
    sys.exit(99)
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

    def test_a_workflow_edit_selects_its_reader_and_checks_macos_filters(self):
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / ".github/workflows/ci-macos.yml").write_text("name: macOS Platform\n")
        self.assertIn("orbit-engine", self.selected())
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
        (self.root / "crates/orbit-core/src/new.rs").unlink()
        self.assertEqual(self.selected(), [])
        (self.root / "crates/orbit-core/src/lib.rs").unlink()
        self.assertEqual(self.selected(), self.core_dependents)
        script = self.root / "scripts/ci-test-affected.py"
        source = script.read_text()
        original = 'if path}'
        self.assertIn(original, source)
        script.write_text(source.replace(original, 'if path and os.fsdecode(path) not in {"crates/orbit-core/src/lib.rs", "scripts/ci-test-affected.py"}}'))
        self.assertNotEqual(self.selected(), self.core_dependents,
                            "suppressing deleted paths must invalidate the selection")

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

    def test_repository_readers_select_their_cargo_tests(self):
        readers = {
            "scripts/test-qa-full-sweep.py": self.core_dependents,
            "scripts/ci-test-affected.py": self.core_dependents,
            "scripts/check_npm_package.py": self.core_dependents,
            "crates/orbit-core/assets/executors/codex.yaml": self.core_dependents,
            "crates/orbit-engine/tests/engine/pr_landing/fixtures/base-refresh-forge.sh": sorted({*self.core_dependents, "orbit-engine"}),
            "scripts/ci-await-jobs.py": sorted({*self.core_dependents, "orbit-engine"}),
            ".github/workflows/ci.yml": sorted({*self.core_dependents, "orbit-engine"}),
            ".github/workflows/ci-macos.yml": sorted({*self.core_dependents, "orbit-engine"}),
            ".github/workflows/website-check.yml": sorted({*self.core_dependents, "orbit-engine"}),
            ".github/workflows/codeql.yml": sorted({*self.core_dependents, "orbit-engine"}),
            "scripts/release-readiness-adapter.py": sorted({*self.core_dependents, "orbit-engine"}),
            "docs/development/examples/release-readiness/scripts/release-readiness.yaml": sorted({*self.core_dependents, "orbit-engine"}),
            "docs/design/auto-tasks/1_overview.md": self.core_dependents,
            "docs/development/qa.md": ["orbit-cli"],
            "plugin/skills/orbit-setup/references/auto-tasks-authoring.md": self.core_dependents,
            "crates/orbit-tools/tests/fixtures/plugins/uv-example/plugin.yaml": ["orbit-cli", "orbit-tools"],
        }
        for relative, expected in readers.items():
            with self.subTest(path=relative):
                path = self.root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                previous = path.read_text() if path.exists() else None
                path.write_text((previous or "") + "# changed\n")
                try:
                    self.assertEqual(self.selected(), expected)
                    if relative == "scripts/test-qa-full-sweep.py":
                        paths = self.root / ".scratch/changed-paths"
                        paths.parent.mkdir(exist_ok=True)
                        paths.write_text(relative + "\n")
                        original = self.metadata.read_text()
                        metadata = json.loads(original)
                        for name in ("orbit-auth", "orbit-exec", "orbit-policy", "orbit-store"):
                            metadata["workspace_members"].append(name)
                            metadata["packages"].append(dict(
                                id=name, name=name, manifest_path=str(self.root / "crates" / name / "Cargo.toml"),
                                dependencies=[], targets=[dict(name=name, kind=["lib"], doctest=False)]))
                        self.metadata.write_text(json.dumps(metadata))
                        try:
                            result = self.gate("--pull-request-scope", str(paths))
                        finally:
                            self.metadata.write_text(original)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn("orbit-core", result.stdout.splitlines()[0])
                finally:
                    if previous is None:
                        path.unlink()
                    else:
                        path.write_text(previous)

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

    def test_a_ci_test_build_adds_the_crates_that_keep_workspace_features(self):
        def build(crates, features=None, **environment):
            result = self.gate("--test-build-packages", crates,
                               GUARD_TEST_TREE_FEATURES=json.dumps(features or {}), **environment)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout.splitlines()[:2]

        # orbit-cli alone resolves tokio without orbit-web's dev-dependency feature.
        self.assertEqual(build("orbit-cli", {"orbit-web": ["test-util"]}),
                         ["orbit-cli orbit-web", "-p orbit-cli -p orbit-web --bins --tests"])
        self.assertEqual(build("orbit-cli", {"orbit-web": ["test-util"], "orbit-cmd": ["io-util"]})[0],
                         "orbit-cli orbit-cmd orbit-web")
        self.assertEqual(build("orbit-tools"), ["orbit-tools", "-p orbit-tools --bins --tests"],
                         "a selection that already resolves the workspace's features builds alone")
        # [ORB-22077] CI colors Cargo's output. A colored repeat marker must not
        # read as a feature gap: it added three needless anchors to hosted builds.
        self.assertEqual(build("orbit-tools", CARGO_TERM_COLOR="always"),
                         ["orbit-tools", "-p orbit-tools --bins --tests"])
        # A crate the checkout lacks, or a gap no member closes, builds the workspace.
        self.assertEqual(build("orbit-retired")[0], "workspace")
        self.assertEqual(build("orbit-cli", GUARD_TEST_TREE_WORKSPACE_ONLY="signal")[0], "workspace")

    def test_disabled_lib_harness_is_omitted_from_build_and_runtime_selectors(self):
        # A library with test=false (as in cargo metadata) must not be forced
        # back by an explicit --lib; --tests retains enabled lib unit tests.
        metadata = json.loads(self.metadata.read_text())
        for package in metadata["packages"]:
            if package["name"] == "orbit-tools":
                package["targets"] = [dict(name="orbit_tools", kind=["lib"], doctest=False, test=False)]
        self.metadata.write_text(json.dumps(metadata))
        result = self.gate("--test-build-packages", "orbit-tools")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines()[:2],
                         ["orbit-tools", "-p orbit-tools --bins --tests"])
        (self.root / "crates/orbit-tools/src/lib.rs").write_text("// changed\n")
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any("--lib" in call for call in self.calls()),
                         "explicit --lib would rebuild the disabled empty lib harness")

    def test_full_test_targets_run_under_build_admission(self):
        self.touch_core()
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        package_flags = [argument for name in self.core_dependents for argument in ("-p", name)]
        runtime = ["nextest", "run", "--timings", "--no-fail-fast", "--success-output", "immediate", *package_flags, "--bins", "--tests"]
        calls = self.calls()
        self.assertEqual(calls[1:], [["nextest", "--version"],
                                    ["build-budget", "--", "cargo", *runtime], runtime])
        self.assertFalse(any("--doc" in call for call in self.calls()))

    def test_test_failures_propagate_including_cargo_fallback(self):
        self.touch_core()
        for environment, code in ((dict(GUARD_TEST_EXIT="17"), 17),
                                  (dict(GUARD_TEST_EXIT="18", GUARD_TEST_NO_NEXTEST="1"), 18)):
            with self.subTest(environment=environment):
                self.log.write_text("")
                result = self.gate(**environment)
                self.assertEqual(result.returncode, code, result.stderr)
                tests = [call for call in self.calls() if call[0] in {"test", "nextest"}
                         and call != ["nextest", "--version"]]
                self.assertTrue(tests)
                self.assertTrue(all("--doc" not in call for call in tests))
                if "GUARD_TEST_NO_NEXTEST" in environment:
                    self.assertEqual(tests[0][0], "test")

    def test_no_selection_requests_doctests(self):
        metadata = json.loads(self.metadata.read_text())
        for package in metadata["packages"]:
            if package["name"] == "orbit-cli":
                package["targets"] = [dict(name="orbit-cli", kind=["bin"], doctest=False)]
        self.metadata.write_text(json.dumps(metadata))
        (self.root / "crates/orbit-cli/src/lib.rs").write_text("// bin-only fixture change\n")
        result = self.gate()
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
                self.assertFalse(any("--doc" in call for call in self.calls()))

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
            packages=self.core_dependents, target_flags=["--bins", "--tests"]), command=None, base=self.base, exit_code=0, notices=[],
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
        self.assertEqual(summary["compile_ms"], 1000)
        self.assertEqual(summary["test_ms"], 100)
        self.assertEqual(summary["package_timings"], [
            dict(package="orbit-cli", compile_ms=100, test_ms=10),
            dict(package="orbit-cmd", compile_ms=200, test_ms=20),
            dict(package="orbit-core", compile_ms=300, test_ms=30),
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

    def test_cargo_fallback_reports_libtest_time_only(self):
        # cargo test cannot attribute its libtest time to a package, so no row
        # claims it. Doctests are disabled, so nothing follows to add to it.
        self.touch_core()
        directory = str(self.root / "target" / "cargo-timings")
        result, summary = self.summarized(GUARD_TEST_NO_NEXTEST="1", GUARD_TEST_TIMINGS="1",
                                          GUARD_TEST_TIMING_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        assert_phase_sums(self, summary)
        # 50 ms of libtest runtime and no doctest command.
        self.assertEqual(summary["test_ms"], 50)
        self.assertTrue(all(entry["test_ms"] is None for entry in summary["package_timings"]))
        self.assertFalse(any("--doc" in call for call in self.calls()))

    def test_selection_carries_no_doctest_packages(self):
        # Doctests are disabled: the selection has no doctest packages and the
        # gate never invokes `cargo test --doc`.
        self.touch_core()
        result, summary = self.summarized()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("doctest_packages", summary["selection"])
        self.assertFalse(any("--doc" in call for call in self.calls())
                         )

    def test_summary_records_what_a_deferral_handoff_derives_from(self):
        # [ORB-15516] Orbit derives an implementer's deferred-sandbox record
        # from the summary: the gate's command, its base, its final exit
        # status and every notice its tests printed.
        self.touch_core()
        notice = "DEFERRED: bubblewrap unavailable: fixture: No permissions to create new namespace"
        result, summary = self.summarized(ORBIT_VALIDATION_GATE="make ci-test-affected",
                                          GUARD_TEST_NOTICE=notice)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((summary["command"], summary["base"], summary["exit_code"], summary["notices"]),
                         ("make ci-test-affected", self.base, 0, [notice]))
        result, failed = self.summarized(GUARD_TEST_NOTICE=notice, GUARD_TEST_EXIT="19")
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
        # the packages their binary ids name, filters to those tests,
        # and still reports the selection it was handed.
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
        [run] = [call for call in self.calls() if call[:1] == ["test"]]
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


@unittest.skipUnless(nextest_available() or os.environ.get("GITHUB_ACTIONS") == "true",
                     "the transport under test is real cargo-nextest")
class NextestOutputTransportTests(unittest.TestCase):
    """[ORB-15161] Run the gate over a real crate under real nextest.

    The host that verifies a required check judges the gate's complete output.
    nextest hides a passing test's output, so a test that returned without
    running its sandboxed path (`DEFERRED: ...`) must still reach that output.
    """

    NOTICE = "DEFERRED: bubblewrap unavailable: fixture: No permissions to create new namespace"

    @classmethod
    def setUpClass(cls):
        if not nextest_available():
            raise AssertionError("cargo-nextest must be installed in CI")

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
                           # Forced progress makes cargo fail without a terminal width; with one, its redraws add \r.
                           CARGO_TERM_COLOR="never", CARGO_TERM_PROGRESS_WHEN="never", NO_COLOR="1")
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
        # Raw bytes: read_text() would translate the \r redraws that the retained evidence keeps.
        self.full_output = logs[0].read_bytes().decode()
        return result.stdout + result.stderr

    def notices(self, output):
        return [line.strip() for line in output.splitlines() if line.lstrip().startswith("DEFERRED:")]

    def test_a_passing_tests_deferral_notice_reaches_the_gate_output(self):
        for stream in ("eprintln", "println"):
            with self.subTest(stream=stream):
                output = self.run_gate(f'#[test]\nfn deferred() {{ {stream}!("{self.NOTICE}"); }}\n')
                self.assertEqual(self.notices(output), [self.NOTICE], output)
                self.assertEqual(self.summary["notices"], [self.NOTICE], output)

    def test_a_passing_child_tests_genuine_skip_notice_is_relayed_and_recorded(self):
        notice = "SKIP: genuine child host capability gap"
        body = f"""
#[test]
fn parent() {{
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "skipping_child", "--ignored", "--nocapture"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.lines().any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")));
    for line in stdout.lines().chain(stderr.lines()) {{
        let trimmed = line.trim_start();
        if trimmed.starts_with("SKIP:") {{
            eprintln!("{{}}", line.trim());
        }}
    }}
}}

#[test]
#[ignore]
fn skipping_child() {{
    eprintln!({json.dumps(notice)});
}}
"""
        output = self.run_gate(body)
        self.assertEqual([line.strip() for line in output.splitlines()].count(notice), 1, output)
        self.assertEqual(self.summary["notices"], [notice], output)
        self.assertEqual(self.summary["executed_tests"], ["suite::fixture parent"], output)

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

    def test_watchdog_cancels_owned_group_when_supervisor_ignores_sigterm(self):
        if sys.platform != "linux":
            self.skipTest("the fixture checks Linux subreaper adoption")
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as temporary:
            root = Path(temporary)
            helper = root / "broken.py"
            helper.write_text(textwrap.dedent(r'''
                import ctypes, importlib.util, json, os, signal, subprocess, sys, threading
                from pathlib import Path

                spec = importlib.util.spec_from_file_location("watchdog_helpers", sys.argv[1])
                helpers = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(helpers)

                libc = ctypes.CDLL(None, use_errno=True)
                libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
                libc.prctl.restype = ctypes.c_int
                assert libc.prctl(36, 1, 0, 0, 0) == 0
                signal.signal(signal.SIGTERM, signal.SIG_IGN)

                pid_path, result_path = Path(sys.argv[2]), Path(sys.argv[3])
                ready_write, ping_read, pong_write = map(int, sys.argv[4:7])
                fd = int(os.environ["ORBIT_FIXTURE_WATCHDOG_FD"])

                grandchild_code = (
                    "import os, pathlib, signal, sys; "
                    "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                    "pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); "
                    "os.write(int(sys.argv[2]), b'R'); "
                    "os.close(int(sys.argv[2])); "
                    "signal.pause()"
                )
                child_code = (
                    "import os, signal, subprocess, sys; "
                    "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                    "ready_fd = int(sys.argv[2]); "
                    "subprocess.Popen([sys.executable, '-c', sys.argv[3], sys.argv[1], str(ready_fd)], "
                    "pass_fds=(ready_fd,)); "
                    "os.close(ready_fd); "
                    "signal.pause()"
                )
                # Anchor waits only on its direct child, so the unwaited grandchild stays
                # adopted by this subreaper regardless of SIGKILL exit order in the group.
                anchor_proc = subprocess.Popen(
                    [sys.executable, "-c", os.environ["ORBIT_FIXTURE_ANCHOR"],
                     str(fd), json.dumps([ready_write]),
                     json.dumps([sys.executable, "-c", child_code, str(pid_path),
                                 str(ready_write), grandchild_code])],
                    start_new_session=True, pass_fds=(fd, ready_write))
                os.close(ready_write)

                def ack_liveness():
                    if os.read(ping_read, 1) == b"p":
                        os.write(pong_write, b"P")

                threading.Thread(target=ack_liveness, daemon=True).start()
                result = anchor_proc.wait()
                grandchild_pid = int(pid_path.read_text())
                adopted_status = helpers.reap_adopted_descendant(grandchild_pid, timeout=5)
                while True:
                    try:
                        reaped, _ = os.waitpid(-1, os.WNOHANG)
                    except ChildProcessError:
                        break
                    if reaped == 0:
                        break
                result_path.write_text(json.dumps([result, adopted_status]))
            ''').lstrip("\n"))
            ready_read, ready_write = os.pipe()
            ping_read, ping_write = os.pipe()
            pong_read, pong_write = os.pipe()
            open_fds = {ready_read, ready_write, ping_read, ping_write, pong_read, pong_write}

            def close_fd(fd):
                if fd in open_fds:
                    open_fds.remove(fd)
                    os.close(fd)

            proc = None
            try:
                proc = spawn_fixture_helper(
                    [sys.executable, str(helper), str(SCRIPTS / "test-ci-test-affected.py"),
                     str(root / "grandchild.pid"), str(root / "result"),
                     str(ready_write), str(ping_read), str(pong_write)],
                    pass_fds=(ready_write, ping_read, pong_write))
                for child_fd in (ready_write, ping_read, pong_write):
                    close_fd(child_fd)

                ready, _, _ = select.select([ready_read], [], [], self.DEADLINE_SECONDS)
                self.assertTrue(ready, "watchdog fixture descendant did not become ready")
                self.assertEqual(os.read(ready_read, 1), b"R")

                proc.send_signal(signal.SIGTERM)
                self.assertEqual(os.write(ping_write, b"p"), 1)
                pong, _, _ = select.select([pong_read], [], [], self.DEADLINE_SECONDS)
                self.assertTrue(pong, "supervisor did not survive SIGTERM")
                self.assertEqual(os.read(pong_read, 1), b"P")
                self.assertIsNone(proc.poll())

                proc.fixture_watchdog.sendall(b"x")
                stdout, stderr = proc.communicate(timeout=self.DEADLINE_SECONDS)
                self.assertEqual(proc.returncode, 0, (stdout + stderr).decode(errors="replace"))
                self.assertEqual(json.loads((root / "result").read_text()), [-signal.SIGKILL, -signal.SIGKILL])
                grandchild_pid = int((root / "grandchild.pid").read_text())
                with self.assertRaises(ProcessLookupError):
                    os.kill(grandchild_pid, 0)
            finally:
                for fd in list(open_fds):
                    close_fd(fd)
                if proc is not None:
                    stop_owned_helper(proc)

    def test_watchdog_ordering_separates_cancellation_adoption_and_timeout_budget(self):
        """Controlled-clock proof of the -9 failure ordering and separated budgets."""
        class FakeClock:
            def __init__(self):
                self.now = 0.0

            def monotonic(self):
                return self.now

            def sleep(self, seconds):
                self.now += seconds

        class FakeSocket:
            def __init__(self, on_send):
                self.on_send = on_send
                self.sent = []
                self.closed = False

            def sendall(self, data):
                self.sent.append(data)
                self.on_send(data)

            def close(self):
                self.closed = True

        class FakeSupervisor:
            def __init__(self, clock, *, exit_after_watchdog=None):
                self.clock = clock
                self.exit_after_watchdog = exit_after_watchdog
                self.returncode = None
                self.signals = []
                self.watchdog_at = None
                self.fixture_watchdog = FakeSocket(self._on_watchdog)

            def _on_watchdog(self, _data):
                self.watchdog_at = self.clock.now

            def send_signal(self, sig):
                self.signals.append((sig, self.clock.now))

            def kill(self):
                self.signals.append((signal.SIGKILL, self.clock.now))
                self.returncode = -signal.SIGKILL

            def communicate(self, timeout=None):
                if self.returncode is not None:
                    return b"", b""
                if self.watchdog_at is not None and self.exit_after_watchdog is not None:
                    elapsed = self.clock.now - self.watchdog_at
                    if timeout is None or self.exit_after_watchdog <= elapsed + timeout:
                        self.clock.sleep(max(0.0, self.exit_after_watchdog - elapsed))
                        self.returncode = 0
                        return b"", b""
                if timeout is not None:
                    self.clock.sleep(timeout)
                    raise subprocess.TimeoutExpired(["broken.py"], timeout)
                return b"", b""

        # Case 1: If ANCHOR reaps the tracked child (yielding ECHILD) or adoption exceeds
        # stop_owned_helper's 5s watchdog_timeout, stop_owned_helper calls proc.kill() -> -9.
        clock = FakeClock()
        preempted = FakeSupervisor(clock, exit_after_watchdog=5.1)
        stop_owned_helper(preempted, sigterm_timeout=5, watchdog_timeout=5)
        self.assertEqual(preempted.returncode, -signal.SIGKILL)
        self.assertEqual(preempted.signals, [(signal.SIGTERM, 0.0), (signal.SIGKILL, 10.0)])
        self.assertEqual(preempted.fixture_watchdog.sent, [b"x", b"x"])
        self.assertTrue(preempted.fixture_watchdog.closed)

        # Case 2: Transient ECHILD (while intermediate child exits) then WNOHANG=0 then SIGKILL
        # adoption/reaping succeeds deterministically within budget.
        adopt_clock = FakeClock()
        sigkill_status = os.W_EXITCODE(0, signal.SIGKILL) if hasattr(os, "W_EXITCODE") else signal.SIGKILL
        outcomes = iter([ChildProcessError(10, "No child processes"), (0, 0), (4321, sigkill_status)])

        def scripted_waitpid(pid, flags):
            self.assertEqual((pid, flags), (4321, os.WNOHANG))
            outcome = next(outcomes)
            if isinstance(outcome, BaseException):
                raise outcome
            return outcome

        exitcode = reap_adopted_descendant(
            4321, timeout=5, poll_interval=0.01, waitpid=scripted_waitpid,
            monotonic=adopt_clock.monotonic, sleep=adopt_clock.sleep)
        self.assertEqual(exitcode, -signal.SIGKILL)
        self.assertAlmostEqual(adopt_clock.monotonic(), 0.02)

        # Case 3: With helper adoption_timeout (5s) strictly inside outer deadline (10s),
        # an un-adopted descendant raises TimeoutError at 5s rather than being masked as -9.
        timeout_clock = FakeClock()

        def never_adopted(_pid, _flags):
            raise ChildProcessError(10, "No child processes")

        with self.assertRaises(TimeoutError):
            reap_adopted_descendant(
                4321, timeout=5, poll_interval=0.5, waitpid=never_adopted,
                monotonic=timeout_clock.monotonic, sleep=timeout_clock.sleep)
        self.assertEqual(timeout_clock.monotonic(), 5.0)
        bounded = FakeSupervisor(FakeClock(), exit_after_watchdog=5.0)
        stop_owned_helper(bounded, sigterm_timeout=5, watchdog_timeout=10)
        self.assertEqual(bounded.returncode, 0)

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
                def owned_popen(*args, **kwargs):
                    fd = int(os.environ["ORBIT_FIXTURE_WATCHDOG_FD"])
                    fds = tuple(kwargs.get("pass_fds") or ())
                    kwargs["pass_fds"] = fds + (fd,)
                    command = [sys.executable, "-c", os.environ["ORBIT_FIXTURE_ANCHOR"],
                               str(fd), json.dumps(fds), json.dumps(args[0])]
                    return real_popen(command, **kwargs)
                def recording_popen(*args, **kwargs):
                    child = owned_popen(*args, **kwargs)
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
                    (root / "child.pid").unlink(missing_ok=True)
                    (root / "result.json").write_text(json.dumps({
                        "code": caught.code, "returncode": children[0].poll(),
                        "signal_restored": signal.getsignal(signal.SIGTERM) == previous}))
            ''').lstrip("\n"))
            proc = spawn_fixture_helper(
                [sys.executable, str(helper), str(SCRIPTS / "ci-test-affected.py"), str(root)])
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
                stop_owned_helper(proc)

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
                def owned_popen(*args, **kwargs):
                    fd = int(os.environ["ORBIT_FIXTURE_WATCHDOG_FD"])
                    fds = tuple(kwargs.get("pass_fds") or ())
                    kwargs["pass_fds"] = fds + (fd,)
                    command = [sys.executable, "-c", os.environ["ORBIT_FIXTURE_ANCHOR"],
                               str(fd), json.dumps(fds), json.dumps(args[0])]
                    return real_popen(command, **kwargs)
                def recording_popen(*args, **kwargs):
                    kwargs["pass_fds"] = tuple(kwargs.get("pass_fds") or ()) + (ack_read, ready_write)
                    child = owned_popen(*args, **kwargs)
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
                    deadline = time.monotonic() + 5
                    while True:
                        try:
                            reaped, status = os.waitpid(grandchild_pid, os.WNOHANG)
                        except (ChildProcessError, InterruptedError):
                            reaped = 0
                        if reaped == grandchild_pid:
                            break
                        if time.monotonic() >= deadline:
                            raise TimeoutError("relay-test descendant was not adopted and reaped")
                        time.sleep(0.01)
                    result["grandchild_killed"] = reaped == grandchild_pid and os.waitstatus_to_exitcode(status) == -signal.SIGKILL
                for name in ("child.pid", "grandchild.pid"):
                    (root / name).unlink(missing_ok=True)
                (root / "result.json").write_text(json.dumps(result))
                # Restore real streams before Python's final flush.
                sys.stdout, sys.stderr = sys.__stdout__, sys.__stderr__
            ''').lstrip("\n"))
            for stream, operation, cleanup_error in (
                    ("stdout", "write", "no"), ("stderr", "write", "no"),
                    ("stdout", "flush", "no"), ("stdout", "write", "yes")):
                with self.subTest(stream=stream, operation=operation, cleanup_error=cleanup_error):
                    proc = spawn_fixture_helper(
                        [sys.executable, str(fixture), "helper", str(SCRIPTS / "ci-test-affected.py"),
                         str(root), stream, operation, cleanup_error])
                    try:
                        stdout, stderr = proc.communicate(timeout=self.DEADLINE_SECONDS)
                    except subprocess.TimeoutExpired:
                        self.fail("run_observed hung after a relay failure with a full pipe")
                    finally:
                        stop_owned_helper(proc)
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
                fd = int(os.environ["ORBIT_FIXTURE_WATCHDOG_FD"])
                fds = tuple(kwargs.get("pass_fds") or ())
                kwargs["pass_fds"] = fds + (fd,)
                command = [sys.executable, "-c", os.environ["ORBIT_FIXTURE_ANCHOR"],
                           str(fd), json.dumps(fds), json.dumps(args[0])]
                child = real_popen(command, **kwargs)
                Path(sys.argv[3]).with_suffix(".pid").write_text(str(child.pid))
                return child
            module.subprocess.Popen = recording_popen
            code, output = module.run_observed([sys.executable, "-u", sys.argv[2]], os.environ)
            Path(sys.argv[3]).with_suffix(".pid").unlink(missing_ok=True)
            Path(sys.argv[3]).write_text(json.dumps({"exit_code": code, "output": output}))
        ''').lstrip("\n"))
        proc = spawn_fixture_helper(
            [sys.executable, str(helper), str(SCRIPTS / "ci-test-affected.py"), str(child), str(result_path)])
        try:
            stdout, stderr = proc.communicate(timeout=self.DEADLINE_SECONDS)
        except subprocess.TimeoutExpired:
            self.fail("run_observed blocked past its deadline on a partial stdout line "
                      "while stderr held more than a pipe of data")
        finally:
            stop_owned_helper(proc)
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

    def test_reaped_leader_is_not_signaled_after_its_group_id_is_released(self):
        """Cleanup signals the session leader's group before reaping releases its PID."""
        import importlib.util
        from types import SimpleNamespace

        spec = importlib.util.spec_from_file_location(
            "ci_test_affected_ownership", SCRIPTS / "ci-test-affected.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        state = {"reserved": True, "owner": "original", "leader_unreaped": True, "pid": 4242}
        signals = []
        foreign = []
        popen_calls = []

        class Pipe:
            def __init__(self, line):
                self.line = line

            def readline(self):
                line, self.line = self.line, b""
                return line

            def close(self):
                return None

        class Process:
            def __init__(self):
                self.pid = state["pid"]
                self.args = ["observed"]
                self.returncode = None
                self.stdout = Pipe(b"line\n")
                self.stderr = Pipe(b"")

            def wait(self, timeout=None):
                self.returncode = 0
                state["leader_unreaped"] = False
                state["reserved"] = False
                state["owner"] = "replacement"
                return 0

            def kill(self):
                if not state["leader_unreaped"]:
                    foreign.append(("kill", self.pid, state["owner"]))
                self.returncode = -signal.SIGKILL
                state["leader_unreaped"] = False

        def fake_killpg(pid, sig):
            owned = (state["reserved"] and pid == state["pid"]
                     and state["owner"] == "original" and state["leader_unreaped"])
            record = (pid, sig, state["owner"], state["leader_unreaped"])
            if owned:
                signals.append(record)
            else:
                foreign.append(record)
            if sig == signal.SIGKILL:
                state["reserved"] = False
                state["owner"] = "released"

        def fake_popen(*args, **kwargs):
            popen_calls.append(kwargs)
            return Process()

        class Relay:
            def write(self, raw):
                raise OSError("injected relay failure")

            def flush(self):
                return None

        class Sink:
            def write(self, raw):
                return len(raw)

            def flush(self):
                return None

        with mock.patch.object(module.subprocess, "Popen", side_effect=fake_popen), \
             mock.patch.object(module.os, "killpg", side_effect=fake_killpg), \
             mock.patch.object(module.sys, "stdout",
                               SimpleNamespace(buffer=Relay(), write=lambda text: None, flush=lambda: None)), \
             mock.patch.object(module.sys, "stderr",
                               SimpleNamespace(buffer=Sink(), write=lambda text: None, flush=lambda: None)):
            with self.assertRaises(OSError) as caught:
                module.run_observed(["observed"], {})
        self.assertEqual(str(caught.exception), "injected relay failure")
        self.assertEqual(len(popen_calls), 1)
        self.assertIs(popen_calls[0].get("start_new_session"), True)
        self.assertEqual(signals, [
            (4242, signal.SIGTERM, "original", True),
            (4242, signal.SIGKILL, "original", True),
        ])
        self.assertEqual(foreign, [])

    @unittest.skipUnless(sys.platform == "linux", "requires waitid WNOWAIT")
    def test_self_killed_group_keeps_unreaped_leader_until_escalation(self):
        import importlib.util
        from types import SimpleNamespace

        spec = importlib.util.spec_from_file_location(
            "ci_test_affected_self_kill", SCRIPTS / "ci-test-affected.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        real_popen = subprocess.Popen
        children = []
        signals = []
        original = BrokenPipeError("relay failure after group death")

        def recording_popen(*args, **kwargs):
            child = real_popen(*args, **kwargs)
            children.append(child)
            return child

        class Relay:
            def write(self, raw):
                # Observe exit without reaping; the command has killed its
                # whole group before the supervisor starts cleanup.
                os.waitid(os.P_PID, children[0].pid, os.WEXITED | os.WNOWAIT)
                raise original

            def flush(self):
                pass

        class Sink:
            def write(self, raw):
                return len(raw)

            def flush(self):
                pass

        def intercepted_killpg(pid, sig):
            # This succeeds only while the leader remains our unreaped child.
            info = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT)
            signals.append((sig, info.si_pid))

        with mock.patch.object(module.subprocess, "Popen", side_effect=recording_popen), \
             mock.patch.object(module.os, "killpg", side_effect=intercepted_killpg), \
             mock.patch.object(module.sys, "stdout", SimpleNamespace(buffer=Relay())), \
             mock.patch.object(module.sys, "stderr", SimpleNamespace(buffer=Sink())):
            with self.assertRaises(BrokenPipeError) as caught:
                module.run_observed([sys.executable, "-c",
                    "import os, signal; os.write(1, b'ready\\n'); "
                    "os.killpg(os.getpgrp(), signal.SIGKILL)"], os.environ)
        self.assertIs(caught.exception, original)
        self.assertEqual(signals, [(signal.SIGTERM, children[0].pid),
                                   (signal.SIGKILL, children[0].pid)])
        self.assertEqual(children[0].returncode, -signal.SIGKILL)


class NextestRelayBoundTests(unittest.TestCase):
    """Drive run_nextest with a scripted runner stream: no build and no child.

    The stream hands out exactly the scripted chunks, so where a line is split
    between two reads is fixed, and it refuses a read without a size bound.
    """

    NOTICE = "DEFERRED: bubblewrap unavailable: fixture: No permissions to create new namespace"
    PASS = "        PASS [   0.010s] suite::fixture quiet\n"
    FAIL = "        FAIL [   0.010s] suite::fixture broken\n"

    def relay(self, chunks, exit_code=0):
        """Returns the exit status, notices, relayed stderr text and archived bytes."""
        import importlib.util
        import io
        from collections import deque
        from types import SimpleNamespace

        spec = importlib.util.spec_from_file_location("ci_test_affected_relay", SCRIPTS / "ci-test-affected.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="nextest-relay-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        module.ROOT = Path(temporary.name)
        testcase = self
        self.reads, self.writes = [], []

        class Stream:
            def __init__(self):
                self.chunks = deque(chunks)

            def read1(self, size=-1):
                if size is None or size < 0:
                    raise AssertionError("the relay read without a size bound")
                testcase.reads.append(size)
                if not self.chunks:
                    return b""
                chunk = self.chunks.popleft()
                if len(chunk) > size:
                    self.chunks.appendleft(chunk[size:])
                    chunk = chunk[:size]
                return chunk

            def unbounded(self, *args):
                raise AssertionError("the relay read a whole line or stream without a size bound")

            __iter__ = __next__ = readline = readlines = read = unbounded

        class Process:
            stdout = Stream()

            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def wait(self):
                return exit_code

            def terminate(self):
                pass

        class Relay(io.StringIO):
            def write(self, text):
                testcase.writes.append(len(text))
                return super().write(text)

        module.subprocess = SimpleNamespace(Popen=lambda *args, **kwargs: Process(),
                                            PIPE=subprocess.PIPE, STDOUT=subprocess.STDOUT)
        relayed = Relay()
        environment = {name: value for name, value in os.environ.items()
                       if not name.startswith("ORBIT_COMMAND_EVIDENCE_")}
        with mock.patch.dict(os.environ, environment, clear=True), contextlib.redirect_stderr(relayed):
            code, notices, _ = module.run_nextest(["fixture-runner"], environment)
        [log] = (module.ROOT / ".orbit/tmp").glob("ci-test-affected-nextest-*.log")
        self.module = module
        lines = relayed.getvalue().splitlines(keepends=True)
        self.assertTrue(lines[0].startswith("ci-test-affected: full nextest output: "), lines[:1])
        return code, notices, "".join(lines[1:]), log.read_bytes()

    def test_split_reads_relay_and_classify_like_whole_lines(self):
        stream = (
            self.PASS + "ordinary passing noise\n"
            + f"\x1b[31m{self.NOTICE}\x1b[0m\n"
            + "  skipping: second notice é\n"
            + "fixture: sandbox_apply: Operation not permitted\n"
            + self.FAIL + "failure détail ✓\n"
            + "     Summary [   0.020s] 2 tests run: 1 passed, 1 failed\n"
            + "unterminated tail").encode()
        expected = (
            self.PASS + self.NOTICE + "\n" + "  skipping: second notice é\n"
            + "fixture: sandbox_apply: Operation not permitted\n"
            + self.FAIL + "failure détail ✓\n"
            + "     Summary [   0.020s] 2 tests run: 1 passed, 1 failed\n"
            + "unterminated tail")
        for size in (len(stream), 1, 2, 7, 64):
            with self.subTest(read_size=size):
                chunks = [stream[index:index + size] for index in range(0, len(stream), size)]
                code, notices, relayed, archived = self.relay(chunks, exit_code=100)
                self.assertEqual(code, 100)
                self.assertEqual(notices, [self.NOTICE, "skipping: second notice é"])
                self.assertEqual(relayed, expected)
                self.assertEqual(archived, stream)

    def test_an_endless_line_is_read_relayed_and_archived_in_bounded_pieces(self):
        megabyte = 1024 * 1024
        marker = b"No permissions to create a new namespace"
        stream = (self.PASS.encode() + b"p" * megabyte + marker + b"q" * megabyte + b"\n"
                  + self.FAIL.encode() + "é".encode() * megabyte + b"\n"
                  + b"DEFERRED: " + b"n" * megabyte + b"\n"
                  + self.NOTICE.encode() + b"\n")
        code, notices, relayed, archived = self.relay([stream])
        bound = self.module.RELAY_CHUNK_BYTES
        self.assertEqual(code, 0)
        self.assertEqual(archived, stream, "the full raw log keeps every byte")
        self.assertLessEqual(max(self.reads), bound)
        self.assertLessEqual(max(self.writes), bound)
        # The failing line and the notice line are relayed whole; of the
        # passing line only the piece that carries the host marker is.
        self.assertEqual(relayed.count("é"), megabyte)
        self.assertNotIn("�", relayed, "a piece boundary must not split a character")
        self.assertEqual(relayed.count("n" * 1024), megabyte // 1024)
        self.assertIn(marker.decode(), relayed)
        self.assertLessEqual(relayed.count("p") + relayed.count("q"), bound)
        cut = self.module.HOST_NOTICE_CUT
        self.assertEqual(notices, ["DEFERRED: " + "n" * (self.module.HOST_NOTICE_CHARS - 10) + cut, self.NOTICE])

    def test_notices_over_the_bound_are_counted_in_a_final_entry(self):
        limit = None
        for extra in (0, 5):
            with self.subTest(extra=extra):
                if limit is None:
                    self.relay([b""])
                    limit = self.module.HOST_NOTICE_LIMIT
                lines = [f"DEFERRED: bubblewrap unavailable: test {index}: refused\n"
                         for index in range(limit + extra)]
                stream = (self.PASS + "".join(lines)).encode()
                code, notices, relayed, archived = self.relay([stream])
                self.assertEqual(code, 0)
                self.assertEqual(archived, stream)
                self.assertEqual(notices[:limit], [line.strip() for line in lines[:limit]])
                for line in lines:
                    self.assertIn(line, relayed)
                if not extra:
                    self.assertEqual(len(notices), limit)
                    self.assertNotIn("not listed", relayed)
                    continue
                [overflow] = notices[limit:]
                self.assertTrue(overflow.startswith(f"ci-test-affected: {extra} more host notices are not listed"),
                                overflow)
                # Not a deferral: a reader requiring every notice to be one refuses.
                self.assertFalse(overflow.startswith(("DEFERRED:", "SKIP:", "skipping")))
                self.assertIn(overflow + "\n", relayed)


if __name__ == "__main__":
    unittest.main()
