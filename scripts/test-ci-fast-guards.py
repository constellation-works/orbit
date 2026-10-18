#!/usr/bin/env python3
"""Exercise guardrail routing, source registration and dependency policy."""

import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


class GuardrailTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="ci-fast-guards-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.guard_calls = self.root / "guard-calls"
        self.guard_calls.mkdir()
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "cargo.log"
        self.metadata = self.root / "metadata.json"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        GIT_CEILING_DIRECTORIES=str(scratch),
                        GUARD_TEST_LOG=str(self.log), GUARD_TEST_METADATA=str(self.metadata),
                        GUARD_TEST_BIN=str(self.bin / "fixture-test-bin"),
                        GUARD_TEST_CALLS=str(self.guard_calls))
        # Pin the guard pool's width and ignore any host build-budget file.
        self.env.update(ORBIT_CARGO_JOBS="4", ORBIT_BUILD_BUDGET_DIR=str(self.root / "budget"))
        self.targets = self.root / "targets.json"
        self.env["GUARD_TEST_TARGETS"] = str(self.targets)
        # A miniature libtest. Without a targets file every listing reports one
        # `fixture_test`. With one (see `set_targets`) it honours target
        # selection, the positional filter and `--exact`, so a guard that drops
        # any of them lists different tests than the workflow would run.
        self.write_executable(self.bin / "fixture_libtest.py", '''import json, os

def load_targets():
    path = os.environ["GUARD_TEST_TARGETS"]
    return json.load(open(path)) if os.path.exists(path) else None

def matches(test, filters, libtest_args):
    if not filters:
        return True
    return test == filters[0] if "--exact" in libtest_args else filters[0] in test

def list_target(target, filters, libtest_args):
    return [test + ": test" for test in target["tests"] if matches(test, filters, libtest_args)]

def cargo_listing(targets, cargo_args, libtest_args):
    if targets is None:
        return ["fixture_test: test"]
    selectors, filters, index = [], [], 0
    while index < len(cargo_args):
        token = cargo_args[index]
        if token in ("--test", "--bin"):
            selectors.append((token[2:], cargo_args[index + 1]))
            index += 1
        elif token in ("--lib", "--bins"):
            selectors.append((token[2:].rstrip("s"), None))
        elif token == "-p":
            index += 1
        elif not token.startswith("-"):
            filters.append(token)
        index += 1
    lines = []
    for target in targets:
        if selectors and not any(kind == target["kind"] and name in (None, target["name"])
                                 for kind, name in selectors):
            continue
        lines += list_target(target, filters, libtest_args)
    return lines
''')
        self.write_executable(self.bin / "cargo", '''#!/usr/bin/env python3
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixture_libtest
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
env_log = os.environ.get("GUARD_TEST_ENV_LOG")
if env_log:
    # Linux uses the idle class; elsewhere nice -n 19 lowers inherited priority.
    idle = getattr(os, "SCHED_IDLE", None)
    with open(env_log, "a") as log:
        log.write(json.dumps({"argv": sys.argv[1:],
                              "RUSTC_WORKSPACE_WRAPPER": os.environ.get("RUSTC_WORKSPACE_WRAPPER"),
                              "CLIPPY_ARGS": os.environ.get("CLIPPY_ARGS"),
                              "idle": idle is not None and os.sched_getscheduler(0) == idle,
                              "nice": os.getpriority(os.PRIO_PROCESS, 0)}) + "\\n")
if sys.argv[1] == "metadata":
    print(open(os.environ["GUARD_TEST_METADATA"]).read())
elif sys.argv[1] == "test" and "--message-format" in sys.argv:
    # `--no-run --message-format json`: report the test binaries of every
    # package named in the fixture metadata, the way a workspace build does.
    targets = fixture_libtest.load_targets()
    if targets is None:
        targets = [dict(name="fixture", kind="lib", binary=os.environ["GUARD_TEST_BIN"])]
    for package in json.load(open(os.environ["GUARD_TEST_METADATA"]))["packages"]:
        for target in targets:
            print(json.dumps({"reason": "compiler-artifact", "package_id": package["id"],
                              "profile": {"test": True}, "executable": target["binary"],
                              "target": {"name": target["name"], "kind": [target["kind"]]}}))
elif sys.argv[1] == "test":
    args = sys.argv[2:]
    split = args.index("--") if "--" in args else len(args)
    for line in fixture_libtest.cargo_listing(fixture_libtest.load_targets(), args[:split], args[split:]):
        print(line)
''')
        # Stands in for a compiled test binary: logs its argv like the cargo
        # stub and lists the tests of the target it was built for.
        self.write_executable(self.bin / "fixture-test-bin", '''#!/usr/bin/env python3
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixture_libtest
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps([os.path.basename(sys.argv[0])] + sys.argv[1:]) + "\\n")
targets = fixture_libtest.load_targets()
if targets is None:
    print("fixture_test: test")
else:
    target = next(t for t in targets if t["binary"].endswith(os.path.basename(sys.argv[0])))
    args = sys.argv[1:]
    filters = [a for a in args if not a.startswith("-")]
    for line in fixture_libtest.list_target(target, filters, args):
        print(line)
''')
        self.write_executable(self.bin / "rg", "#!/bin/bash\nexit 1\n")

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def set_targets(self, *targets):
        """Give the fixture package these (name, kind, tests) test targets."""
        entries = []
        for name, kind, tests in targets:
            binary = self.bin / f"fixture-test-bin-{name}"
            shutil.copy2(self.bin / "fixture-test-bin", binary)
            entries.append(dict(name=name, kind=kind, tests=tests, binary=str(binary)))
        self.targets.write_text(json.dumps(entries))

    def set_macos_step(self, command):
        workflow = self.root / ".github/workflows/ci-macos.yml"
        workflow.write_text(workflow.read_text().replace(
            "cargo test -p orbit-types fixture_test", command))

    def run_guard(self, name, *arguments, env_overrides=None):
        for call in self.guard_calls.iterdir():
            call.unlink()
        shutil.copy2(SCRIPTS / name, self.scripts / name)
        interpreter = "python3" if name.endswith(".py") else "/bin/bash"
        return subprocess.run([interpreter, str(self.scripts / name), *arguments],
                              env=dict(self.env, **(env_overrides or {})),
                              text=True, capture_output=True)

    def prepare_ci(self):
        source = (SCRIPTS / "ci-guardrails.sh").read_text()
        for name in re.findall(r'\$repo_root/scripts/([\w.-]+)', source):
            self.write_executable(self.scripts / name,
                                  f'#!/bin/bash\nprintf "%s\\n" "$@" > "$GUARD_TEST_CALLS/{name}"\n')
        shutil.copy2(SCRIPTS / "ci-guardrails.sh", self.scripts / "ci-guardrails.sh")
        shutil.copy2(SCRIPTS / "check-ci-macos.sh", self.scripts / "check-ci-macos.sh")
        # The lint wrapper is the environment under test, not a guard.
        shutil.copy2(SCRIPTS / "with-test-build-lint.sh", self.scripts / "with-test-build-lint.sh")
        # The pool sizes itself from the real budget query, so keep the real script.
        shutil.copy2(SCRIPTS / "build-budget.py", self.scripts / "build-budget.py")
        # The full run wraps nextest in the real sampler. A stub would swallow
        # the cargo invocation the assertions below observe.
        shutil.copy2(SCRIPTS / "cpu-utilisation.sh", self.scripts / "cpu-utilisation.sh")
        workflows = self.root / ".github/workflows"
        workflows.mkdir(parents=True, exist_ok=True)
        (self.root / "Cargo.toml").touch()
        self.metadata.write_text(json.dumps(dict(
            packages=[dict(id="path+file:///fixture/orbit-types#0.1.0", name="orbit-types")])))
        (workflows / "ci-macos.yml").write_text('''on:
  pull_request:
jobs:
  test:
    steps:
      - run: cargo test -p orbit-types fixture_test
''')

    def init_ci_git(self):
        self.env.pop("CI_TEST_BASE", None)
        self.env["GIT_CEILING_DIRECTORIES"] = str(self.root.parent)
        self.git("init", "-q", "-b", "agent-main")
        self.git("config", "user.name", "Guard fixture")
        self.git("config", "user.email", "guard@example.invalid")
        (self.root / ".gitignore").write_text(".orbit/\nbin/\nguard-calls/\n*.log\n")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture base")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.env["CI_TEST_BASE"] = self.base
        self.git("checkout", "-q", "--detach")
        # A committed crates-only change still belongs to the candidate diff.
        self.crate_input = self.root / "crates/example/Cargo.toml"
        self.crate_input.parent.mkdir(parents=True)
        self.crate_input.write_text("# crates-only change\n")
        self.git("add", "crates")
        self.git("commit", "-qm", "crate candidate")

    def git(self, *arguments):
        result = subprocess.run(["git", "-C", str(self.root), *arguments],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def assert_self_tests(self, ran):
        # Discover the fixture's executable tests; assertions are about what
        # the runner executed, not a pinned source-list or count.
        expected = {path.name for path in self.scripts.glob("test-*")
                    if path.name != "test-qa-full-sweep.py"}
        actual = {path.name for path in self.guard_calls.iterdir()} & expected
        self.assertEqual(actual, expected if ran else set())
        self.assertTrue((self.guard_calls / "test-qa-full-sweep.py").exists(),
                        "the live surface inventory must run on crates changes")

    def prepare_python_prerequisite(self):
        self.prepare_ci()
        shutil.copy2(SCRIPTS / "require-python.sh", self.scripts)
        shutil.copy2(SCRIPTS.parent / "Makefile", self.root)
        # Only version/module probes are stubbed. Other Python consumers
        # (including the Cargo stub and runner cleanup) use the real interpreter.
        self.yaml_probe = self.root / "yaml-probe.log"
        self.env["GUARD_TEST_YAML_PROBE"] = str(self.yaml_probe)
        self.write_executable(self.bin / "python3", f'''#!/bin/sh
if [ "$1" = -c ]; then
  case "$2" in
    *sys.version_info*) echo 3.14.7; exit 0 ;;
    'import yaml')
      echo probe >> "$GUARD_TEST_YAML_PROBE"
      exit "$GUARD_TEST_YAML_STATUS" ;;
  esac
fi
exec {shlex.quote(sys.executable)} "$@"
''')
        self.write_executable(self.bin / "budget", '#!/bin/sh\nshift\nexec "$@"\n')
        self.init_ci_git()

    def test_selected_yaml_prerequisite_stops_before_cargo_and_guards(self):
        """Missing PyYAML must refuse before work, not skip mandatory fixtures."""
        self.prepare_python_prerequisite()
        (self.scripts / "changed-input.sh").touch()
        commands = (("make", "ci-fast", f"BUILD_BUDGET={self.bin / 'budget'}"),
                    ("/bin/bash", str(self.scripts / "ci-guardrails.sh"), "--fast"),
                    ("/bin/bash", str(self.scripts / "ci-guardrails.sh")))
        for command in commands:
            for available in (False, True):
                with self.subTest(command=command, yaml=available):
                    self.log.unlink(missing_ok=True)
                    self.yaml_probe.unlink(missing_ok=True)
                    for call in self.guard_calls.iterdir():
                        call.unlink()
                    result = subprocess.run(
                        command, cwd=self.root, text=True, capture_output=True,
                        env=dict(self.env, GUARD_TEST_YAML_STATUS="0" if available else "1"))
                    self.assertEqual(result.returncode == 0, available, result.stderr)
                    if available:
                        self.assertTrue(self.yaml_probe.exists())
                        self.assert_self_tests(True)
                        self.assertTrue((self.guard_calls / "test-ci-download-verified.py").exists())
                    else:
                        self.assertIn("PyYAML", result.stderr)
                        self.assertIn(f"python3 3.14.7 at {self.bin}/python3", result.stderr)
                        self.assertIn("-m pip install PyYAML", result.stderr)
                        self.assertIn("venv", result.stderr)
                        self.assertFalse(self.log.exists(), "Cargo ran before YAML preflight")
                        self.assertEqual(list(self.guard_calls.iterdir()), [],
                                         "a guard/fixture ran before YAML preflight")
                        self.assertNotIn("SKIPPED", result.stdout)

    def test_crates_only_fast_selection_does_not_require_yaml(self):
        self.prepare_python_prerequisite()
        self.env["GUARD_TEST_YAML_STATUS"] = "1"
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.yaml_probe.exists(), "unselected fixtures imposed YAML dependency")
        self.assert_self_tests(False)

    def test_fast_crates_only_skips_self_tests_for_explicit_and_merge_bases(self):
        self.prepare_ci()
        self.init_ci_git()
        # Include unstaged and untracked crates paths as well as the commit.
        self.crate_input.write_text("# unstaged\n")
        (self.crate_input.parent / "untracked.rs").touch()
        for explicit in (True, False):
            with self.subTest(explicit=explicit):
                if not explicit:
                    self.env.pop("CI_TEST_BASE")
                result = self.run_guard("ci-guardrails.sh", "--fast")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assert_self_tests(False)

    def test_fast_changed_inputs_run_every_self_test(self):
        self.prepare_ci()
        self.init_ci_git()
        for path in ("scripts/new-guard.sh", ".github/new.yml", "Makefile",
                     "install.sh", "RELEASING.md", "plugin/new.json", ".cargo/config.toml"):
            with self.subTest(path=path):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("# changed input\n")
                result = self.run_guard("ci-guardrails.sh", "--fast")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assert_self_tests(True)
                target.unlink()
        # Test the tracked path query independently of untracked inputs.
        target = self.scripts / "test-build-budget.sh"
        target.write_text(target.read_text() + "# changed\n")
        self.git("add", "scripts")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_self_tests(True)
        self.git("commit", "-qm", "script change")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_self_tests(True)

    def test_fast_deleted_and_renamed_inputs_run_self_tests(self):
        self.prepare_ci()
        self.init_ci_git()
        target = self.scripts / "fixture-input.txt"
        target.touch()
        self.git("add", "scripts")
        self.git("commit", "-qm", "tracked input")
        self.env["CI_TEST_BASE"] = self.git("rev-parse", "HEAD").stdout.strip()
        target.rename(self.crate_input.parent / "moved-input.txt")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_self_tests(True)

    def test_unresolvable_base_runs_every_self_test(self):
        self.prepare_ci()
        self.init_ci_git()
        self.env["CI_TEST_BASE"] = "missing-delivery-base"
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_self_tests(True)

    def test_full_ci_runs_every_self_test_on_crates_only_diff(self):
        self.prepare_ci()
        # The last pool member holds so a pool that outlived the builds would
        # still be running when nextest starts.
        self.write_executable(self.scripts / "test-cpu-utilisation.py", '''#!/usr/bin/env python3
import json, os, time
time.sleep(0.5)
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["guard-end", "test-cpu-utilisation.py"]) + "\\n")
open(os.path.join(os.environ["GUARD_TEST_CALLS"], "test-cpu-utilisation.py"), "w").close()
''')
        # The exclusive lane's last suite holds too, so a lane that outlived
        # the builds would still be running when nextest starts.
        self.write_executable(self.scripts / "test-compiler-cache-namespaces.sh", '''#!/usr/bin/env python3
import json, os, time
time.sleep(0.5)
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["guard-end", "test-compiler-cache-namespaces.sh"]) + "\\n")
open(os.path.join(os.environ["GUARD_TEST_CALLS"], "test-compiler-cache-namespaces.sh"), "w").close()
''')
        self.init_ci_git()
        result = self.run_guard("ci-guardrails.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_self_tests(True)
        phases = re.findall(r"ci-guardrails-phase: (\S+) wall_seconds=\d+ exit=(\d+)", result.stderr)
        self.assertIn(("clippy-production", "0"), phases)
        self.assertNotIn("clippy-all-targets", [name for name, _exit in phases])
        self.assertIn(("workspace-tests", "0"), phases)
        self.assertIn(("repository-guards", "0"), phases)
        self.assertRegex(
            result.stderr,
            r"cpu-util: label=nextest (busy_pct=\d+\.\d+ idle_pct=\d+\.\d+ "
            r"iowait_pct=\d+\.\d+|unavailable=(unchanged|decreased|proc_stat))",
        )
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        production_clippy = ["check", "--workspace", "--lib", "--bins"]
        self.assertIn(production_clippy, calls)
        self.assertFalse(any("--all-targets" in call for call in calls))
        listing = ["nextest", "list", "--list-type", "binaries-only", "--message-format", "json",
                   "--workspace", "--lib", "--bins", "--tests"]
        self.assertIn(listing, calls)
        runs = [call for call in calls if call[:2] == ["nextest", "run"]]
        self.assertEqual(len(runs), 1, calls)
        run = runs[0]
        self.assertEqual(run[:7], ["nextest", "run", "--profile", "ci", "--no-fail-fast",
                                   "--success-output", "immediate"])
        # nextest runs the listed binaries instead of building its own set.
        self.assertIn("--binaries-metadata", run)
        self.assertIn("--cargo-metadata", run)
        # Repository guards never share the host with the workspace suites:
        # the last pool member and the last exclusive suite end before nextest
        # starts. The production clippy, doctest and documentation chain starts
        # after the listing.
        self.assertLess(calls.index(["guard-end", "test-cpu-utilisation.py"]), calls.index(run), calls)
        self.assertLess(calls.index(["guard-end", "test-compiler-cache-namespaces.sh"]),
                        calls.index(run), calls)
        clippy = calls.index(production_clippy)
        doctests = calls.index(["test", "--no-fail-fast", "--workspace", "--doc"])
        self.assertLess(calls.index(listing), clippy, calls)
        self.assertLess(clippy, doctests, calls)
        self.assertLess(doctests, calls.index(["doc", "--no-deps", "--workspace"]), calls)
        for phase in ("doctests", "documentation"):
            self.assertIn((phase, "0"), phases)
        # A background check failure must keep its status and timing in the
        # final diagnostic tail, and the chain's later phases still report.
        original = (self.bin / "cargo").read_text()
        (self.bin / "cargo").write_text(original.replace(
            'if sys.argv[1] == "metadata":',
            'if sys.argv[1] == "check":\n    raise SystemExit(23)\n'
            'if sys.argv[1] == "metadata":'))
        self.log.write_text("")
        result = self.run_guard("ci-guardrails.sh")
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertRegex(result.stderr, r"ci-guardrails-phase: clippy-production wall_seconds=\d+ exit=23")
        phases = re.findall(r"ci-guardrails-phase: (\S+) wall_seconds=\d+ exit=(\d+)", result.stderr)
        for phase in ("doctests", "documentation"):
            self.assertIn((phase, "0"), phases)
        # A failed test build must keep its status and must not be followed by
        # tests.
        (self.bin / "cargo").write_text(original.replace(
            'if sys.argv[1] == "metadata":',
            'if sys.argv[1] == "test" and "--no-run" in sys.argv:\n    raise SystemExit(29)\n'
            'if sys.argv[1] == "metadata":'))
        self.log.write_text("")
        result = self.run_guard("ci-guardrails.sh")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stderr,
                         r"ci-guardrails-phase: workflow-test-list-build wall_seconds=\d+ exit=[1-9]")
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertFalse(any(call[:2] == ["nextest", "run"] for call in calls))

    def test_parallel_guards_rendezvous_and_all_failures_are_named(self):
        self.prepare_ci()
        pipes = [self.root / "left.fifo", self.root / "right.fifo"]
        for pipe in pipes:
            os.mkfifo(pipe)
        for index, name in enumerate(("check-orphan-modules.sh", "check-doc-links.py")):
            self.write_executable(self.scripts / name, f'''#!/usr/bin/env python3
import os, select
incoming = os.open({str(pipes[index])!r}, os.O_RDWR | os.O_NONBLOCK)
outgoing = os.open({str(pipes[1-index])!r}, os.O_RDWR | os.O_NONBLOCK)
os.write(outgoing, b"ready")
assert select.select([incoming], [], [], 10)[0], "guards did not overlap"
assert os.read(incoming, 5) == b"ready"
raise SystemExit({17+index})
''')
        # The two rendezvous guards are six launches apart, so both stay in flight only
        # when the pool holds at least six at once; eight keeps the batch the test always had.
        result = self.run_guard("ci-guardrails.sh", "--fast", env_overrides={"ORBIT_CARGO_JOBS": "8"})
        self.assertEqual(result.returncode, 17, result.stderr)
        for name in ("check-orphan-modules.sh", "check-doc-links.py"):
            self.assertIn(f"FAILED {name}", result.stderr)
            self.assertRegex(result.stderr, rf"ci-guardrails: {re.escape(name)} [0-9.]+ s")
        self.assertTrue((self.guard_calls / "test-cross-revision-check.sh").exists(),
                        "a failing guard must not leave remaining children unjoined")

    def write_overlapping_member(self, name, events, outgoing, incoming, hold=0):
        # A pool member announces itself on its outgoing pipe and blocks until its
        # partner does the same, so the pair can finish only while both are running.
        self.write_executable(self.scripts / name, f'''#!/usr/bin/env python3
import os, select, time
def log(event):
    with open({str(events)!r}, "a") as stream:
        stream.write(event + "\\n")
log("start {name}")
incoming_fd = os.open({str(incoming)!r}, os.O_RDWR | os.O_NONBLOCK)
outgoing_fd = os.open({str(outgoing)!r}, os.O_RDWR | os.O_NONBLOCK)
os.write(outgoing_fd, b"ready")
assert select.select([incoming_fd], [], [], 10)[0], "pool members did not overlap"
assert os.read(incoming_fd, 5) == b"ready"
time.sleep({hold})
log("end {name}")
''')

    def write_event_stub(self, name, events, hold=0):
        self.write_executable(self.scripts / name,
                              f'#!/bin/sh\nprintf "start {name}\\n" >> "{events}"\n'
                              f'sleep {hold}\n'
                              f'printf "end {name}\\n" >> "{events}"\n')

    def write_priority_stub(self, name, priorities, events=None, hold=0):
        # Records whether the guard runs lowered: in Linux's idle class, or else
        # at a higher nice value than this test process.
        self.write_executable(self.scripts / name, f'''#!/usr/bin/env python3
import json, os, time
def log(event):
    if {str(events) if events else ""!r}:
        with open({str(events)!r}, "a") as stream:
            stream.write(event + "\\n")
log("start {name}")
idle = getattr(os, "SCHED_IDLE", None)
lowered = ((idle is not None and os.sched_getscheduler(0) == idle)
           or os.getpriority(os.PRIO_PROCESS, 0) > {os.getpriority(os.PRIO_PROCESS, 0)})
with open({str(priorities)!r}, "a") as stream:
    stream.write(json.dumps([{name!r}, lowered]) + "\\n")
time.sleep({hold})
log("end {name}")
''')

    def test_pool_and_exclusive_self_test_failures_are_named_and_fail_the_job(self):
        self.prepare_ci()
        self.write_executable(self.scripts / "test-release-workflow.py", "#!/bin/sh\nexit 19\n")
        self.write_executable(self.scripts / "test-build-budget.sh", "#!/bin/sh\nexit 21\n")
        result = self.run_guard("ci-guardrails.sh", "--fast",
                                env_overrides={"CI_TEST_BASE": "missing-delivery-base"})
        # The pool joins before the exclusive suites, so its failure is the job's exit status.
        self.assertEqual(result.returncode, 19, result.stderr)
        self.assertIn("ci-guardrails: FAILED test-release-workflow.py (exit 19)", result.stderr)
        self.assertIn("ci-guardrails: FAILED test-build-budget.sh (exit 21)", result.stderr)
        self.assertRegex(result.stderr, r"ci-guardrails: test-build-budget\.sh [0-9.]+ s")

    def test_exclusive_self_tests_run_one_at_a_time_beside_a_lowered_pool(self):
        self.prepare_ci()
        events = self.root / "events"
        priorities = self.root / "priorities"
        # Pool members hold, so the exclusive lane overlaps them. Each records its
        # own priority, so a pool member at normal priority beside the lane shows.
        pool = ("test-installer-security.sh", "check-orphan-modules.sh", "test-cross-revision-check.sh")
        for name in pool:
            self.write_priority_stub(name, priorities, hold=0.5)
        # The first exclusive suite holds, so a second one started beside it
        # would show its start before the first one's end; the correct order never does.
        exclusive = ("test-build-budget.sh", "test-compiler-cache.sh")
        self.write_priority_stub(exclusive[0], priorities, events, hold=0.5)
        self.write_priority_stub(exclusive[1], priorities, events)
        result = self.run_guard("ci-guardrails.sh", "--fast",
                                env_overrides={"CI_TEST_BASE": "missing-delivery-base"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(events.read_text().splitlines(),
                         [f"{edge} {name}" for name in exclusive for edge in ("start", "end")])
        observed = dict(json.loads(line) for line in priorities.read_text().splitlines())
        # Exclusive suites keep the caller's priority; everything in the pool,
        # which shares the host with them, runs lowered.
        self.assertEqual(observed, {**{name: True for name in pool},
                                    **{name: False for name in exclusive}})
        self.assertRegex(result.stderr, r"ci-guardrails-phase: exclusive-self-tests wall_seconds=\d+ exit=0")

    def test_pool_width_is_the_build_budget_cargo_jobs(self):
        self.prepare_ci()
        events = self.root / "events"
        left, right = self.root / "left.fifo", self.root / "right.fifo"
        os.mkfifo(left)
        os.mkfifo(right)
        installer, mcp = "test-installer-security.sh", "test-mcp-registry-publish-workflow.sh"
        third = "test-check-unused-dependencies.py"
        # Two cargo jobs admit two members at once: the first pair rendezvous and hold,
        # so the third starts only after the oldest of them has ended.
        self.write_overlapping_member(installer, events, outgoing=left, incoming=right, hold=0.5)
        self.write_overlapping_member(mcp, events, outgoing=right, incoming=left, hold=0.5)
        self.write_event_stub(third, events)
        result = self.run_guard("ci-guardrails.sh", "--fast",
                                env_overrides={"CI_TEST_BASE": "missing-delivery-base",
                                               "ORBIT_CARGO_JOBS": "2"})
        self.assertEqual(result.returncode, 0, result.stderr)
        log = events.read_text().splitlines()
        self.assertLess(log.index(f"end {installer}"), log.index(f"start {third}"), log)

    def test_make_ci_fast_reports_failing_guard_name(self):
        self.prepare_ci()
        shutil.copy2(SCRIPTS.parent / "Makefile", self.root / "Makefile")
        self.write_executable(
            self.bin / "budget",
            '#!/bin/sh\nprintf "%s\\n" "$*" >> "$GUARD_TEST_CALLS/build-budget"\n'
            'shift\nexec "$@"\n',
        )
        self.write_executable(self.scripts / "check-doc-links.py", "#!/bin/sh\nexit 18\n")
        # Copy the runner before make invokes it; the other executables are stubs.
        shutil.copy2(SCRIPTS / "ci-guardrails.sh", self.scripts)
        result = subprocess.run(["make", "ci-fast", f"BUILD_BUDGET={self.bin / 'budget'}"],
                                cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("FAILED check-doc-links.py (exit 18)", result.stderr)
        self.assertRegex(result.stderr, r"ci-guardrails: check-doc-links.py [0-9]+(?:\.[0-9]+)? s")
        self.assertFalse(any(line.startswith("ci-fast: MSRV check ")
                             for line in result.stderr.splitlines()), result.stderr)
        cargo_calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(cargo_calls, [["fmt", "--all", "--", "--check"]])
        self.assertFalse((self.guard_calls / "build-budget").exists(),
                         "a failing fast guard must not invoke the MSRV build-budget command")

        # The same fixture with guard success must make the unchanged MSRV
        # command exactly once through the build-budget wrapper.
        self.write_executable(self.scripts / "check-doc-links.py", "#!/bin/sh\nexit 0\n")
        self.log.unlink(missing_ok=True)
        (self.guard_calls / "build-budget").unlink(missing_ok=True)
        result = subprocess.run(["make", "ci-fast", f"BUILD_BUDGET={self.bin / 'budget'}"],
                                cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        msrv = [line for line in result.stderr.splitlines()
                if line.startswith("ci-fast: MSRV check ")]
        self.assertEqual(len(msrv), 1, result.stderr)
        self.assertRegex(msrv[0], r"^ci-fast: MSRV check [0-9]+(?:\.[0-9]+)? s$")
        self.assertNotIn("ci-guardrails:", msrv[0])
        cargo_calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(cargo_calls, [
            ["fmt", "--all", "--", "--check"],
            ["+1.89", "check", "--workspace", "--locked"],
        ])
        self.assertEqual((self.guard_calls / "build-budget").read_text().splitlines(), [
            "-- cargo +1.89 check --workspace --locked",
        ])

    def test_static_runs_file_guards_and_rejects_public_ids_without_build_or_network(self):
        """Early handoff catches shipped IDs while retaining fast's file guards."""
        self.prepare_ci()
        fast = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(fast.returncode, 0, fast.stderr)
        expected = {path.name for path in self.guard_calls.iterdir()
                    if not path.name.startswith("test-") or path.name == "test-qa-full-sweep.py"}
        expected -= {"check-workflow-action-pins.sh", "check-desktop-ui.sh", "smoke-plugin-install.sh"}
        self.log.unlink()
        # A real production-string guard, not a stubbed failure. Check clean,
        # seeded bad and repaired input through the new public Make command.
        shutil.copy2(SCRIPTS / "check-public-artifact-ids.py", self.scripts)
        shutil.copy2(SCRIPTS.parent / "Makefile", self.root)
        self.write_executable(self.scripts / "check-stability.sh",
                              '#!/bin/sh\n[ "$CARGO_NET_OFFLINE" = true ] || exit 7\n'
                              ': > "$GUARD_TEST_CALLS/check-stability.sh"\n'
                              'cargo metadata --no-deps --format-version 1 --locked >/dev/null\n')
        source = self.root / "crates/example/src/lib.rs"
        source.parent.mkdir(parents=True)
        for text, succeeds in (("pub const ERROR: &str = \"task <id> failed\";", True),
                               ("pub const ERROR: &str = \"task ORB-12345 failed\";", False),
                               ("pub const ERROR: &str = \"task <id> failed\";", True)):
            with self.subTest(succeeds=succeeds):
                source.write_text(text)
                self.log.unlink(missing_ok=True)
                for call in self.guard_calls.iterdir():
                    call.unlink()
                result = subprocess.run(["make", "ci-guards"], cwd=self.root,
                                        env=self.env, text=True, capture_output=True)
                self.assertEqual(result.returncode == 0, succeeds, result.stdout + result.stderr)
                # The real guard has no call receipt; every other file guard
                # still ran. Excluded network/behavior/self-test stubs did not.
                actual = {path.name for path in self.guard_calls.iterdir()}
                self.assertEqual(actual, expected - {"check-public-artifact-ids.py"})
                calls = [json.loads(line) for line in self.log.read_text().splitlines()]
                self.assertEqual(calls, [["metadata", "--no-deps", "--format-version", "1", "--locked"]],
                                 "static mode must use only offline metadata, never build or format")
                if not succeeds:
                    self.assertIn("FAILED check-public-artifact-ids.py", result.stderr)
                    self.assertIn("concrete artifact ID", result.stderr)

    def test_static_qa_inventory_uses_offline_metadata_and_no_self_tests(self):
        """QA's isolated child must retain the static lane's no-network contract."""
        (self.scripts / "qa-full-sweep-inventory.json").write_text(json.dumps({"scenarios": []}))
        self.write_executable(self.bin / "cargo", f'''#!/usr/bin/env python3
import json, sys
from pathlib import Path
Path({str(self.log)!r}).write_text(json.dumps(sys.argv[1:]))
assert sys.argv[1] == "metadata" and "--offline" in sys.argv, sys.argv
print(json.dumps({{"packages": []}}))
''')
        driver = self.root / "driver.py"
        driver.write_text('''import importlib.util, sys
spec = importlib.util.spec_from_file_location("qa", sys.argv.pop(1))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
qa.validate_inventory = lambda *_: ([], {})
def forbidden():
    raise AssertionError("static inventory ran fixture self-tests")
qa.self_test = forbidden
qa.main()
''')
        command = [sys.executable, str(driver), str(SCRIPTS / "test-qa-full-sweep.py"),
                   "--repo-root", str(self.root), "--check", "--static"]
        result = subprocess.run(command, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(self.log.read_text()),
                         ["metadata", "--no-deps", "--format-version", "1", "--locked", "--offline"])
        self.log.unlink()
        for argument in ("--check-cargo-selections", "--run-commands", "--self-test"):
            with self.subTest(argument=argument):
                result = subprocess.run([*command, argument], env=self.env,
                                        text=True, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.log.exists(), "invalid static mode launched Cargo")

    def test_fast_does_not_enumerate_or_compile_tests(self):
        self.prepare_ci()
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [["fmt", "--all", "--", "--check"]])

    def test_fast_rejects_shell_sleep_polls(self):
        self.prepare_ci()
        shutil.copy2(SCRIPTS / "check-test-shell-waits.py", self.scripts)
        (self.scripts / "test-shell-waits-allowlist.json").write_text("[]")
        fixture = self.root / "crates/example/tests/fixture.rs"
        fixture.parent.mkdir(parents=True)
        cases = [
            'let script = "while [ ! -e x ]; do sleep 0.1; done";',
            'let script = r#"until [ -e x ]; do /bin/sleep 0.1; done"#;',
            'let script = r#"while [ ! -e x ]; do\n/bin/sleep 0.1\ndone"#;',
            's.push_str("while [ ! -e x ]; do\\n");\n'
            's.push_str("sleep 0.1\\ndone\\n");',
            'let script = "while true; do for n in 1 2; do sleep 0.1; done; done";',
            'let script = "sh -c \'while true; do /bin/sleep 0.1; done\'";',
            'let script = r#"while true; do "/bin/sleep" 0.1; done"#;',
            'let script = "while true; do sh -c \'sleep 0.1\'; done";',
        ]
        # Keep one full runner failure to verify wiring and propagation. The
        # remaining syntax rows exercise the same checker directly rather
        # than launching every unrelated guard stub again.
        for index, source in enumerate(cases):
            with self.subTest(source=source):
                fixture.write_text(source)
                result = (self.run_guard("ci-guardrails.sh", "--fast") if index == 0
                          else self.run_guard("check-test-shell-waits.py"))
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("fixture.rs:", result.stderr)
                self.assertIn("shell sleep poll", result.stderr)
        # A sibling unit-test path receives the same protection.
        fixture.write_text("")
        fixture = self.root / "crates/example/src/process/tests/wait.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(cases[0])
        result = self.run_guard("check-test-shell-waits.py")
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_fast_allows_nonpolling_sleep_and_narrow_reasoned_exception(self):
        self.prepare_ci()
        shutil.copy2(SCRIPTS / "check-test-shell-waits.py", self.scripts)
        allowlist = self.scripts / "test-shell-waits-allowlist.json"
        allowlist.write_text("[]")
        fixture = self.root / "crates/example/tests/fixture.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text('let script = "sleep 0.1; while true; do read -r x; done";\n'
                           '// "while true; do sleep 1; done"\n'
                           'let prose = "we sleep while waiting";')
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        # The clean row above checks full runner wiring; exception policy is
        # checked at the executable boundary without repeating that runner.
        fixture.write_text('let script = "while [ ! -e x ]; do sleep 0.1; done";')
        exception = dict(path="crates/example/tests/fixture.rs",
                         loop="while [ ! -e x ]; do", reason="bounded sandbox fixture")
        allowlist.write_text(json.dumps([exception]))
        result = self.run_guard("check-test-shell-waits.py")
        self.assertEqual(result.returncode, 0, result.stderr)
        # An exception grants one exact loop, not the entire file or duplicates.
        fixture.write_text(fixture.read_text() + '\nlet other = "until true; do sleep 1; done";')
        result = self.run_guard("check-test-shell-waits.py")
        self.assertEqual(result.returncode, 1, result.stderr)
        fixture.write_text("")
        result = self.run_guard("check-test-shell-waits.py")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("stale shell-wait exception", result.stderr)
        exception["reason"] = ""
        allowlist.write_text(json.dumps([exception]))
        result = self.run_guard("check-test-shell-waits.py")
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_fast_propagates_desktop_ui_failure(self):
        self.prepare_ci()
        self.write_executable(self.scripts / "check-desktop-ui.sh", "#!/bin/bash\nexit 17\n")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 17)

    def test_fast_propagates_doc_link_failure(self):
        self.prepare_ci()
        self.write_executable(self.scripts / "check-doc-links.py", "#!/bin/bash\nexit 18\n")
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 18)

    def test_fast_invokes_web_blocking_handler_check(self):
        self.prepare_ci()
        self.write_executable(
            self.scripts / "check-web-blocking-handlers.py",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["check-web-blocking-handlers.py"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["check-web-blocking-handlers.py"], calls)

    def test_fast_invokes_codeql_extension_schema_check(self):
        self.prepare_ci()
        self.write_executable(
            self.scripts / "check-codeql-extension-schema.py",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["check-codeql-extension-schema.py"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["check-codeql-extension-schema.py"], calls)

    def test_fast_invokes_dashboard_vendor_check(self):
        self.prepare_ci()
        self.write_executable(
            self.scripts / "check-dashboard-vendor.py",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["check-dashboard-vendor.py"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh", "--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["check-dashboard-vendor.py"], calls)

    def test_full_still_checks_workflow_test_matches(self):
        # The full run lists the workflow's filtered tests from one workspace
        # build (the artifacts the nextest pass reuses), never from a
        # per-package `cargo test -p` build. [DANI-10428]
        self.prepare_ci()
        env_log = self.root / "cargo-env.log"
        self.env["GUARD_TEST_ENV_LOG"] = str(env_log)
        result = self.run_guard("ci-guardrails.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        folded = ["test", "--workspace", "--lib", "--bins", "--tests", "--examples", "--locked",
                  "--no-run", "--message-format", "json"]
        self.assertIn(folded, calls)
        observed = [json.loads(line) for line in env_log.read_text().splitlines()]
        lint = {
            "RUSTC_WORKSPACE_WRAPPER": "clippy-driver",
            "CLIPPY_ARGS": (
                "-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__"
                "-A__CLIPPY_HACKERY__clippy::disallowed_methods__CLIPPY_HACKERY__"
            ),
        }
        builds = [row for row in observed if row["argv"] == folded]
        self.assertEqual([{key: row[key] for key in ("argv", *lint)} for row in builds],
                         [dict(argv=folded, **lint)])
        # The workspace test listing runs in the main shell; nextest keeps its priority.
        listed = [row for row in observed if row["argv"][:2] == ["nextest", "list"]]
        foreground = {key: listed[0][key] for key in ("idle", "nice")}
        # Cargo fingerprints this environment. The nextest listing and the
        # doctest build must see exactly the folded build's values, or each
        # recompiles every workspace member before it can run.
        listing = [row for row in observed if row["argv"][:2] == ["nextest", "list"]]
        doctests = [row for row in observed if "--doc" in row["argv"]]
        self.assertEqual(len(listing), 1, observed)
        self.assertEqual(len(doctests), 1, observed)
        for row in (*listing, *doctests):
            self.assertEqual({key: row[key] for key in lint}, lint, row)
        # Documentation runs under production Clippy's lint environment, so it
        # reuses that pass's check of the workspace instead of repeating it.
        production_lint = {
            "RUSTC_WORKSPACE_WRAPPER": "clippy-driver",
            "CLIPPY_ARGS": (
                "-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__"
                "-D__CLIPPY_HACKERY__clippy::disallowed_methods__CLIPPY_HACKERY__"
            ),
        }
        production = [row for row in observed if row["argv"][:1] in (["check"], ["doc"])]
        self.assertEqual([row["argv"][0] for row in production], ["check", "doc"], observed)
        for row in production:
            self.assertEqual({key: row[key] for key in production_lint}, production_lint, row)
        # The production clippy, doctest and documentation chain overlaps the
        # tests only at lowered priority, so it cannot take CPU time from their
        # deadline assertions; the tests themselves keep normal priority. The
        # workflow test build is lowered too: it overlaps the exclusive suites.
        background = [*production, *doctests, *builds]
        self.assertEqual(len(background), 4, observed)
        for row in background:
            self.assertTrue((row["idle"] and not foreground["idle"]) or
                            row["nice"] > foreground["nice"], (row, foreground))
        runs = [row for row in observed if row["argv"][:2] == ["nextest", "run"]]
        self.assertEqual(len(runs), 1, observed)
        self.assertEqual({key: runs[0][key] for key in foreground}, foreground)
        self.assertTrue(all(row["RUSTC_WORKSPACE_WRAPPER"] is None
                            for row in observed if row["argv"][:1] == ["metadata"]))
        self.assertIn(["fixture-test-bin", "--list", "fixture_test"], calls)
        self.assertNotIn("-p", [argument for call in calls for argument in call])

    def test_check_commands_lower_themselves_when_parent_renice_is_denied(self):
        # Reproduce the macOS worker restriction without depending on the
        # host sandbox: no idle scheduler and no priority changes to parents.
        self.prepare_ci()
        for command in ("chrt", "renice"):
            self.write_executable(self.bin / command, "#!/bin/sh\nexit 1\n")
        env_log = self.root / "cargo-env.log"
        self.env["GUARD_TEST_ENV_LOG"] = str(env_log)
        result = self.run_guard("ci-guardrails.sh", "--checks")
        self.assertEqual(result.returncode, 0, result.stderr)
        observed = [json.loads(line) for line in env_log.read_text().splitlines()]
        foreground = next(row for row in observed if row["argv"][0] == "fmt")
        self.assertEqual(foreground["nice"], os.getpriority(os.PRIO_PROCESS, 0))
        background = [row for row in observed
                      if row["argv"][0] in ("check", "test", "doc")]
        self.assertEqual(len(background), 3, observed)
        for row in background:
            self.assertGreater(row["nice"], foreground["nice"], (row, foreground))

    def test_split_ci_modes_together_run_the_full_ci_work(self):
        """The hosted test-build and Check jobs split the full run between them."""
        self.prepare_ci()
        self.init_ci_git()
        env_log = self.root / "cargo-env.log"
        self.env["GUARD_TEST_ENV_LOG"] = str(env_log)
        folded = ["test", "--workspace", "--lib", "--bins", "--tests", "--examples", "--locked",
                  "--no-run", "--message-format", "json"]
        archive = str(self.root / "nextest-archive.tar.zst")
        archive_call = ["nextest", "archive", "--locked", "--workspace", "--lib", "--bins", "--tests",
                        "--archive-file", archive]

        # --test-archive: the folded build, the archive the shards run, then the
        # partition guard over that archive. No guard, check or test runs.
        result = self.run_guard("ci-guardrails.sh", "--test-archive", archive,
                                env_overrides={"NEXTEST_SHARDS": "3"})
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(folded, calls)
        self.assertLess(calls.index(folded), calls.index(archive_call), calls)
        self.assertFalse(any(call[:2] == ["nextest", "run"] or call[:1] in (["fmt"], ["check"], ["doc"])
                             or "--doc" in call for call in calls), calls)
        self.assertEqual((self.guard_calls / "check-nextest-partitions.py").read_text().splitlines(),
                         ["--shards", "3", "--", "cargo", "nextest", "list", "--archive-file", archive,
                          "--workspace-remap", str(self.root)])
        self.assertEqual({path.name for path in self.guard_calls.iterdir()} - {"require-python.sh"},
                         {"check-nextest-partitions.py"}, "no repository guard or self-test runs")
        phases = re.findall(r"ci-guardrails-phase: (\S+) wall_seconds=\d+ exit=(\d+)", result.stderr)
        self.assertEqual(phases, [("workflow-test-list-build", "0"), ("workspace-test-archive", "0"),
                                  ("workspace-test-partitions", "0")])
        # The archive and listings must reuse the folded build, not rebuild it.
        observed = [json.loads(line) for line in env_log.read_text().splitlines()]
        archived = [row for row in observed if row["argv"] == archive_call]
        self.assertEqual([row["RUSTC_WORKSPACE_WRAPPER"] for row in archived], ["clippy-driver"])
        # A partition gap fails the job with the guard's status.
        self.write_executable(self.scripts / "check-nextest-partitions.py", "#!/bin/sh\nexit 7\n")
        self.log.write_text("")
        result = self.run_guard("ci-guardrails.sh", "--test-archive", archive,
                                env_overrides={"NEXTEST_SHARDS": "3"})
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertIn("ci-guardrails-phase: workspace-test-partitions wall_seconds=", result.stderr)

        # --checks: everything else, with no test build and no nextest.
        for call in self.guard_calls.iterdir():
            call.unlink()
        self.log.write_text("")
        env_log.write_text("")
        result = self.run_guard("ci-guardrails.sh", "--checks")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_self_tests(True)
        self.assertTrue((self.guard_calls / "test-ci-download-verified.py").exists())
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertNotIn(folded, calls)
        self.assertFalse(any(call[:1] == ["nextest"] for call in calls), calls)
        for expected in (["fmt", "--all", "--", "--check"], ["check", "--workspace", "--lib", "--bins"],
                         ["test", "--no-fail-fast", "--workspace", "--doc"], ["doc", "--no-deps", "--workspace"]):
            self.assertIn(expected, calls)
        phases = dict(re.findall(r"ci-guardrails-phase: (\S+) wall_seconds=\d+ exit=(\d+)", result.stderr))
        for phase in ("fmt", "clippy-production", "doctests", "documentation", "repository-guards"):
            self.assertEqual(phases.get(phase), "0", result.stderr)
        # The check chain shares the host with the exclusive suites, so it runs
        # lowered like the build it replaces there.
        observed = [json.loads(line) for line in env_log.read_text().splitlines()]
        formatting = next(row for row in observed if row["argv"][:1] == ["fmt"])
        for row in observed:
            if row["argv"][:1] in (["check"], ["doc"]) or "--doc" in row["argv"]:
                self.assertTrue((row["idle"] and not formatting["idle"]) or row["nice"] > formatting["nice"],
                                (row, formatting))

    def test_macos_check_defaults_to_per_package_listing(self):
        # Without --workspace-build (the macOS job and local runs, whose `-p`
        # artifacts are already warm) the per-package listing is unchanged.
        self.prepare_ci()
        result = self.run_guard("check-ci-macos.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [["test", "-p", "orbit-types", "--locked", "fixture_test", "--", "--list"]])

    def test_macos_check_requires_unrestricted_pull_request_coverage(self):
        # Required native checks must report even for unrelated docs changes.
        # Exercise the public guard with real workflow-event counterfactuals.
        for event, accepted in (
            ("  pull_request:\n", True),
            ("  pull_request: {}\n", True),
            ("  push:\n", False),
            ("  pull_request:\n    branches: [main]\n", False),
            ("  pull_request:\n    branches-ignore: [agent-main]\n", False),
            ("  pull_request:\n    paths:\n      - Cargo.toml\n", False),
            ("  pull_request:\n    paths-ignore: ['docs/**']\n", False),
            ("  pull_request: {paths: ['crates/**']}\n", False),
        ):
            with self.subTest(event=event):
                self.prepare_ci()
                workflow = self.root / ".github/workflows/ci-macos.yml"
                workflow.write_text(workflow.read_text().replace("  pull_request:\n", event))
                result = self.run_guard("check-ci-macos.sh")
                self.assertEqual(result.returncode, 0 if accepted else 1, result.stdout + result.stderr)
                if not accepted:
                    self.assertIn("unfiltered pull_request event", result.stderr)

    def test_macos_check_forwards_target_selectors_and_exact_to_the_listing(self):
        self.prepare_ci()
        self.set_targets(("process", "test", ["update::runs"]), ("other", "test", ["update::runs"]))
        self.set_macos_step("cargo test --no-fail-fast -p orbit-types --locked update:: --test process"
                            " -- --exact --nocapture")
        result = self.run_guard("check-ci-macos.sh")
        self.assertEqual(result.returncode, 1, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [["test", "-p", "orbit-types", "--locked", "--test", "process",
                                  "update::", "--", "--list", "--exact"]])

    def test_macos_check_rejects_filter_matching_only_in_another_test_binary(self):
        # `--test process` matches nothing when the module moved to another
        # binary, even though the package still has a matching test elsewhere.
        for workspace_build in ([], ["--workspace-build"]):
            with self.subTest(workspace_build=bool(workspace_build)):
                self.prepare_ci()
                self.set_targets(("process", "test", ["other::runs"]), ("tool", "test", ["update::runs"]))
                self.set_macos_step("cargo test -p orbit-types --locked update:: --test process")
                result = self.run_guard("check-ci-macos.sh", *workspace_build)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("matched zero tests: orbit-types update:: --test process", result.stderr)

    def test_macos_check_rejects_filter_matching_only_as_substring_under_exact(self):
        for workspace_build in ([], ["--workspace-build"]):
            with self.subTest(workspace_build=bool(workspace_build)):
                self.prepare_ci()
                self.set_targets(("tool", "test", ["plugin_cli_group::lockfile_upgrade_renamed"]))
                self.set_macos_step("cargo test -p orbit-types --locked plugin_cli_group::lockfile_upgrade"
                                    " --test tool -- --exact")
                result = self.run_guard("check-ci-macos.sh", *workspace_build)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("matched zero tests", result.stderr)

    def test_macos_check_accepts_filters_that_the_selected_target_runs(self):
        for workspace_build in ([], ["--workspace-build"]):
            with self.subTest(workspace_build=bool(workspace_build)):
                self.prepare_ci()
                self.set_targets(("process", "test", ["update::runs"]),
                                 ("tool", "test", ["plugin_cli_group::lockfile_upgrade"]))
                self.set_macos_step("cargo test -p orbit-types --locked update:: --test process\n"
                                    "      - run: cargo test -p orbit-types --locked"
                                    " plugin_cli_group::lockfile_upgrade --test tool -- --exact")
                result = self.run_guard("check-ci-macos.sh", *workspace_build)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_macos_check_rejects_missing_selected_target_in_workspace_build(self):
        self.prepare_ci()
        self.set_targets(("tool", "test", ["update::runs"]))
        self.set_macos_step("cargo test -p orbit-types --locked update:: --test process")
        result = self.run_guard("check-ci-macos.sh", "--workspace-build")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("no test binary matching --test process", result.stderr)

    def test_workspace_build_failure_names_the_rendered_lint_diagnostic(self):
        # `--message-format json` moves rustc's diagnostics to stdout as
        # `compiler-message` objects, and stderr keeps only cargo's summary. A
        # failed all-targets lint must still name its lint, file and line.
        self.prepare_ci()
        rendered = ("error: this expression creates a reference which is immediately dereferenced\n"
                    "  --> crates/orbit-types/tests/fixture.rs:3:13\n"
                    "  = note: `#[deny(clippy::needless_borrow)]` on by default")
        self.env["GUARD_TEST_RENDERED"] = rendered
        self.write_executable(self.bin / "cargo", '''#!/usr/bin/env python3
import json, os, sys
if sys.argv[1] == "metadata":
    print(open(os.environ["GUARD_TEST_METADATA"]).read())
    raise SystemExit(0)
print(json.dumps({"reason": "compiler-message", "message": {"rendered": os.environ["GUARD_TEST_RENDERED"]}}))
print("error: could not compile `orbit-types` (lib test) due to 1 previous error", file=sys.stderr)
raise SystemExit(101)
''')
        result = self.run_guard("check-ci-macos.sh", "--workspace-build")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(rendered, result.stderr)

    def test_macos_check_rejects_unknown_flags(self):
        self.prepare_ci()
        result = self.run_guard("check-ci-macos.sh", "--fast")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)

    def test_full_invokes_cargo_deny_guard(self):
        self.prepare_ci()
        # The gate is soft-presence: stub the tool so the test does not depend
        # on the host having cargo-deny installed.
        self.write_executable(self.bin / "cargo-deny", "#!/bin/bash\nexit 0\n")
        self.write_executable(
            self.scripts / "cargo-deny.sh",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps(["cargo-deny.sh"] + sys.argv[1:]) + "\\n")
''',
        )
        result = self.run_guard("ci-guardrails.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(["cargo-deny.sh", "check"], calls)

    def dependency_result(self, owner, dependency, kind=None):
        (self.root / "Cargo.toml").write_text(
            '[workspace]\n[workspace.dependencies]\ntempfile = "3"\n'
        )
        manifests = {}
        for name in {owner, dependency}:
            path = self.root / f"{name}.toml"
            path.touch()
            manifests[name] = str(path)
        packages = [dict(id=name, name=name, manifest_path=manifest,
                         dependencies=([dict(name=dependency, kind=kind)] if name == owner else []))
                    for name, manifest in manifests.items()]
        self.metadata.write_text(json.dumps(dict(workspace_members=list(manifests), packages=packages)))
        return self.run_guard("check-dependency-direction.sh")

    def test_dependency_direction_fails_when_ripgrep_is_missing(self):
        without_rg = self.root / "without-rg"
        without_rg.mkdir()
        shutil.copy(shutil.which("dirname"), without_rg / "dirname")

        result = self.run_guard("check-dependency-direction.sh",
                                env_overrides={"PATH": str(without_rg)})

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ripgrep (rg) is required; install it before running", result.stderr)
        self.assertNotIn("dependency direction guard passed", result.stdout)

    def test_allowed_dependency_on_native_bash(self):
        result = self.dependency_result("orbit-common", "orbit-types")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("dependency direction guard passed", result.stdout)

    def test_forbidden_dependency_names_owning_manifest(self):
        result = self.dependency_result("orbit-types", "orbit-common")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"forbidden dependency 'orbit-common' found in {self.root}/orbit-types.toml", result.stdout)

    def test_dev_only_dependency_rejects_production_edge(self):
        result = self.dependency_result("orbit-cli", "orbit-engine")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must remain dev-only", result.stdout)

    def test_dev_only_dependency_accepts_test_edge(self):
        result = self.dependency_result("orbit-cli", "orbit-engine", "dev")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_workspace_dependency_must_be_inherited(self):
        self.dependency_result("orbit-common", "orbit-types")
        manifest = self.root / "orbit-common.toml"
        manifest.write_text('[dev-dependencies]\ntempfile = "3"\n')
        result = self.run_guard("check-dependency-direction.sh")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dev-dependencies.tempfile must inherit", result.stdout)

        manifest.write_text('[dev-dependencies]\ntempfile.workspace = true\n')
        result = self.run_guard("check-dependency-direction.sh")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_goldens_runs_mcp_conformance_and_cli_snapshot_tests(self):
        result = self.run_guard("check-goldens.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(
            calls,
            [
                [
                    "test",
                    "-p",
                    "orbit-core",
                    "--test",
                    "ci_failure_goldens",
                    "ci_failure_fixture_goldens",
                    "--",
                    "--exact",
                ],
                [
                    "test",
                    "-p",
                    "orbit-tools",
                    "--test",
                    "tools",
                    "--",
                    "public_tool_surface::github_log_goldens::",
                    "mcp_definitions::",
                ],
                ["test", "-p", "orbit-cli", "--test", "output", "--", "help_goldens::", "output_goldens::"],
                [
                    "test",
                    "-p",
                    "orbit-cli",
                    "--test",
                    "mcp",
                    "mcp_roundtrip::mcp_serve_tools_list_matches_production_snapshot",
                    "--",
                    "--exact",
                ],
                ["test", "-p", "orbit-exec", "--test", "sandbox_profile_goldens"],
                ["test", "-p", "orbit-core", "--test", "sandbox_profile_goldens"],
                ["test", "-p", "orbit-store", "--lib",
                 "driver::sqlite::migration::tests::ledger::store_schema_prefix_goldens",
                 "--", "--exact",
                 "driver::sqlite::migration::tests::ledger::task_registry_schema_prefix_goldens"],
            ],
        )

    def test_goldens_update_sets_regeneration_env_vars(self):
        self.write_executable(
            self.bin / "cargo",
            '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["GUARD_TEST_LOG"], "a") as log:
    log.write(json.dumps({
        "argv": sys.argv[1:],
        "help": os.environ.get("ORBIT_UPDATE_HELP_GOLDENS"),
        "output": os.environ.get("ORBIT_UPDATE_OUTPUT_GOLDENS"),
        "mcp": os.environ.get("ORBIT_MCP_UPDATE_SNAPSHOT"),
        "sandbox": os.environ.get("ORBIT_UPDATE_SANDBOX_GOLDENS"),
        "logs": os.environ.get("ORBIT_UPDATE_LOG_GOLDENS"),
        "store_schema": os.environ.get("ORBIT_UPDATE_STORE_SCHEMA_GOLDENS"),
    }) + "\\n")
''',
        )
        result = self.run_guard("check-goldens.sh", "--update")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 7)
        for call in calls:
            self.assertEqual(call["help"], "1")
            self.assertEqual(call["output"], "1")
            self.assertEqual(call["mcp"], "1")
            self.assertEqual(call["sandbox"], "1")
            self.assertEqual(call["logs"], "1")
            self.assertEqual(call["store_schema"], "1")

    def test_goldens_rejects_unknown_flags(self):
        result = self.run_guard("check-goldens.sh", "--fast")
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: check-goldens.sh [--update]", result.stderr)


class DocLinkGuardrailTests(unittest.TestCase):
    """Exercise the checker through its executable boundary in real Git repos."""

    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="doc-link-fixture-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = dict(os.environ)
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            self.env.pop(name, None)
        subprocess.run(["git", "init", "-q", str(self.root)], env=self.env, check=True)
        self.write("scripts/check-doc-links.py", (SCRIPTS / "check-doc-links.py").read_text())

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run_checker(self):
        subprocess.run(["git", "add", "."], cwd=self.root, env=self.env, check=True)
        return subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                              env=self.env, text=True, capture_output=True)

    def test_valid_links_slugs_and_examples(self):
        self.write("crates/demo/src/lib.rs", "// fixture source\n")
        self.write("docs/target.md", "# `snake_case` & *Details*\n# Repeat\n# Repeat\n"
                   "# Repeat-2\n# Repeat\nSetext title\n============\n"
                   "<a name=\"custom\"></a>\n# Ελληνικά\n")
        self.write("docs/asset (one).svg", "<svg/>\n")
        self.write("docs/guide.md", "# Guide\n[local](#guide)\n"
                   "[`code label`](target.md#snake_case--details)\n"
                   "[duplicate](target.md#repeat-3)\n[setext](target.md#setext-title)\n"
                   "[Unicode](target.md#%CE%B5%CE%BB%CE%BB%CE%B7%CE%BD%CE%B9%CE%BA%CE%AC)\n"
                   "[custom](target.md#custom)\n![image](<asset (one).svg> \"title\")\n"
                   "[reference][target]\n[target]: target.md#repeat-1\n"
                   "`crates/demo/src/lib.rs` and `crates/demo/src/lib.rs::item`\n"
                   "`[example](missing.md)`\n~~~md\n[example](missing.md)\n"
                   "`crates/missing.rs`\n~~~\n\n    [example](missing.md)\n"
                   "<!-- [comment](missing.md) -->\n"
                   "\\[escaped](missing.md)\n"
                   "[external](https://example.com/missing.md#missing)\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("checked", result.stdout)

    def test_broken_link_anchor_and_source_path_name_each_line(self):
        self.write("docs/target.md", "# Existing heading\n")
        self.write("docs/guide.md", "[good](target.md#existing-heading)\n"
                   "[broken](missing.md)\n[anchor](target.md#missing)\n"
                   "`crates/demo/src/missing.rs`\n\n- List item\n"
                   "    [nested](missing-nested.md)\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 1, result.stderr)
        for line, reason, target in (
                (2, "missing link target", "missing.md"),
                (3, "missing heading anchor", "target.md#missing"),
                (4, "missing repository source path", "crates/demo/src/missing.rs"),
                (7, "missing link target", "missing-nested.md")):
            self.assertIn(f"docs/guide.md:{line}: {reason}: {target}", result.stderr)

    def test_untracked_source_is_accepted(self):
        self.write("docs/guide.md", "`crates/demo/src/untracked.rs`\n")
        subprocess.run(["git", "add", "."], cwd=self.root, env=self.env, check=True)
        self.write("crates/demo/src/untracked.rs", "// untracked\n")
        result = subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("checked 1 Markdown files", result.stdout)

    def test_untracked_markdown_is_checked(self):
        self.run_checker()
        self.write("docs/untracked.md", "[broken](missing.md)\n")
        result = subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("docs/untracked.md:1: missing link target: missing.md", result.stderr)

    def test_ignored_source_is_rejected(self):
        self.write(".gitignore", "crates/demo/src/ignored.rs\n")
        self.write("docs/guide.md", "`crates/demo/src/ignored.rs`\n")
        self.run_checker()
        self.write("crates/demo/src/ignored.rs", "// ignored\n")
        result = subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("missing repository source path: crates/demo/src/ignored.rs", result.stderr)

    def test_deleted_tracked_source_is_rejected(self):
        self.write("docs/guide.md", "`crates/demo/src/deleted.rs`\n")
        self.write("crates/demo/src/deleted.rs", "// tracked then deleted\n")
        self.run_checker()
        (self.root / "crates/demo/src/deleted.rs").unlink()
        result = subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("missing repository source path: crates/demo/src/deleted.rs", result.stderr)

    def test_excluded_documents_are_ignored_when_tracked_or_untracked(self):
        excluded = ("docs/design/demo/4_decisions.md", "docs/design/_templates/example.md",
                    "docs/design/CONVENTIONS.md", "docs/rca/record.md", "CHANGELOG.md",
                    "crates/demo/tests/fixtures/example.md")
        for name in excluded:
            self.write(name, "[historical](missing.md)\n`crates/missing.rs`\n")
        tracked = self.run_checker()
        self.assertEqual(tracked.returncode, 0, tracked.stderr)
        self.assertIn("checked 0 Markdown files", tracked.stdout)
        subprocess.run(["git", "rm", "--cached", "--", *excluded], cwd=self.root,
                       env=self.env, check=True, capture_output=True, text=True)
        result = subprocess.run(["python3", str(self.root / "scripts/check-doc-links.py")],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("checked 0 Markdown files", result.stdout)

    def test_website_routes_resolve_to_source_and_validate_anchors(self):
        self.write("website/src/content/docs/how-to/guide.md",
                   "[page](../../reference/config/#snake_case)\n[index](../)\n")
        self.write("website/src/content/docs/how-to/index.md", "# How to\n")
        self.write("website/src/content/docs/reference/config.md", "# snake_case\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.write("website/src/content/docs/reference/config.md", "# Renamed\n")
        result = self.run_checker()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("guide.md:1: missing heading anchor", result.stderr)


class WorkflowActionPinGuardrailTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "curl_calls.log"
        self.status_map = self.root / "curl_status_map.json"
        self.status_map.write_text("{}")
        self.env = dict(
            os.environ,
            PATH=f"{self.bin}:{os.environ['PATH']}",
            FAKE_CURL_LOG=str(self.log),
            FAKE_CURL_STATUS_MAP_FILE=str(self.status_map),
        )
        self.env.pop("ORBIT_STRICT_WORKFLOW_ACTION_PINS", None)
        # Curl is already simulated. Record the real guard's requested waits
        # in the same log so ordering/backoff remain observable without sleep.
        self.write_executable(self.bin / "sleep",
                              '#!/bin/sh\nprintf "sleep %s\\n" "$*" >> "$FAKE_CURL_LOG"\n')
        self.write_executable(self.bin / "curl", '''#!/usr/bin/env python3
import json, os, sys
url = sys.argv[-1]
log = os.environ.get("FAKE_CURL_LOG")
if log:
    with open(log, "a") as f:
        f.write(url + "\\n")
    with open(log) as f:
        attempt = sum(line.rstrip("\\n") == url for line in f)
else:
    attempt = 1
mapping = json.loads(open(os.environ["FAKE_CURL_STATUS_MAP_FILE"]).read())
status = mapping.get(url)
if isinstance(status, list):
    status = status[min(attempt - 1, len(status) - 1)]
if status is None:
    status = "000"
sys.stdout.write(status)
if status == "000":
    sys.exit(1)
''')

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def set_statuses(self, mapping):
        self.status_map.write_text(json.dumps(mapping))

    def write_workflow(self, name, uses_lines):
        workflows = self.root / ".github/workflows"
        workflows.mkdir(parents=True, exist_ok=True)
        body = "jobs:\n  job:\n    steps:\n" + "".join(
            f"      - uses: {line}\n" for line in uses_lines
        )
        (workflows / name).write_text(body)

    def run_guard(self, *arguments, env_overrides=None):
        shutil.copy2(SCRIPTS / "check-workflow-action-pins.sh", self.scripts / "check-workflow-action-pins.sh")
        env = dict(self.env)
        if env_overrides:
            env.update(env_overrides)
        return subprocess.run(
            ["/bin/bash", str(self.scripts / "check-workflow-action-pins.sh"), *arguments],
            env=env, text=True, capture_output=True,
        )

    def pin_url(self, owner, sha):
        return f"https://api.github.com/repos/{owner}/commits/{sha}"

    def pin_call_count(self, url):
        return sum(line == url for line in self.log.read_text().splitlines())

    def test_passes_when_all_pins_resolve(self):
        good_sha = "a" * 40
        self.write_workflow("check.yml", [f"actions/checkout@{good_sha} # v1"])
        self.set_statuses({
            self.pin_url("actions/checkout", good_sha): "200",
        })
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.log.read_text().splitlines(), [self.pin_url("actions/checkout", good_sha)])

    def test_fails_when_a_pin_does_not_resolve(self):
        bad_sha = "b" * 40
        self.write_workflow("check.yml", [f"actions/setup-node@{bad_sha} # v7.0.0"])
        url = self.pin_url("actions/setup-node", bad_sha)
        for status in ("404", "422"):
            with self.subTest(status=status):
                self.log.write_text("")
                self.set_statuses({url: status})
                result = self.run_guard()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"actions/setup-node@{bad_sha} does not resolve (status {status})", result.stderr)
                self.assertIn(".github/workflows/check.yml:", result.stderr)
                self.assertEqual(self.pin_call_count(url), 1)
                self.assertEqual(self.log.read_text().splitlines(), [url])

    def test_retries_curl_000_then_skips_with_warning(self):
        bad_sha = "c" * 40
        self.write_workflow("check.yml", [f"actions/setup-node@{bad_sha} # v7.0.0"])
        url = self.pin_url("actions/setup-node", bad_sha)
        self.set_statuses({url: ["000", "000", "000"]})
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("inconclusive resolving", result.stderr)
        self.assertIn("status 000 after 3 attempts", result.stderr)
        self.assertIn("skipping", result.stderr)
        self.assertEqual(self.pin_call_count(url), 3)
        self.assertEqual(self.log.read_text().splitlines(), [url, "sleep 0.25", url, "sleep 0.5", url])

    def test_retries_rate_limits_and_server_errors_then_skips(self):
        sha = "e" * 40
        self.write_workflow("check.yml", [f"actions/checkout@{sha} # v1"])
        url = self.pin_url("actions/checkout", sha)
        for status in ("403", "429", "500", "503"):
            with self.subTest(status=status):
                self.log.write_text("")
                self.set_statuses({url: [status, status, status]})
                result = self.run_guard()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"status {status} after 3 attempts", result.stderr)
                self.assertIn("skipping", result.stderr)
                self.assertEqual(self.pin_call_count(url), 3)
                self.assertEqual(self.log.read_text().splitlines(),
                                 [url, "sleep 0.25", url, "sleep 0.5", url])

    def test_strict_mode_fails_when_result_remains_inconclusive(self):
        sha = "f" * 40
        self.write_workflow("check.yml", [f"actions/checkout@{sha} # v1"])
        url = self.pin_url("actions/checkout", sha)
        self.set_statuses({url: ["403", "403", "403"]})
        result = self.run_guard(env_overrides={"ORBIT_STRICT_WORKFLOW_ACTION_PINS": "1"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("strict mode requires resolution", result.stderr)
        self.assertEqual(self.pin_call_count(url), 3)
        self.assertEqual(self.log.read_text().splitlines(), [url, "sleep 0.25", url, "sleep 0.5", url])

    def test_dedupes_repeated_pins(self):
        sha = "d" * 40
        self.write_workflow(
            "a.yml",
            [f"actions/checkout@{sha} # v1", f"actions/checkout@{sha} # v1"],
        )
        self.set_statuses({
            self.pin_url("actions/checkout", sha): "200",
        })
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.log.read_text().splitlines()
        commit_calls = [c for c in calls if c.endswith(f"/commits/{sha}")]
        self.assertEqual(len(commit_calls), 1)


class WorkflowYamlGuardrailTests(unittest.TestCase):
    def setUp(self):
        try:
            import yaml  # noqa: F401
        except ImportError:
            self.skipTest("PyYAML not installed; the guard skips locally without it")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        self.workflows = self.root / ".github/workflows"
        self.workflows.mkdir(parents=True)
        shutil.copy2(SCRIPTS / "check-workflow-yaml.py", self.root / "scripts")

    def write_workflow(self, name, run):
        (self.workflows / name).write_text(
            "on: push\njobs:\n  test:\n    steps:\n      - name: filtered tests\n"
            f"        run: {run}\n")

    def run_guard(self):
        return subprocess.run(
            ["python3", str(self.root / "scripts/check-workflow-yaml.py")],
            text=True, capture_output=True,
        )

    def test_passes_when_every_workflow_parses(self):
        self.write_workflow("ci.yml", "cargo test -p orbit-cli --locked")
        self.write_workflow(
            "ci-macos.yaml", "|\n          cargo test -p orbit-cli --locked generation_root::")
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2 workflow files parsed", result.stdout)

    def test_trailing_colon_filter_fails_with_its_line(self):
        # A plain scalar ending in `:` is a mapping key; GitHub then creates
        # no jobs for the workflow and the leg silently stops running.
        self.write_workflow("ci-macos.yml", "cargo test -p orbit-cli --locked generation_root::")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn(".github/workflows/ci-macos.yml:6: invalid YAML", result.stderr)

    def test_workflow_without_jobs_fails(self):
        (self.workflows / "empty.yml").write_text("on: push\n")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn("empty.yml: a workflow must be a mapping with a `jobs` mapping",
                      result.stderr)

    def test_homebrew_run_expressions_fail_but_env_values_pass(self):
        import yaml

        expression = "${{ needs.publish-release.outputs.version }}"
        workflow = dict(jobs={"bump-homebrew-tap": dict(steps=[
            dict(env=dict(VERSION=expression), run='git commit -m "orbit v$VERSION"'),
        ])})
        path = self.workflows / "release.yml"
        path.write_text(yaml.safe_dump(workflow))
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)

        for run in (f'VERSION="{expression}"', f'git commit -m "orbit v{expression}"',
                    f'echo safe\n# comment\necho "{expression}"'):
            with self.subTest(run=run):
                workflow["jobs"]["bump-homebrew-tap"]["steps"][0]["run"] = run
                path.write_text(yaml.safe_dump(workflow))
                result = self.run_guard()
                self.assertEqual(result.returncode, 1,
                                 "Homebrew scripts must reject expression substitution before "
                                 "Bash parses tag-derived metadata")
                self.assertIn("bump-homebrew-tap step 1", result.stderr)


class CargoDenyGuardrailTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "cargo_deny.log"
        self.fake_deny = self.bin / "cargo-deny"
        self.write_fake_deny()
        self.env = dict(
            os.environ,
            PATH=f"{self.bin}:{os.environ['PATH']}",
            FAKE_DENY_LOG=str(self.log),
            FAKE_DENY_EXIT="0",
        )
        for key in (
            "CARGO_DENY_DB_PATH",
            "ORBIT_CARGO_DENY_DB_PATH",
            "CARGO_DENY_DISABLE_FETCH",
            "ORBIT_CARGO_DENY_DISABLE_FETCH",
            "CARGO_DENY_OFFLINE",
            "ORBIT_CARGO_DENY_OFFLINE",
        ):
            self.env.pop(key, None)

    def write_fake_deny(self):
        self.fake_deny.write_text('''#!/usr/bin/env python3
import json, os, sys
log = os.environ.get("FAKE_DENY_LOG")
config_content = None
config_path = None
if "--config" in sys.argv:
    idx = sys.argv.index("--config") + 1
    if idx < len(sys.argv):
        config_path = sys.argv[idx]
        if os.path.exists(config_path):
            config_content = open(config_path).read()
entry = {
    "argv": sys.argv[1:],
    "config_path": config_path,
    "config_content": config_content,
}
if log:
    with open(log, "a") as f:
        f.write(json.dumps(entry) + "\\n")
sys.exit(int(os.environ.get("FAKE_DENY_EXIT", "0")))
''')
        self.fake_deny.chmod(0o755)

    def run_script(self, *args, env_overrides=None):
        env = dict(self.env)
        if env_overrides:
            env.update(env_overrides)
        return subprocess.run(
            ["/bin/bash", str(SCRIPTS / "cargo-deny.sh"), *args],
            env=env,
            text=True,
            capture_output=True,
        )

    def test_default_invocation_passes_through(self):
        result = self.run_script("check")
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(entries[0]["argv"], ["check"])
        self.assertIsNone(entries[0]["config_path"])

    def test_writable_override_injects_db_path_and_cleans_up(self):
        override_dir = self.root / "writable_dbs"
        override_dir.mkdir()
        result = self.run_script("check", env_overrides={"CARGO_DENY_DB_PATH": str(override_dir)})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn("--config", entries[0]["argv"])
        self.assertIn(f'db-path = "{override_dir}"', entries[0]["config_content"])
        # Temporary config file must be cleaned up on exit
        self.assertFalse(os.path.exists(entries[0]["config_path"]))

    def test_orbit_prefix_override_supported(self):
        override_dir = self.root / "orbit_writable_dbs"
        override_dir.mkdir()
        result = self.run_script("check", env_overrides={"ORBIT_CARGO_DENY_DB_PATH": str(override_dir)})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(f'db-path = "{override_dir}"', entries[0]["config_content"])

    def test_direct_repo_path_resolves_container(self):
        repo_dir = self.root / "advisory-db-3157b0e258782691"
        (repo_dir / "crates").mkdir(parents=True)
        result = self.run_script("check", env_overrides={"CARGO_DENY_DB_PATH": str(repo_dir)})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn(f'db-path = "{self.root}"', entries[0]["config_content"])

    def test_offline_mode_appends_disable_fetch(self):
        result = self.run_script("check", env_overrides={"CARGO_DENY_OFFLINE": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        entries = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertIn("--disable-fetch", entries[0]["argv"])

    def test_explicit_failure_propagated_without_skip(self):
        result = self.run_script("check", env_overrides={"FAKE_DENY_EXIT": "42"})
        self.assertEqual(result.returncode, 42)

    def test_real_cargo_deny_with_writable_fixture(self):
        if not shutil.which("cargo-deny"):
            self.skipTest("cargo-deny not installed")
        system_advisory = Path.home() / ".cargo/advisory-dbs"
        if not system_advisory.exists():
            self.skipTest("system advisory-dbs snapshot not found")
        fixture = self.root / "fixture-advisory-dbs"
        shutil.copytree(str(system_advisory), str(fixture))
        env = dict(os.environ, CARGO_DENY_DB_PATH=str(fixture), CARGO_DENY_DISABLE_FETCH="1")
        res = subprocess.run(
            ["/bin/bash", str(SCRIPTS / "cargo-deny.sh"), "check", "advisories"],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res.returncode, 0, f"stdout: {res.stdout}\nstderr: {res.stderr}")
        self.assertTrue((fixture / "db.lock").exists())

    def test_real_cargo_deny_missing_data_fails_without_skip(self):
        if not shutil.which("cargo-deny"):
            self.skipTest("cargo-deny not installed")
        empty_fixture = self.root / "empty-dbs"
        empty_fixture.mkdir()
        env = dict(os.environ, CARGO_DENY_DB_PATH=str(empty_fixture), CARGO_DENY_DISABLE_FETCH="1")
        res = subprocess.run(
            ["/bin/bash", str(SCRIPTS / "cargo-deny.sh"), "check", "advisories"],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(res.returncode, 0)
        self.assertIn("error", res.stderr.lower())


class ErrorTranslationGuardTests(unittest.TestCase):
    """Run check-error-translation.sh against a generated miniature crates tree."""

    SCRIPT = SCRIPTS / "check-error-translation.sh"

    def setUp(self):
        if not shutil.which("rg"):
            self.skipTest("ripgrep not installed")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.script = self.root / "check-error-translation.sh"
        self.write_script(self.SCRIPT.read_text())
        text = self.SCRIPT.read_text()
        crates = self.root / "crates"
        for err_type, crate, translator in re.findall(r'^  "(\w+):([\w-]+):(\w+)"$', text, re.M):
            self.write_source(crate, f"{translator}.rs", f"pub enum {err_type} {{}}\npub fn {translator}() {{}}\n")
        for err_type, crate in re.findall(r'^  "(\w+):([\w-]+)"$', text, re.M):
            self.write_source(crate, f"{err_type}.rs", f"pub enum {err_type} {{}}\n")
            self.write_source("orbit-common", f"from_{err_type}.rs",
                              f"impl From<{err_type}> for OrbitError {{}}\n")
        self.assertTrue(crates.is_dir())

    def write_script(self, text):
        self.script.write_text(text)
        self.script.chmod(0o755)

    def write_source(self, crate, name, text):
        path = self.root / "crates" / crate / "src" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run_guard(self):
        return subprocess.run(["/bin/bash", str(self.script), str(self.root)],
                              text=True, capture_output=True)

    def test_empty_allowlist_passes_a_clean_tree(self):
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("unbound variable", result.stderr)
        self.assertIn("error translation guard passed", result.stdout)

    def test_unallowlisted_translator_outside_registry_fails(self):
        self.write_source("orbit-cli", "stray.rs", "fn stray_error_to_orbit() {}\n")
        result = self.run_guard()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("defines unregistered translator 'stray_error_to_orbit'", result.stdout)

    def test_allowlisted_translator_outside_registry_passes(self):
        self.write_source("orbit-cli", "stray.rs", "fn stray_error_to_orbit() {}\n")
        text = self.SCRIPT.read_text()
        self.assertIn("allowlist=()", text)
        self.write_script(text.replace("allowlist=()", "allowlist=(stray_error_to_orbit)"))
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class OrphanModuleTests(unittest.TestCase):
    """Run the public orphan guard against disposable Rust source trees."""

    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="orphan-guard-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        self.guard = self.root / "scripts/check-orphan-modules.sh"
        shutil.copy2(SCRIPTS / self.guard.name, self.guard)
        self.source = self.root / "crates/fixture/src"

    def write_source(self, path, content):
        target = self.source / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)

    def run_guard(self):
        return subprocess.run(["bash", str(self.guard)], cwd=self.root,
                              text=True, capture_output=True, check=False, timeout=30)

    def compile_and_execute(self, expected, entrypoint="lib.rs"):
        binary = self.root / "fixture-test"
        compiled = subprocess.run(["rustc", "--edition=2024", "--test",
                                   str(self.source / entrypoint), "-o", str(binary)],
                                  text=True, capture_output=True, timeout=60)
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        listing = subprocess.run([str(binary), "--list"], text=True,
                                 capture_output=True, timeout=30)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertEqual(set(re.findall(r"^(.*): test$", listing.stdout, re.MULTILINE)),
                         expected)
        executed = subprocess.run([str(binary), "--nocapture"], text=True,
                                  capture_output=True, timeout=30)
        self.assertEqual(executed.returncode, 0, executed.stdout + executed.stderr)
        self.assertEqual(set(re.findall(r"^test (.*) \.\.\. ok$", executed.stdout,
                                        re.MULTILINE)), expected)

    def test_spaced_and_escaped_paths_register_only_compiler_destinations(self):
        # Source spellings are Rust escapes, not Python's decoded strings.
        variants = (
            ("spaced", '#[ path = "checks/spaced.rs"]', "checks/spaced.rs"),
            ("hex", r'#[path = "checks/probe\x2ers"]', "checks/probe.rs"),
            ("unicode", r'#[ path = "checks/\u{e_9}.rs" ]', "checks/é.rs"),
            ("quoted", r'''#[path = "checks/quo\"te\\apos\'trophe.rs"]''',
             'checks/quo"te\\apos\'trophe.rs'),
            ("continued", '#[path = "checks/\\\n \tcontinued.rs"]', "checks/continued.rs"),
            ("controls", r'#[path = "checks/tab\tline\nreturn\r.rs"]',
             "checks/tab\tline\nreturn\r.rs"),
        )
        expected = set()
        orphans = set()
        for directory, prefix in (("", ""), ("tests/", "tests::")):
            owner = '#[cfg(test)] mod tests;\n' if not directory else ""
            for name, attribute, destination in variants:
                owner += f'{attribute} mod {name};\n'
                self.write_source(directory + destination,
                                  "#[test] fn registered() { assert_eq!(2 + 2, 4); }\n")
                expected.add(f"{prefix}{name}::registered")
                orphans.add(directory + name + ".rs")
            owner += ('/* #[ path = "comment.rs"] mod comment; */\n'
                      'const TEXT: &str = r#"#[ path = "string.rs"] mod string;"#;\n')
            orphans.update({directory + "comment.rs", directory + "string.rs"})
            self.write_source(directory + ("mod.rs" if directory else "lib.rs"), owner)

        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for path in orphans:
            self.write_source(path, '#[test] fn unregistered() { panic!("orphan"); }\n')
        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^orphan(?: test)? module: (\S+)",
                                        result.stdout, re.MULTILINE)),
                         {f"crates/fixture/src/{path}" for path in orphans})

    def test_separated_attribute_openings_match_compiler_destinations(self):
        variants = (
            ("space", '# [path = "checks/space.rs"]', "checks/space.rs"),
            ("multiline", '#\t\r\n[ path = "checks/multiline.rs" ]',
             "checks/multiline.rs"),
            ("block", '#/* outer /* nested [ ] */ comment */[path = "checks/block.rs"]',
             "checks/block.rs"),
            ("line", '#// ignored [ ]\n[path = r#"checks/line.rs"#]', "checks/line.rs"),
            ("escaped", r'# /* gap */ [path = "checks/escaped\x2ers" /* trailing */]',
             "checks/escaped.rs"),
            ("raw", '# /* gap */ [path = r"checks/mod.rs" // trailing ]\n]',
             "checks/mod.rs"),
        )
        expected = set()
        orphans = set()
        for directory, prefix in (("", ""), ("tests/", "tests::")):
            owner = '#[cfg(test)] mod tests;\n' if not directory else ""
            for name, attribute, destination in variants:
                owner += f'{attribute} mod {name};\n'
                self.write_source(directory + destination,
                                  "#[test] fn registered() { assert_eq!(3_u32.checked_add(4), Some(7)); }\n")
                expected.add(f"{prefix}{name}::registered")
                orphans.add(directory + name + ".rs")
            owner += ('// # [path = "line_text.rs"] mod line_text;\n'
                      '/* # /* gap */ [path = "block_text.rs"] mod block_text; */\n'
                      'const TEXT: &str = "# [path = \\"string_text.rs\\"] mod string_text;";\n'
                      'const RAW: &str = r##"# /* gap */ [path = r#"raw_text.rs"#] '
                      'mod raw_text;"##;\n')
            orphans.update(directory + name + ".rs" for name in (
                "line_text", "block_text", "string_text", "raw_text", "unregistered"))
            self.write_source(directory + ("mod.rs" if directory else "lib.rs"), owner)

        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for path in orphans:
            self.write_source(path, '#[test] fn unregistered() { panic!("orphan"); }\n')
        # rustc must still execute only the redirected files after the default
        # destinations, inert lookalikes and a genuine orphan appear.
        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^orphan(?: test)? module: (\S+)",
                                        result.stdout, re.MULTILINE)),
                         {f"crates/fixture/src/{path}" for path in orphans})

    def check_trailing_comment_paths(self, destination):
        variants = (
            ("plain", '"{}" /* explanation */'),
            ("raw", 'r"{}" /* explanation */'),
            ("hashed", 'r#"{}"# /* outer /* nested ] */ comment */'),
            ("multi_hash", 'r###"{}"### // closing ] is in a comment\n'),
        )
        expected = set()
        orphans = set()
        for directory, prefix in (("", ""), ("tests/", "tests::")):
            owner = '#[cfg(test)] mod tests;\n' if not directory else ""
            for name, literal in variants:
                # Comment-like text inside the literal must remain path data.
                path = f"{name}//{destination}"
                owner += f'#[path = {literal.format(path)}] mod {name};\n'
                self.write_source(directory + path, "#[test] fn registered() {}\n")
                expected.add(f"{prefix}{name}::registered")
                orphans.add(directory + name + ".rs")
            owner += ('/* #[path = "comment.rs" /* trailing */] mod comment; */\n'
                      'const TEXT: &str = r##"#[path = r#"string.rs"# '
                      '/* trailing */] mod string;"##;\n')
            orphans.update({directory + "comment.rs", directory + "string.rs"})
            self.write_source(directory + ("mod.rs" if directory else "lib.rs"), owner)

        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for path in orphans:
            self.write_source(path, '#[test] fn unregistered() { panic!("orphan"); }\n')
        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^(orphan(?: test)? module): (\S+)",
                                        result.stdout, re.MULTILINE)), {
            ("orphan test module" if path.startswith("tests/") else "orphan module",
             f"crates/fixture/src/{path}") for path in orphans
        })

    def test_trailing_comment_paths_accept_compiler_registered_files(self):
        self.check_trailing_comment_paths("live.rs")

    def test_trailing_comment_paths_reject_uncompiled_default_files(self):
        # mod.rs is exempt from candidate checks: a missed override otherwise
        # silently licenses the default sibling without any false-positive clue.
        self.check_trailing_comment_paths("mod.rs")

    def test_symlinked_crate_roots_register_only_their_logical_children(self):
        physical = self.root / "owner.rs"
        physical.write_text('#[path="tests/case.rs"] mod case;\nmod ordinary;\n')
        entrypoints = ("lib.rs", "main.rs", "bin/secondary/main.rs")
        for entrypoint in entrypoints:
            link = self.source / entrypoint
            link.parent.mkdir(parents=True, exist_ok=True)
            link.symlink_to(os.path.relpath(physical, link.parent))
            for child in ("tests/case.rs", "ordinary.rs"):
                self.write_source(str(Path(entrypoint).parent / child),
                                  "#[test] fn registered() { "
                                  "assert_eq!(3_u32.checked_add(4), Some(7)); }\n")
        # Shared physical roots must each keep their own logical directory,
        # even when another root has already populated the source cache.
        expected = {"case::registered", "ordinary::registered"}
        for entrypoint in entrypoints:
            self.compile_and_execute(expected, entrypoint)
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        self.write_source("tests/lost.rs", '#[test] fn unregistered() { panic!("orphan"); }\n')
        for entrypoint in entrypoints:
            self.compile_and_execute(expected, entrypoint)
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "orphan test module: crates/fixture/src/tests/lost.rs — no module declaration resolves to this file",
        ])

    def test_symlinked_owners_preserve_each_logical_child_directory(self):
        # Three logical owners share one physical file outside the src scan.
        physical = self.root / "shared/owner.rs"
        physical.parent.mkdir()
        physical.write_text('mod child;\nmod inner { mod leaf; }\n'
                            '#[path="relative.rs"] mod relative;\n')
        self.write_source("lib.rs", 'mod parent;\nmod alternate;\n'
                          '#[path="redirect/alias.rs"] mod redirected;\nmod repeated;\n')
        for path in ("parent.rs", "alternate.rs", "redirect/alias.rs"):
            link = self.source / path
            link.parent.mkdir(parents=True, exist_ok=True)
            link.symlink_to(os.path.relpath(physical, link.parent))
        expected = set()
        for directory, prefix in (("parent/", "parent"), ("alternate/", "alternate"),
                                  ("redirect/", "redirected")):
            for child, module in (("child.rs", "child"), ("inner/leaf.rs", "inner::leaf")):
                self.write_source(directory + child, "#[test] fn registered() {}\n")
                expected.add(f"{prefix}::{module}::registered")
        for path in ("relative.rs", "redirect/relative.rs"):
            self.write_source(path, "#[test] fn registered() {}\n")
        expected.update(f"{prefix}::relative::registered"
                        for prefix in ("parent", "alternate", "redirected"))
        # Rust also permits nested reuse of one physical owner when each
        # logical directory eventually leads to a different leaf file.
        repeated = physical.parent / "repeated.rs"
        repeated.write_text("mod child;\n")
        for path in ("repeated.rs", "repeated/child.rs"):
            link = self.source / path
            link.parent.mkdir(parents=True, exist_ok=True)
            link.symlink_to(os.path.relpath(repeated, link.parent))
        self.write_source("repeated/child/child.rs", "#[test] fn registered() {}\n")
        expected.add("repeated::child::child::registered")
        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        self.write_source("parent/lost.rs", '#[test] fn unregistered() { panic!("orphan"); }\n')
        self.compile_and_execute(expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "orphan module: crates/fixture/src/parent/lost.rs — no module declaration resolves to this file",
        ])

        # A declared directory symlink can grow logical paths indefinitely.
        # The public guard must terminate without registering unrelated files.
        self.write_source("cycle.rs", '#[path="loop/cycle.rs"] mod again;\n')
        (self.source / "loop").symlink_to(".", target_is_directory=True)
        self.write_source("lib.rs", (self.source / "lib.rs").read_text() + "mod cycle;\n")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "orphan module: crates/fixture/src/parent/lost.rs — no module declaration resolves to this file",
        ])

    def test_redirected_parent_children_inherit_the_resolved_module_directory(self):
        self.write_source("lib.rs", '#[path="other.rs"] mod renamed;\n'
                          '#[cfg(test)] mod tests;\n'
                          'mod shared;\n#[path="shared.rs"] mod alias;\n')
        self.write_source("tests/mod.rs", '#[path="other.rs"] mod renamed;\n')
        # One physical file may have both ordinary and redirected ownership.
        self.write_source("shared.rs", "mod dual;\n")
        for path in ("dual.rs", "shared/dual.rs"):
            self.write_source(path, "#[test] fn live() {}\n")
        expected = {"shared::dual::live", "alias::dual::live"}
        orphans = set()
        for directory, prefix in (("", ""), ("tests/", "tests::")):
            self.write_source(directory + "other.rs", 'mod child;\n'
                              'mod inner { mod leaf; }\n'
                              '#[path="nested/redirect.rs"] mod redirected;\n')
            self.write_source(directory + "child.rs", 'mod deep;\n'
                              '#[test] fn live() {}\n')
            self.write_source(directory + "nested/redirect.rs", 'mod child;\n'
                              'mod inner { mod leaf; }\n')
            destinations = {
                "child/deep.rs": "renamed::child::deep",
                "inner/leaf.rs": "renamed::inner::leaf",
                "nested/child.rs": "renamed::redirected::child",
                "nested/inner/leaf.rs": "renamed::redirected::inner::leaf",
            }
            expected.add(prefix + "renamed::child::live")
            for path, module in destinations.items():
                self.write_source(directory + path, "#[test] fn live() {}\n")
                expected.add(prefix + module + "::live")
            # Neither the declared name nor the redirected source's stem
            # licenses an ordinary destination or a child directory.
            orphans.update(directory + path for path in (
                "renamed.rs", "other/child.rs", "other/inner/leaf.rs",
                "nested/redirected.rs", "nested/redirect/child.rs",
            ))

        binary = self.root / "fixture-test"
        compiled = subprocess.run(["rustc", "--edition=2024", "--test",
                                   str(self.source / "lib.rs"), "-o", str(binary)],
                                  text=True, capture_output=True)
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        listing = subprocess.run([str(binary), "--list"], text=True, capture_output=True)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertEqual(set(re.findall(r"^(.*): test$", listing.stdout, re.MULTILINE)),
                         expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        for path in orphans:
            self.write_source(path, '#[test] fn never_registered() { panic!("lost test"); }\n')
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^orphan(?: test)? module: (\S+)",
                                        result.stdout, re.MULTILINE)),
                         {f"crates/fixture/src/{path}" for path in orphans})

    def test_ignored_module_owners_still_register_their_children(self):
        # Candidate files are checked regardless of ignore rules, so owners
        # must remain visible too, including directories named like outputs.
        (self.root / ".ignore").write_text("build/\n")
        self.write_source("lib.rs", "mod build;\n#[cfg(test)] mod tests;\n")
        self.write_source("tests/mod.rs", "mod build;\n")
        for directory in ("build/", "tests/build/"):
            self.write_source(f"{directory}mod.rs", "mod live;\n")
            self.write_source(f"{directory}live.rs", "#[test] fn registered() {}\n")

        binary = self.root / "fixture-test"
        compiled = subprocess.run(["rustc", "--edition=2024", "--test",
                                   str(self.source / "lib.rs"), "-o", str(binary)],
                                  text=True, capture_output=True)
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        listing = subprocess.run([str(binary), "--list"], text=True, capture_output=True)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertEqual(set(re.findall(r"^(.*): test$", listing.stdout, re.MULTILINE)), {
            "build::live::registered", "tests::build::live::registered",
        })
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        for directory in ("build/", "tests/build/"):
            self.write_source(f"{directory}lost.rs", "#[test] fn never_runs() {}\n")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^(orphan(?: test)? module): (\S+)",
                                        result.stdout, re.MULTILINE)), {
            ("orphan module", "crates/fixture/src/build/lost.rs"),
            ("orphan test module", "crates/fixture/src/tests/build/lost.rs"),
        })

    def test_path_override_does_not_register_same_name_files(self):
        self.write_source("lib.rs", '#[path="other.rs"] mod lost;\n'
                          'mod ordinary;\n#[cfg(test)] mod tests;\n')
        self.write_source("tests/mod.rs", '#[path="other.rs"] mod lost;\n'
                          'mod ordinary;\n')
        for directory in ("", "tests/"):
            self.write_source(f"{directory}other.rs", "#[test] fn live() {}\n")
            self.write_source(f"{directory}ordinary.rs", "#[test] fn plain() {}\n")

        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        for directory in ("", "tests/"):
            self.write_source(f"{directory}lost.rs",
                              '#[test] fn never_runs() { panic!("lost test"); }\n')
        binary = self.root / "fixture-test"
        compiled = subprocess.run(["rustc", "--edition=2024", "--test",
                                   str(self.source / "lib.rs"), "-o", str(binary)],
                                  text=True, capture_output=True)
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        listing = subprocess.run([str(binary), "--list"], text=True, capture_output=True)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertEqual(set(re.findall(r"^(.*): test$", listing.stdout, re.MULTILINE)), {
            "lost::live", "ordinary::plain", "tests::lost::live", "tests::ordinary::plain",
        })

        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^(orphan(?: test)? module): (\S+)",
                                        result.stdout, re.MULTILINE)), {
            ("orphan module", "crates/fixture/src/lost.rs"),
            ("orphan test module", "crates/fixture/src/tests/lost.rs"),
        })

    def test_path_attribute_only_registers_its_resolved_file(self):
        self.write_source("lib.rs", '#[path = "feature.rs"] mod production;\n'
                          '#[cfg(test)] mod tests;\n')
        self.write_source("feature.rs", "pub fn feature() {}\n")
        self.write_source("tests/mod.rs", "")
        self.write_source("tests/feature.rs", '#[test] fn lost_coverage() { panic!("must run"); }\n')
        self.write_source("unregistered/feature.rs", "pub fn unregistered() {}\n")

        # The compiler proves the colliding test is absent, rather than merely
        # relying on the guard's interpretation of module registration.
        binary = self.root / "fixture-test"
        compiled = subprocess.run(["rustc", "--test", str(self.source / "lib.rs"),
                                   "-o", str(binary)], text=True, capture_output=True)
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        listing = subprocess.run([str(binary), "--list"], text=True, capture_output=True)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertIn("0 tests, 0 benchmarks", listing.stdout)

        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^(orphan(?: test)? module): (\S+)",
                                        result.stdout, re.MULTILINE)), {
            ("orphan test module", "crates/fixture/src/tests/feature.rs"),
            ("orphan module", "crates/fixture/src/unregistered/feature.rs"),
        })

    def test_genuine_paths_and_lexical_declarations_still_register(self):
        self.write_source("lib.rs", '#[path = "feature.rs"] mod production;\n'
                          'mod nested;\n#[cfg(test)] mod tests;\n')
        self.write_source("feature.rs", "")
        self.write_source("nested/mod.rs", '#[path = "./../relative.rs"] mod relocated;\n'
                          '#[cfg(any())]\n#[path = "disabled.rs"] mod inactive;\n')
        self.write_source("relative.rs", "")
        self.write_source("nested/disabled.rs", "")
        self.write_source("tests/mod.rs", '#[path = "../nested/../tests/feature.rs"] mod renamed;\n'
                          'mod r#type;\n/* mod lost; */\n'
                          'const TEXT: &str = r#"mod lost;"#;\n')
        self.write_source("tests/feature.rs", "")
        self.write_source("tests/type.rs", "")

        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        # Neither a comment nor a raw string declares the test. Conditional
        # declarations above remain accepted without evaluating their cfg.
        self.write_source("tests/lost.rs", "")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("orphan test module: crates/fixture/src/tests/lost.rs", result.stdout)

    def test_raw_path_literals_match_compiler_and_reject_false_registrations(self):
        variants = {
            "plain": lambda path: f'"{path}"',
            "raw": lambda path: f'r"{path}"',
            "hashed": lambda path: f'r#"{path}"#',
            "multi_hash": lambda path: f'r###"{path}"###',
        }
        expected = set()
        orphans = set()
        for directory, module_prefix in (("", ""), ("tests/", "tests::")):
            owner = '#[cfg(test)] mod tests;\n' if not directory else ""
            for name, literal in variants.items():
                owner += (f'#[path = {literal(f"{name}/direct.rs")}] mod direct_{name};\n'
                          f'mod {name} {{ #[path = {literal("chosen.rs")}] mod renamed; }}\n'
                          f'#[path = {literal(f"relocated_{name}")}] mod override_{name} {{\n'
                          f'    #[path = {literal("chosen.rs")}] mod renamed;\n'
                          '}\n')
                for path, module in (
                    (f"{name}/direct.rs", f"direct_{name}"),
                    (f"{name}/chosen.rs", f"{name}::renamed"),
                    (f"relocated_{name}/chosen.rs", f"override_{name}::renamed"),
                ):
                    self.write_source(directory + path, "#[test] fn registered() {}\n")
                    expected.add(f"{module_prefix}{module}::registered")
                # A path override must not license the default same-name file
                # or an unrelated file with the destination's basename.
                for path in (f"{name}/renamed.rs", f"unregistered/{name}/chosen.rs"):
                    orphans.add(directory + path)
            owner += ('/* #[path = r#"comment.rs"#] mod comment_only; */\n'
                      'const TEXT: &str = r####"#[path = r###"raw_text.rs"###] '
                      'mod string_only;"####;\n')
            orphans.update({directory + "comment.rs", directory + "raw_text.rs"})
            self.write_source(directory + ("mod.rs" if directory else "lib.rs"), owner)

        binary = self.root / "fixture-test"
        compiled = subprocess.run(["rustc", "--edition=2024", "--test",
                                   str(self.source / "lib.rs"), "-o", str(binary)],
                                  text=True, capture_output=True)
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        listing = subprocess.run([str(binary), "--list"], text=True, capture_output=True)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertEqual(set(re.findall(r"^(.*): test$", listing.stdout, re.MULTILINE)), expected)
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        for path in orphans:
            self.write_source(path, '#[test] fn never_registered() { panic!("lost test"); }\n')
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^orphan(?: test)? module: (\S+)",
                                        result.stdout, re.MULTILINE)),
                         {f"crates/fixture/src/{path}" for path in orphans})

    def test_inline_path_directories_match_compiler_and_reject_collisions(self):
        self.write_source("lib.rs", 'mod external;\nmod directory;\n'
                          'mod inner {\n'
                          '    #[path = "chosen.rs"] pub(crate) mod renamed;\n'
                          '    mod r#type { #[path = "chosen.rs"] mod renamed; }\n'
                          '}\n'
                          'mod sibling { #[path = "chosen.rs"] mod renamed; }\n')
        # Both mod.rs and ordinary file modules contain nested inline modules.
        # An explicit inline path overrides the ordinary file's stem directory.
        owner = ('const TEXT: &str = r#"mod misleading { }"#;\n'
                 '/* mod misleading { */\n'
                 'mod inner {\n'
                 '    fn braces() { let _ = "}"; }\n'
                 '    #[path = "chosen.rs"] mod renamed;\n'
                 '    mod deep { #[path = "./chosen.rs"] mod renamed; }\n'
                 '}\n'
                 '#[path = "relocated"] mod overridden {\n'
                 '    #[path = "chosen.rs"] mod renamed;\n'
                 '    mod deep { #[path = "chosen.rs"] mod renamed; }\n'
                 '}\n'
                 '#[path = "after.rs"] mod after;\n')
        self.write_source("external.rs", owner)
        self.write_source("directory/mod.rs", owner)
        destinations = {
            "inner/chosen.rs": "inner::renamed",
            "inner/type/chosen.rs": "inner::r#type::renamed",
            "sibling/chosen.rs": "sibling::renamed",
            "external/inner/chosen.rs": "external::inner::renamed",
            "external/inner/deep/chosen.rs": "external::inner::deep::renamed",
            "relocated/chosen.rs": "external::overridden::renamed",
            "relocated/deep/chosen.rs": "external::overridden::deep::renamed",
            "after.rs": "external::after",
            "directory/inner/chosen.rs": "directory::inner::renamed",
            "directory/inner/deep/chosen.rs": "directory::inner::deep::renamed",
            "directory/relocated/chosen.rs": "directory::overridden::renamed",
            "directory/relocated/deep/chosen.rs": "directory::overridden::deep::renamed",
            "directory/after.rs": "directory::after",
        }
        for path in destinations:
            self.write_source(path, "#[test] fn registered_inline_path() {}\n")

        binary = self.root / "fixture-test"
        compiled = subprocess.run(["rustc", "--test", str(self.source / "lib.rs"),
                                   "-o", str(binary)], text=True, capture_output=True)
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        listing = subprocess.run([str(binary), "--list"], text=True, capture_output=True)
        self.assertEqual(listing.returncode, 0, listing.stdout + listing.stderr)
        self.assertEqual(set(re.findall(r"^(.*): test$", listing.stdout, re.MULTILINE)),
                         {f"{module}::registered_inline_path" for module in destinations.values()})

        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        # Neither a root-relative guess nor another same-basename file may be
        # mistaken for one of the compiler-verified inline destinations.
        for path in ("chosen.rs", "unregistered/chosen.rs", "tests/chosen.rs"):
            self.write_source(path, '#[test] fn lost_coverage() { panic!("must run"); }\n')
        result = self.run_guard()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(set(re.findall(r"^(orphan(?: test)? module): (\S+)",
                                        result.stdout, re.MULTILINE)), {
            ("orphan module", "crates/fixture/src/chosen.rs"),
            ("orphan module", "crates/fixture/src/unregistered/chosen.rs"),
            ("orphan test module", "crates/fixture/src/tests/chosen.rs"),
        })


