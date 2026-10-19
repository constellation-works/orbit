#!/usr/bin/env python3
"""Execute PR-state and compiler-cache workflow controls offline."""

import http.server
import json
import os
from pathlib import Path
import re
import self_test_hang
import shutil
import signal
import sys
import tempfile
import textwrap
import threading
import time
import unittest

from workflow_helper_tests import WorkflowHelperRevisionTests

try:
    import yaml
except ImportError:
    yaml = None


ROOT = Path(__file__).resolve().parent.parent
RESPONSES = json.loads((ROOT / "scripts/fixtures/pr-state-responses.json").read_text())["responses"]


def evaluate(expression, event, state="", *, cancelled=False, dependency_skipped=False,
             success=True, bindings=None):
    """Evaluate the small expression subset used by these admission conditions.

    This is an offline condition fixture, not a GitHub Actions scheduler. The
    skipped-dependency rule still needs a status function to override implicit
    success(); model it for the non-PR case where the gate itself is skipped.
    """
    expression = expression.removeprefix("${{").removesuffix("}}").strip()
    if dependency_skipped and not re.search(r"(?:cancelled|always)\(\)", expression):
        return False
    expression = expression.replace("github.event_name", repr(event))
    expression = expression.replace("needs.pr-state.outputs.state", repr(state))
    for name, value in (bindings or {}).items():
        expression = expression.replace(name, repr(value))
    expression = expression.replace("success()", str(success))
    expression = expression.replace("cancelled()", str(cancelled)).replace("always()", "True")
    expression = expression.replace("&&", " and ").replace("||", " or ")
    expression = re.sub(r"!(?!=)", " not ", expression)
    return bool(eval(expression.strip(), {"__builtins__": {}},
                     {"contains": lambda search, item: item.lower() in search.lower()}))


SCOPE = "needs.scope.outputs.mode"


def scope_admits(name, job, mode):
    """Which jobs a ci.yml change scope admits: the scope job always runs, the
    docs job only for `docs`, and every other scoped job unless `docs` or
    `skip`. An empty mode (a failed or skipped scope job) admits the full run."""
    if name == "scope" or SCOPE not in job["if"]:
        return True
    if f"{SCOPE} == 'docs'" in job["if"]:
        return mode == "docs"
    return mode not in ("docs", "skip")


