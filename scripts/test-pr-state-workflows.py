#!/usr/bin/env python3
"""Execute PR-state and compiler-cache workflow controls offline."""

import http.server
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import unittest

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
    return bool(eval(expression.strip(), {"__builtins__": {}}, {}))


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
        result = subprocess.run([step["shell"], "-e", "-o", "pipefail", "-c", script],
                                cwd=self.root, env=env, text=True, capture_output=True, timeout=10)
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

    def assert_admission(self, document, event, state, expected, *, cancelled=False):
        for name, job in document["jobs"].items():
            if name == "pr-state":
                continue
            with self.subTest(job=name, event=event, state=state, cancelled=cancelled):
                needs = job.get("needs", [])
                if isinstance(needs, str):
                    needs = [needs]
                self.assertIn("pr-state", needs, "build jobs must wait for live PR state")
                self.assertEqual(evaluate(job["if"], event, state, cancelled=cancelled,
                                          dependency_skipped=event != "pull_request"), expected)

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
                        SCCACHE_DIR=str(self.root / "objects"), SCCACHE_CACHE_SIZE=self.job["env"]["SCCACHE_CACHE_SIZE"],
                        SCCACHE_CLIENT_SIDE=self.job["env"]["SCCACHE_CLIENT_SIDE"],
                        CARGO_INCREMENTAL=self.document["env"]["CARGO_INCREMENTAL"],
                        COMPILER_CACHE_RESTORE_STARTED="0", RESTORE_OUTCOME="success")

    def write(self, name, text, *, executable=False):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        if executable:
            path.chmod(0o755)
        return path

    def run_step(self, step):
        self.output.write_text("")
        self.envfile.write_text("")
        result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", step["run"]],
                                cwd=self.root, env=self.env, text=True, capture_output=True, timeout=20)
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
        self.assertEqual(restore["with"]["path"], "${{ env.SCCACHE_DIR }}")
        self.assertEqual(save["with"]["path"], restore["with"]["path"])
        self.assertEqual(save["with"]["key"], restore["with"]["key"])
        self.assertEqual(restore["with"]["restore-keys"], "${{ steps.compiler-key.outputs.prefix }}")
        # Dependency cache remains its own archive; object restores never
        # include source files, Cargo fingerprints, mtimes or executables.
        dependency = self.steps["rust-cache"]
        self.assertNotEqual(dependency["with"]["shared-key"], restore["with"]["key"])
        self.assertRegex(self.job["env"]["SCCACHE_DIR"], r"^\$\{\{ runner\.temp \}\}/orbit-ci-compiler-v\d+$",
                         "compiler objects must stay in a versioned runner-local archive apart from Cargo target state")

    def test_compatibility_identity_invalidates_inputs_but_not_source_content(self):
        for name in ("rustc", "cargo", "clippy-driver"):
            self.write("bin/" + name, '#!/bin/sh\nprintf "%s\\n" "${FIXTURE_COMPILER:-compiler-v1}"\n', executable=True)
        for name in ("Cargo.toml", "crates/member/Cargo.toml", "Cargo.lock", ".cargo/config.toml",
                     "rust-toolchain.toml", "scripts/compiler-cache.sh", "scripts/rustc-compiler-cache.sh",
                     "scripts/ci-guardrails.sh", ".github/workflows/ci.yml", "src/lib.rs"):
            self.write(name, "original\n")
        subprocess.run(["git", "init", "-q", str(self.root)], check=True, capture_output=True)
        subprocess.run(["git", "add", "."], cwd=self.root, check=True, capture_output=True)
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
        for name in ("FIXTURE_COMPILER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_TEST_DEBUG",
                     "CARGO_BUILD_TARGET", "CARGO_FEATURE_EXPERIMENT", "ImageVersion"):
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


if __name__ == "__main__":
    unittest.main()