class PythonPreflightTests(unittest.TestCase):
    """Drive scripts/require-python.sh and the make gates with a stub python3."""

    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="python-preflight-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.bin = Path(self.temporary.name)
        self.log = self.bin / "cargo.log"

    def stub(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def stub_python(self, version):
        self.stub("python3", f"#!/bin/sh\necho {version}\n")

    def env(self):
        # The stub directory comes first; /usr/bin:/bin keep the shell tools.
        return dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", GUARD_TEST_LOG=str(self.log))

    def run_command(self, *command):
        return subprocess.run(list(command), env=self.env(), text=True, capture_output=True,
                              cwd=SCRIPTS.parent)

    def test_python_39_fails_naming_minimum_and_interpreter(self):
        self.stub_python("3.9.6")
        result = self.run_command("/bin/bash", str(SCRIPTS / "require-python.sh"))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertEqual(len(result.stderr.strip().splitlines()), 1, result.stderr)
        self.assertIn("Python >= 3.11", result.stderr)
        self.assertIn(f"python3 3.9.6 at {self.bin}/python3", result.stderr)

    def test_python_311_and_newer_pass_silently(self):
        for version in ("3.11.0", "3.12.4", "3.14.4", "4.0.0"):
            with self.subTest(version=version):
                self.stub_python(version)
                result = self.run_command("/bin/bash", str(SCRIPTS / "require-python.sh"))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout + result.stderr, "")

    def test_unreadable_version_and_missing_python_fail(self):
        self.stub("python3", "#!/bin/sh\nexit 1\n")
        result = self.run_command("/bin/bash", str(SCRIPTS / "require-python.sh"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown version", result.stderr)
        (self.bin / "python3").unlink()
        result = subprocess.run(["/bin/bash", str(SCRIPTS / "require-python.sh")],
                                env=dict(self.env(), PATH=str(self.bin)),
                                text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no python3 is on PATH", result.stderr)

    def test_make_gates_stop_before_running_anything_else(self):
        self.stub_python("3.9.6")
        self.stub("cargo", '#!/bin/sh\necho "$@" >> "$GUARD_TEST_LOG"\n')
        for target in ("ci-fast", "ci-lint"):
            with self.subTest(target=target):
                result = self.run_command("make", target, f"CARGO={self.bin}/cargo")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("python3 3.9.6", result.stderr)
                self.assertIn("Python >= 3.11", result.stderr)
                self.assertFalse(self.log.exists(), "a gate ran before the Python preflight")

    def test_ci_guardrails_runs_preflight_before_any_gate(self):
        self.stub_python("3.9.6")
        self.stub("rg", "#!/bin/sh\nexit 1\n")
        self.stub("cargo", '#!/bin/sh\necho "$@" >> "$GUARD_TEST_LOG"\n')
        result = self.run_command("/bin/bash", str(SCRIPTS / "ci-guardrails.sh"), "--fast")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Python >= 3.11", result.stderr)
        self.assertFalse(self.log.exists(), "a gate ran before the Python preflight")


if __name__ == "__main__":
    unittest.main()