@unittest.skipIf(yaml is None, "PyYAML not installed; the workflow guard skips locally without it")
class PrStateWorkflowTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="pr-state-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.output = self.root / "output"
        self.credentials = self.root / "credentials.json"
        self.requests = []
        self.response = {}
        self.api_failure = False
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests.append({"method": self.command, "path": self.path})
                # A bodyless gateway failure preserves the no-output failure
                # case; successful malformed payloads are separate controls.
                body = b"" if fixture.api_failure else json.dumps(fixture.response).encode()
                self.send_response(503 if fixture.api_failure else 200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(body)

            do_POST = do_PATCH = do_DELETE = do_GET

            def log_message(self, *args):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01})
        thread.start()

        def close_server():
            server.shutdown()
            server.server_close()
            thread.join()

        self.addCleanup(close_server)
        real_gh = shutil.which("gh")
        self.assertIsNotNone(real_gh, "the offline fixture requires the GitHub CLI used by the workflow")
        # Redirect only the transport endpoint; real gh handles placeholders,
        # method selection, JSON projection and errors, and real Bash executes.
        gh = self.root / "gh"
        gh.write_text("""#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
Path(os.environ["FIXTURE_CREDENTIALS"]).write_text(json.dumps({"token": os.environ.get("GH_TOKEN")}))
args = [os.environ["FIXTURE_ENDPOINT"] + arg if arg.startswith("repos/") else arg
        for arg in sys.argv[1:]]
os.execv(os.environ["FIXTURE_GH"], [os.environ["FIXTURE_GH"], *args])
""")
        gh.chmod(0o755)
        self.fixture_env = {"FIXTURE_GH": real_gh,
                            "FIXTURE_CREDENTIALS": str(self.credentials),
                            "FIXTURE_ENDPOINT": f"http://127.0.0.1:{server.server_port}/"}
        self.workflows = {}
        for path in sorted((ROOT / ".github/workflows").glob("*.y*ml")):
            document = yaml.safe_load(path.read_text())
            # YAML 1.1 reads the unquoted Actions key `on` as True.
            if "pull_request" in document.get("on", document.get(True, {})):
                self.workflows[path.name] = document
        self.assertTrue(self.workflows, "the fixture must exercise PR workflows")
        # Frozen original runs cannot acquire the new command by rewriting
        # today's workflows. Execute the historical source through real gh,
        # jq and Bash as well as the projected command.
        legacy = ROOT / "crates/orbit-engine/tests/engine/pr_landing/infrastructure/fixtures/legacy-codeql.yml"
        self.workflows["legacy-codeql"] = yaml.safe_load(legacy.read_text())

    def run_gate(self, gate, response, *, api_failure=False):
        self.output.write_text("")
        self.response = response
        self.api_failure = api_failure
        self.requests.clear()
        self.credentials.write_text("{}")
        step = gate["steps"][0]
        bindings = {"${{ github.token }}": "offline-token",
                    "${{ github.event.pull_request.number }}": "42",
                    "${{ github.repository }}": "fixture/repository"}
        env = dict(PATH=f"{self.root}:{os.environ['PATH']}", LANG="C",
                   HOME=str(self.root), GH_CONFIG_DIR=str(self.root / "gh-config"),
                   GITHUB_REPOSITORY="fixture/repository", GITHUB_OUTPUT=str(self.output),
                   **self.fixture_env)
        env.update({key: bindings.get(value, value) for key, value in step["env"].items()})
        script = step["run"]
        for expression, value in bindings.items():
            script = script.replace(expression, value)
        result = self_test_hang.run(
            [step["shell"], "-e", "-o", "pipefail", "-c", script],
            step=str(step.get("id") or "pr-state"),
            cwd=self.root, env=env, text=True, capture_output=True)
        self.assertEqual(self.requests, [{"method": "GET", "path": "/repos/fixture/repository/pulls/42"}],
                         "each gate must make exactly one read of the bound pull request")
        self.assertEqual(json.loads(self.credentials.read_text()), {"token": "offline-token"})
        outputs = dict(line.split("=", 1) for line in self.output.read_text().splitlines())
        # Follow the actual step-to-job output binding consumed by needs.
        binding = gate["outputs"]["state"]
        match = re.fullmatch(r"\$\{\{ steps\.([\w-]+)\.outputs\.([\w-]+) \}\}", binding)
        self.assertIsNotNone(match, "gate must publish its actual step output")
        self.assertEqual(match[1], step["id"])
        return result, outputs.get(match[2], "")

    def assert_admission(self, document, event, state, expected, *, cancelled=False, mode="full"):
        for name, job in document["jobs"].items():
            if name == "pr-state":
                continue
            with self.subTest(job=name, event=event, state=state, cancelled=cancelled, mode=mode):
                needs = job.get("needs", [])
                if isinstance(needs, str):
                    needs = [needs]
                self.assertIn("pr-state", needs, "build jobs must wait for live PR state")
                self.assertEqual(evaluate(job["if"], event, state, cancelled=cancelled,
                                          dependency_skipped=event != "pull_request",
                                          bindings={SCOPE: mode}),
                                 expected and scope_admits(name, job, mode))

    def test_live_states_admit_open_and_skip_merged_or_closed_jobs(self):
        for name, document in self.workflows.items():
            gate = document["jobs"]["pr-state"]
            with self.subTest(workflow=name):
                self.assertTrue(evaluate(gate["if"], "pull_request"))
                self.assertEqual(gate["permissions"], {"pull-requests": "read"})
                self.assertEqual(gate["runs-on"], "ubuntu-latest")
                self.assertLessEqual(gate["timeout-minutes"], 2)
                self.assertEqual(len(gate["steps"]), 1, "gate must run without checking out PR code")
            for state, response in RESPONSES.items():
                with self.subTest(workflow=name, response=state):
                    result, output = self.run_gate(gate, response)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(output, state)
                    self.assert_admission(document, "pull_request", output, state == "open")
                    print(f"{name}: live {state} -> build jobs {'admitted' if state == 'open' else 'skipped'}")

    def test_non_pr_events_skip_gate_and_admit_existing_jobs(self):
        for name, document in self.workflows.items():
            for event in document.get("on", document.get(True, {})):
                if event == "pull_request":
                    continue
                with self.subTest(workflow=name, event=event):
                    self.assertFalse(evaluate(document["jobs"]["pr-state"]["if"], event))
                    self.assert_admission(document, event, "", True)
                    self.assert_admission(document, event, "", False, cancelled=True)
                    print(f"{name}: {event} -> gate skipped, build jobs admitted")

    def test_api_errors_fail_gate_without_admitting_unknown_state(self):
        for name, document in self.workflows.items():
            for response, api_failure in ((RESPONSES["open"], True), ({}, False),
                                          ({"state": "unexpected", "merged": False}, False)):
                with self.subTest(workflow=name, response=response, api_failure=api_failure):
                    result, output = self.run_gate(document["jobs"]["pr-state"], response,
                                                   api_failure=api_failure)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(output, "")
                    self.assert_admission(document, "pull_request", output, False)


@unittest.skipIf(yaml is None, "PyYAML is required to execute workflow controls")
class CompilerCacheWorkflowTests(unittest.TestCase):
    """Execute the ordinary CI cache contract, without network or cache uploads."""

    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="compiler-workflow-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.document = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        self.job = self.document["jobs"]["ci"]
        self.steps = {step["id"]: step for step in self.job["steps"] if "id" in step}
        self.output = self.root / "output"
        self.envfile = self.root / "env"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = dict(PATH=f"{self.bin}:{os.environ['PATH']}", HOME=str(self.root / "home"),
                        GITHUB_OUTPUT=str(self.output), GITHUB_ENV=str(self.envfile),
                        RUNNER_TEMP=str(self.root / "runner temp"),
                        CARGO_INCREMENTAL=self.document["env"]["CARGO_INCREMENTAL"],
                        COMPILER_CACHE_RESTORE_STARTED="0", RESTORE_OUTCOME="success")
        # Resolve job env before any runner exists, as Actions does. This
        # fixture supports property lookups; runner/env/steps are unavailable
        # at this boundary and must not be silently replaced by fixture data.
        contexts = {name: {} for name in ("github", "needs", "strategy", "matrix", "vars", "secrets", "inputs")}

        def resolve(match):
            value = contexts
            for part in match[1].strip().split("."):
                value = value[part]
            return str(value)

        self.env.update({name: re.sub(r"\$\{\{(.*?)\}\}", resolve, str(value))
                         for name, value in self.job["env"].items()})
        for step in self.job["steps"]:
            if step.get("id") == "compiler-key":
                break
            if step.get("id") == "compiler-env":
                self.env.update(self.run_step(step)[1])
        self.assertIn("SCCACHE_DIR", self.env,
                      "runtime environment must reach cache identity before restore and setup")

    def write(self, name, text, *, executable=False):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        if executable:
            path.chmod(0o755)
        return path

    def run_step(self, step, timeout=None):
        self.output.write_text("")
        self.envfile.write_text("")
        result = self_test_hang.run(
            ["bash", "-e", "-o", "pipefail", "-c", step["run"]],
            step=str(step.get("id") or step.get("name") or "workflow step"),
            cwd=self.root, env=self.env, text=True, capture_output=True, timeout=timeout)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        outputs = dict(line.split("=", 1) for line in self.output.read_text().splitlines())
        exports = dict(line.split("=", 1) for line in self.envfile.read_text().splitlines())
        return outputs, exports, result.stdout

    def test_only_successful_upstream_integration_push_can_upload(self):
        save = self.steps["compiler-save"]
        restore = self.steps["compiler-restore"]
        # Restore-only has no post action; simulate scheduling the real save
        # condition, including failures, cancellation, forks and exact hits.
        self.assertRegex(restore["uses"], r"^actions/cache/restore@[0-9a-f]{40}$")
        self.assertRegex(save["uses"], r"^actions/cache/save@[0-9a-f]{40}$")
        for event in ("push", "pull_request", "pull_request_target", "workflow_dispatch"):
            for repository in ("constellation-works/orbit", "fork/orbit"):
                for ref in ("refs/heads/agent-main", "refs/heads/main", "refs/pull/42/merge"):
                    for success, cancelled, ready, hit in ((True, False, "true", "false"),
                                                          (False, False, "true", "false"),
                                                          (False, True, "true", "false"),
                                                          (True, False, "false", "false"),
                                                          (True, False, "true", "true")):
                        with self.subTest(event=event, repository=repository, ref=ref,
                                          success=success, cancelled=cancelled, ready=ready, hit=hit):
                            allowed = evaluate(save["if"], event, success=success, cancelled=cancelled,
                                               bindings={"github.repository": repository, "github.ref": ref,
                                                         "steps.compiler-setup.outputs.ready": ready,
                                                         "steps.compiler-restore.outputs.cache-hit": hit})
                            self.assertEqual(allowed, event == "push" and repository == "constellation-works/orbit"
                                             and ref == "refs/heads/agent-main" and success and not cancelled
                                             and ready == "true" and hit != "true")
        cache_steps = [s for s in self.job["steps"] if s.get("uses", "").startswith("actions/cache")]
        self.assertEqual(cache_steps, [restore, save], "compiler objects must have no implicit upload action")
        # The test-build job compiles with the same cache contract.
        for name, job in self.document["jobs"].items():
            steps = {step["id"]: step for step in job["steps"] if step.get("id", "").startswith("compiler-")}
            if steps:
                with self.subTest(job=name):
                    self.assertEqual(steps, {key: value for key, value in self.steps.items()
                                             if key.startswith("compiler-")},
                                     "every job's compiler-object steps must match the Check job's")
                    self.assertEqual(job["env"]["SCCACHE_CACHE_SIZE"], self.job["env"]["SCCACHE_CACHE_SIZE"])
        self.assertEqual(restore["with"]["path"], "${{ env.SCCACHE_DIR }}")
        self.assertEqual(save["with"]["path"], restore["with"]["path"])
        self.assertEqual(save["with"]["key"], restore["with"]["key"])
        self.assertEqual(restore["with"]["restore-keys"], "${{ steps.compiler-key.outputs.prefix }}")
        # Dependency cache remains its own archive; object restores never
        # include source files, Cargo fingerprints, mtimes or executables.
        dependency = self.steps["rust-cache"]
        self.assertNotEqual(dependency["with"]["shared-key"], restore["with"]["key"])
        self.assertRegex(str(Path(self.env["SCCACHE_DIR"]).relative_to(self.env["RUNNER_TEMP"])),
                         r"^orbit-ci-compiler-v\d+$",
                         "compiler objects must stay in a versioned runner-local archive apart from Cargo target state")

    def test_compatibility_identity_invalidates_inputs_but_not_source_content(self):
        for name in ("rustc", "cargo", "clippy-driver"):
            self.write("bin/" + name, '#!/bin/sh\nprintf "%s\\n" "${FIXTURE_COMPILER:-compiler-v1}"\n', executable=True)
        for name in ("Cargo.toml", "crates/member/Cargo.toml", "Cargo.lock", ".cargo/config.toml",
                     "rust-toolchain.toml", "scripts/compiler-cache.sh", "scripts/rustc-compiler-cache.sh",
                     "scripts/ci-guardrails.sh", ".github/workflows/ci.yml", "src/lib.rs"):
            self.write(name, "original\n")
        self_test_hang.run(["git", "init", "-q", str(self.root)], step="git init",
                           check=True, capture_output=True)
        self_test_hang.run(["git", "add", "."], step="git add", cwd=self.root,
                           check=True, capture_output=True)
        step = self.steps["compiler-key"]
        baseline, _, _ = self.run_step(step)
        self.assertTrue(baseline["key"].startswith(baseline["prefix"]))
        self.assertEqual(self.run_step(step)[0]["prefix"], baseline["prefix"])
        for name in ("Cargo.toml", "crates/member/Cargo.toml", "Cargo.lock", ".cargo/config.toml",
                     "rust-toolchain.toml", "scripts/compiler-cache.sh", "scripts/rustc-compiler-cache.sh",
                     "scripts/ci-guardrails.sh", ".github/workflows/ci.yml"):
            with self.subTest(file=name):
                path = self.write(name, "changed profile/features/configuration/command\n")
                changed = self.run_step(step)[0]
                self.assertNotEqual(changed["prefix"], baseline["prefix"])
                path.write_text("original\n")
        # GITHUB_JOB: the Check and test-build jobs compile different units,
        # so each saves its own archive instead of racing for one key.
        for name in ("FIXTURE_COMPILER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_TEST_DEBUG",
                     "CARGO_BUILD_TARGET", "CARGO_FEATURE_EXPERIMENT", "ImageVersion", "GITHUB_JOB"):
            with self.subTest(env=name):
                self.env[name] = "different"
                self.assertNotEqual(self.run_step(step)[0]["prefix"], baseline["prefix"])
                del self.env[name]
        self.write("home/.cargo/config.toml", "[build]\nrustflags=['--cfg', 'changed']\n")
        self.assertNotEqual(self.run_step(step)[0]["prefix"], baseline["prefix"])
        self.write("home/.cargo/config.toml", "")
        with_home_config = self.run_step(step)[0]
        self.write("src/lib.rs", "changed source\n")
        self.assertEqual(self.run_step(step)[0]["prefix"], with_home_config["prefix"],
                         "source reuse relies on compiler content keys, never archive fingerprints")

    def test_setup_enables_verified_install_or_falls_back_without_upload(self):
        # Execute setup against an installer transport fixture. Existing public
        # wrapper tests separately execute rustc fallback and argv preservation.
        self.write("scripts/compiler-cache.sh", '''#!/bin/sh
set -eu
[ "$*" = 'setup --install' ]
exit "${FIXTURE_INSTALL_EXIT:-0}"
''', executable=True)
        self.write("home/.orbit/cache/bin/sccache", '''#!/bin/sh
set -eu
[ "$SCCACHE_SERVER_UDS" = /tmp/orbit-sccache.sock ]
[ "$SCCACHE_CACHE_SIZE" = "$FIXTURE_EXPECT_CAPACITY" ]
case "$1" in --show-stats|--zero-stats) exit "${FIXTURE_STATS_EXIT:-0}";; *) exit 9;; esac
''', executable=True)
        step = self.steps["compiler-setup"]
        self.env["FIXTURE_EXPECT_CAPACITY"] = self.job["env"]["SCCACHE_CACHE_SIZE"]
        for install_exit, stats_exit, expected in (("0", "0", "true"), ("1", "0", "false"), ("0", "1", "false")):
            with self.subTest(install_exit=install_exit, stats_exit=stats_exit):
                self.env.update(FIXTURE_INSTALL_EXIT=install_exit, FIXTURE_STATS_EXIT=stats_exit)
                outputs, exports, log = self.run_step(step)
                self.assertEqual(outputs["ready"], expected)
                self.assertEqual(exports.get("ORBIT_COMPILER_CACHE"), "0" if expected == "false" else None)
                self.assertRegex(log, r"compiler_cache_restore_seconds=\d+")
                self.assertRegex(log, r"compiler_cache_setup_seconds=\d+")
                self.assertEqual(exports["ORBIT_COMPILER_CACHE_BIN"], str(self.root / "home/.orbit/cache/bin/sccache"))
        # A restore service failure is just a cold writable cache; an unusable
        # local restore must instead force ordinary rustc and disable upload.
        self.env.update(FIXTURE_INSTALL_EXIT="0", FIXTURE_STATS_EXIT="0", RESTORE_OUTCOME="failure")
        self.assertEqual(self.run_step(step)[0]["ready"], "true")
        self.env["SCCACHE_DIR"] = str(self.write("unusable-cache", "not a directory"))
        outputs, exports, _ = self.run_step(step)
        self.assertEqual(outputs["ready"], "false")
        self.assertEqual(exports["ORBIT_COMPILER_CACHE"], "0")

    def test_slow_step_past_the_removed_cap_completes(self):
        # Sleep stands in for a correct step that loses the CPU under idle
        # scheduling. 25s is past the removed 10s and 20s caps.
        step = {"id": "slow-compiler-step", "run": f"sleep {self_test_hang.SLOW_FIXTURE_SECONDS}\n"}
        started = time.monotonic()
        outputs, _, _ = self.run_step(step)
        self.assertEqual(outputs, {})
        self.assertGreater(time.monotonic() - started, 20)

    def test_hung_step_fails_and_names_the_step(self):
        step = {"id": "hung-compiler-step", "run": "sleep 30\n"}
        with self.assertRaises(AssertionError) as caught:
            self.run_step(step, timeout=self_test_hang.HANG_PROBE_SECONDS)
        self.assertIn(
            "hung step: hung-compiler-step "
            f"(command bash exceeded {self_test_hang.HANG_PROBE_SECONDS:g}s)",
            str(caught.exception))

    def test_timeout_reaps_live_leader_and_exited_leader_descendant(self):
        # Public run() boundary used by the workflow fixtures. The leader must
        # not outlive the hang error, and neither may a child that inherited
        # the output pipes and stayed in the leader's process group.
        if not Path("/proc/self/stat").exists():
            self.skipTest("process liveness is observed through /proc")
        limit = self_test_hang.HANG_PROBE_SECONDS
        exited = textwrap.dedent("""\
            import os, sys, time
            record = sys.argv[1]
            read_fd, write_fd = os.pipe()
            child = os.fork()
            if child == 0:
                os.close(read_fd)
                os.write(write_fd, b"ready")
                os.close(write_fd)
                time.sleep(30)
                os._exit(0)
            os.close(write_fd)
            if os.read(read_fd, 5) != b"ready":
                os._exit(2)
            os.close(read_fd)
            with open(record, "w", encoding="utf-8") as handle:
                handle.write(f"{os.getpid()}\\n{child}\\n")
                handle.flush()
                os.fsync(handle.fileno())
            os._exit(0)
            """)
        live = textwrap.dedent("""\
            import os, sys, time
            with open(sys.argv[1], "w", encoding="utf-8") as handle:
                handle.write(f"{os.getpid()}\\n")
                handle.flush()
                os.fsync(handle.fileno())
            time.sleep(30)
            """)
        for step, script, count in (
                ("live-leader", live, 1),
                ("exited-leader-with-descendant", exited, 2)):
            with self.subTest(step=step):
                record = self.root / f"{step}.pids"
                marker = str(record)
                self.addCleanup(_stop_owned_processes, record, marker)
                try:
                    with self.assertRaises(AssertionError) as caught:
                        self_test_hang.run(
                            [sys.executable, "-c", script, marker],
                            step=step, cwd=self.root, capture_output=True, text=True,
                            timeout=limit)
                    self.assertIn(
                        f"hung step: {step} (command {sys.executable} exceeded {limit:g}s)",
                        str(caught.exception))
                    pids = _recorded_pids(record)
                    self.assertEqual(len(pids), count, "the fixture must record every process")
                    running = [f"{pid}:{_owned_state(pid, marker)}" for pid in pids
                               if _owned_state(pid, marker) in {"R", "S", "D", "T"}]
                    self.assertEqual(running, [], "run() must terminate fixture processes before returning")
                finally:
                    _stop_owned_processes(record, marker)


def _recorded_pids(path):
    if not path.exists():
        return []
    return [int(line) for line in path.read_text().split() if line.strip()]


def _owned_state(pid, marker):
    """Return the /proc state when pid is still this fixture, else None."""
    try:
        command = Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\x00", b" ")
        stat = Path(f"/proc/{pid}/stat").read_text()
    except (FileNotFoundError, ProcessLookupError):
        # A process that exits between open and read fails with ESRCH.
        return None
    if marker.encode() not in command:
        return None
    return stat.rsplit(")", 1)[1].split(None, 1)[0]


def _stop_owned_processes(path, marker):
    for pid in _recorded_pids(path):
        if _owned_state(pid, marker) is None:
            continue
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


@unittest.skipIf(yaml is None, "PyYAML not installed; the workflow guard skips locally without it")
class NextestShardWorkflowTests(unittest.TestCase):
    """Run the shard steps with stub tools: the shard jobs and the Check job's wait agree."""

    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="nextest-shards-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.document = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        self.calls = self.root / "calls"
        # Each stub records its argv, one JSON list per line. It names the real
        # interpreter: the python3 stub shadows it on PATH.
        record = f"#!{sys.executable}\nimport json, sys\nopen({str(self.calls)!r}, 'a').write(json.dumps(sys.argv[1:]) + '\\n')\n"
        for path in (self.root / "bin/python3", self.root / "scripts/cpu-utilisation.sh"):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(record)
            path.chmod(0o755)

    def run_step(self, job, name):
        step = next(step for step in job["steps"] if step.get("name") == name)
        self.calls.write_text("")
        env = dict(PATH=f"{self.root / 'bin'}:{os.environ['PATH']}", RUNNER_TEMP=str(self.root),
                   **self.document["env"], **job.get("env", {}))
        result = self_test_hang.run(
            ["bash", "-e", "-o", "pipefail", "-c", step["run"]],
            step=name, cwd=self.root,
            env={key: str(value) for key, value in env.items()},
            text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def test_shard_jobs_partition_once_each_and_the_check_job_awaits_all_of_them(self):
        shards = int(self.document["env"]["NEXTEST_SHARDS"])
        jobs = self.document["jobs"]
        shard_jobs = {job_id: job for job_id, job in jobs.items() if job_id.startswith("test-")
                      and job_id != "test-build"}
        self.assertGreater(shards, 1)
        # The shards start on the build's upload step; the partition guard
        # follows it. A renamed upload step would leave them waiting for the
        # whole build job.
        build_steps = [step.get("name") for step in jobs["test-build"]["steps"]]
        upload = "Upload nextest archive"
        self.assertIn(upload, build_steps)
        self.assertLess(build_steps.index(upload), build_steps.index("Prove the shards cover every test"))
        partitions = {}
        for job_id, job in shard_jobs.items():
            with self.subTest(job=job_id):
                self.assertNotIn("strategy", job, "Orbit's rerun certification needs literal shard jobs")
                self.assertEqual(job["steps"], shard_jobs["test-1"]["steps"], "every shard runs the same steps")
                [nextest] = self.run_step(job, "Run nextest shard")
                self.assertEqual(nextest[:4], ["--label", "nextest", "--", "cargo"])
                self.assertIn("--no-fail-fast", nextest)
                self.assertNotIn("--run-ignored", nextest)
                partition = nextest[nextest.index("--partition") + 1]
                self.assertNotIn(partition, partitions, f"{job_id} repeats {partitions.get(partition)}'s shard")
                partitions[partition] = job["name"]
                [wait] = self.run_step(job, "Await the nextest archive")
                self.assertEqual(wait[1:], ["--job", "Test build", "--step", upload])
        self.assertEqual(sorted(partitions), sorted(f"count:{k}/{shards}" for k in range(1, shards + 1)),
                         "the shard jobs must select every count partition of NEXTEST_SHARDS")
        self.assertEqual(sorted(partitions.values()), sorted(f"Test shard {k}/{shards}" for k in range(1, shards + 1)))
        [wait] = self.run_step(jobs["ci"], "Await the test build and nextest shards")
        self.assertEqual(wait[0], ".orbit/tmp/workflow-ci/scripts/ci-await-jobs.py")
        awaited = wait[2::2]
        self.assertEqual(wait[1::2], ["--job"] * len(awaited))
        self.assertEqual(sorted(awaited), sorted([jobs["test-build"]["name"], *partitions.values()]),
                         "the Check job's elapsed time must cover the test build and every shard")

    def test_an_affected_crates_archive_passes_empty_partitions_and_gates_only_orbit_exec(self):
        jobs = self.document["jobs"]
        for job_id in ("test-1", "test-2", "test-3"):
            with self.subTest(job=job_id):
                [nextest] = self.run_step(jobs[job_id], "Run nextest shard")
                # The full archive's partition guard fails the test build on an
                # empty shard, so only an affected-crates archive relies on this.
                self.assertEqual(nextest[nextest.index("--no-tests") + 1], "pass")
        gate = next(step for step in jobs["test-1"]["steps"]
                    if step.get("name") == "Linux sandbox/policy enforcement gate")
        for shard, mode, runs in (("1", "full", True), ("1", "", True), ("2", "full", False),
                                  ("1", "affected:orbit-cli,orbit-web", False),
                                  ("1", "affected:orbit-cli,orbit-core,orbit-policy,orbit-tools", False),
                                  ("1", "affected:orbit-cli,orbit-exec,orbit-tools", True)):
            with self.subTest(shard=shard, mode=mode):
                self.assertEqual(evaluate(gate["if"], "pull_request", bindings={SCOPE: mode, "env.SHARD": shard}),
                                 runs, "the sandbox gate runs on the full archive and whenever orbit-exec changed")


@unittest.skipIf(yaml is None, "PyYAML not installed; the workflow guard skips locally without it")
class ChangeScopeTests(unittest.TestCase):
    """Run scripts/ci-change-scope.sh against a stub `gh` serving recorded API
    answers and a stub `cargo` reporting the recorded crate graph
    (scripts/fixtures/ci-crate-graph.json), and check the ci.yml jobs each
    resulting mode admits."""

    SHA = "a" * 40
    HEAD = "b" * 40
    BEFORE = "c" * 40
    SUITE = 900

    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="change-scope-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.routes = self.root / "routes.json"
        self.output = self.root / "output"
        gh = self.root / "bin/gh"
        gh.parent.mkdir()
        # The stub answers `gh api [--paginate] <path>` from the routes file;
        # an unrouted path fails like an API error.
        gh.write_text(f"""#!{sys.executable}
import json, sys
args = [arg for arg in sys.argv[1:] if arg not in ("api", "--paginate")]
routes = json.load(open({str(self.routes)!r}))
if args and args[0] in routes:
    print(json.dumps(routes[args[0]]))
else:
    sys.exit(1)
""")
        gh.chmod(0o755)
        # The scope job reads the merge ref's Cargo metadata. The stub builds
        # it from the recorded graph, rooted at this checkout as Cargo would be.
        cargo = self.root / "bin/cargo"
        cargo.write_text(f"""#!{sys.executable}
import json, os, sys
assert sys.argv[1] == "metadata", sys.argv
graph = json.load(open(os.environ["FIXTURE_CRATE_GRAPH"]))["crates"]
crate = lambda name: os.path.join({str(ROOT)!r}, "crates", name)
print(json.dumps(dict(workspace_members=sorted(graph), packages=[
    dict(id=name, name=name, manifest_path=os.path.join(crate(name), "Cargo.toml"), targets=[],
         dependencies=[dict(name=dependency, path=crate(dependency)) for dependency in dependencies])
    for name, dependencies in graph.items()])))
""")
        cargo.chmod(0o755)
        self.graph = str(ROOT / "scripts/fixtures/ci-crate-graph.json")
        self.assertIsNotNone(shutil.which("jq"), "the scope script requires jq, as on the hosted runner")

    def classify(self, event, routes, *, graph=None, **env):
        self.routes.write_text(json.dumps(routes))
        self.output.write_text("")
        result = self_test_hang.run(
            [str(ROOT / "scripts/ci-change-scope.sh")], step="change-scope", cwd=self.root,
            env=dict(PATH=f"{self.root / 'bin'}:{os.environ['PATH']}", LANG="C", GH_REPO="o/r",
                     EVENT=event, GITHUB_OUTPUT=str(self.output),
                     FIXTURE_CRATE_GRAPH=graph or self.graph, **env),
            text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        [line] = self.output.read_text().splitlines()
        self.last_log = result.stdout
        return line.removeprefix("mode=")

    def pull_request(self, *paths, count=None, graph=None):
        files = [{"filename": path} for path in paths]
        return self.classify("pull_request", {
            "repos/o/r/pulls/7": {"changed_files": len(paths) if count is None else count},
            "repos/o/r/pulls/7/files?per_page=100": files,
        }, PR="7", graph=graph)

    def push(self, *paths, same_tree=False, head_passed=True, before_check="success", head_scope="full"):
        routes = {
            f"repos/o/r/git/commits/{self.SHA}": {"tree": {"sha": "t1"}},
            f"repos/o/r/commits/{self.SHA}/pulls": [
                {"merged_at": "2026-10-18T00:00:00Z", "merge_commit_sha": self.SHA, "head": {"sha": self.HEAD}}],
            f"repos/o/r/git/commits/{self.HEAD}": {"tree": {"sha": "t1" if same_tree else "t2"}},
            f"repos/o/r/commits/{self.HEAD}/check-runs?check_name=Check%20%2F%20Clippy%20%2F%20Test&filter=latest":
                {"check_runs": [{"conclusion": "success" if head_passed else "failure",
                                 "app": {"slug": "github-actions"}, "check_suite": {"id": self.SUITE}}]},
            # The head's Change scope runs: an older full run in another suite,
            # then the passing run's own, whose notice says what it tested.
            f"repos/o/r/commits/{self.HEAD}/check-runs?check_name=Change%20scope&filter=all":
                {"check_runs": [{"id": 41, "app": {"slug": "github-actions"}, "check_suite": {"id": self.SUITE - 1}},
                                {"id": 42, "app": {"slug": "github-actions"}, "check_suite": {"id": self.SUITE}}]},
            "repos/o/r/check-runs/41/annotations": [{"title": "Test selection", "message": "full"}],
            "repos/o/r/check-runs/42/annotations":
                [] if head_scope is None else [{"title": "Test selection", "message": head_scope}],
            f"repos/o/r/compare/{self.BEFORE}...{self.SHA}":
                {"status": "ahead", "files": [{"filename": path} for path in paths]},
        }
        if before_check != "api_error":
            pending = before_check in ("queued", "in_progress")
            routes[f"repos/o/r/commits/{self.BEFORE}/check-runs?check_name=Check%20%2F%20Clippy%20%2F%20Test&filter=latest"] = {
                "check_runs": [] if before_check is None else [{
                    "status": before_check if pending else "completed",
                    "conclusion": None if pending else before_check,
                    "app": {"slug": "github-actions"},
                }],
            }
        return self.classify("push", routes, SHA=self.SHA, BEFORE=self.BEFORE)

    def test_pull_request_scope(self):
        self.assertEqual(self.pull_request("README.md", "docs/guide.md"), "docs")
        # Markdown compiled into a crate or mirrored into the plugin is code,
        # and orbit-cli's tests read the configuration reference page.
        self.assertEqual(self.pull_request("docs/guide.md", "crates/orbit-core/assets/skills/x.md"),
                         "affected:orbit-cli,orbit-cmd,orbit-core,orbit-web")
        self.assertEqual(self.pull_request("plugin/skills/x.md"), "full")
        self.assertEqual(self.pull_request("website/src/content/docs/reference/config.md"), "affected:orbit-cli")
        # A listing that disagrees with the reported count is not trusted.
        self.assertEqual(self.pull_request("docs/guide.md", count=2), "full")
        self.assertEqual(self.classify("pull_request", {}, PR="7"), "full", "an API error runs the full gate")

    def test_pull_request_test_selection(self):
        selections = {
            "leaf crate": (["crates/orbit-cli/src/main.rs", "docs/guide.md"], "affected:orbit-cli"),
            "mid-graph crate": (["crates/orbit-mcp/src/lib.rs"], "affected:orbit-cli,orbit-cmd,orbit-mcp,orbit-web"),
            "core crate": (["crates/orbit-types/src/lib.rs"], "full"),
            "workspace-level file": (["crates/orbit-cli/src/main.rs", "Cargo.lock"], "full"),
            "toolchain file": (["crates/orbit-cli/src/main.rs", "rust-toolchain.toml"], "full"),
            "member manifest": (["crates/orbit-cli/Cargo.toml"], "full"),
            "non-crate path": (["crates/orbit-cli/src/main.rs", "scripts/ci-guardrails.sh"], "full"),
            "workflow": (["crates/orbit-cli/src/main.rs", ".github/workflows/ci.yml"], "full"),
            "path no crate owns": (["crates/orbit-retired/src/lib.rs"], "full"),
            # Read across crates by tests the dependency graph cannot see.
            "cross-crate fixture": (["crates/orbit-web/assets/dashboard/vendor/marked.umd.js"],
                                    "affected:orbit-cli,orbit-cmd,orbit-mcp,orbit-web"),
            "cross-crate asset": (["crates/orbit-core/assets/activities/task_pilot.yaml"],
                                  "affected:orbit-cli,orbit-cmd,orbit-core,orbit-engine,orbit-web"),
        }
        for case, (paths, mode) in selections.items():
            with self.subTest(case=case):
                self.assertEqual(self.pull_request(*paths), mode)
                full_notice = "::notice title=Test selection::full" in self.last_log
                self.assertEqual(full_notice, mode == "full",
                                 "only a run that tests every crate may let a push skip its tree")
        self.assertEqual(self.pull_request("crates/orbit-cli/src/main.rs", graph=str(self.root / "missing.json")),
                         "full", "a selection error runs the full suite")
        self.assertIn("::notice title=Test selection::full", self.last_log)

    def test_push_scope(self):
        self.assertEqual(self.push("crates/orbit-core/src/lib.rs", same_tree=True), "skip")
        self.assertEqual(self.push("crates/orbit-core/src/lib.rs"), "full",
                         "a merged tree the PR never tested runs the full gate")
        self.assertEqual(self.push("crates/orbit-core/src/lib.rs", same_tree=True, head_passed=False), "full")
        # The head passed on its affected crates only, or its run left no notice.
        for head_scope in ("affected:orbit-cli", None):
            with self.subTest(head_scope=head_scope):
                self.assertEqual(self.push("crates/orbit-cli/src/main.rs", same_tree=True, head_scope=head_scope),
                                 "full", "a push tests every crate the merged PR did not")
        self.assertEqual(self.push("docs/guide.md"), "docs")
        self.assertEqual(self.classify("push", {}, SHA=self.SHA, BEFORE=self.BEFORE), "full")

    def test_docs_push_after_untested_before_runs_full_gate(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())["jobs"]
        # A pending code push may be replaced before its checks even exist.
        # Incomplete, cancelled and skipped checks cannot establish a tested base.
        for check in (None, "queued", "in_progress", "cancelled", "skipped", "failure", "api_error"):
            with self.subTest(before_check=check):
                mode = self.push("docs/guide.md", before_check=check)
                self.assertEqual(mode, "full", "a Markdown delta cannot hide untested code in BEFORE")
                admitted = {name for name, job in jobs.items() if name != "pr-state" and evaluate(
                    job["if"], "push", dependency_skipped=True, bindings={SCOPE: mode})}
                self.assertEqual(admitted, {"scope", "msrv", "ci", "test-build", "test-1", "test-2", "test-3"})
        self.assertEqual(self.push("docs/guide.md", same_tree=True, before_check="cancelled"), "skip",
                         "an exact tree that passed on the PR head is safe independently of BEFORE")

    def test_modes_admit_the_matching_jobs(self):
        document = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        jobs = document["jobs"]
        expected = {
            "full": {"scope", "msrv", "ci", "test-build", "test-1", "test-2", "test-3"},
            "affected:orbit-cli": {"scope", "msrv", "ci", "test-build", "test-1", "test-2", "test-3"},
            "": {"scope", "msrv", "ci", "test-build", "test-1", "test-2", "test-3"},
            "docs": {"scope", "docs"},
            "skip": {"scope"},
        }
        for mode, admitted in expected.items():
            for event in ("pull_request", "push"):
                with self.subTest(mode=mode, event=event):
                    actual = {name for name, job in jobs.items() if name != "pr-state" and evaluate(
                        job["if"], event, "open", dependency_skipped=event != "pull_request",
                        bindings={SCOPE: mode})}
                    self.assertEqual(actual, admitted)


if __name__ == "__main__":
    unittest.main()
