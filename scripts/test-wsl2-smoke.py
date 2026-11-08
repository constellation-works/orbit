#!/usr/bin/env python3
"""Regress the observed nested-Bubblewrap smoke failure at the script boundary.

Fixtures replace package installation and Orbit commands; Git and the script's
HTTP client still run. No provider, live Orbit state or network install is used.
"""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest


SCRIPT = Path(__file__).with_name("wsl2-smoke.sh").resolve()
SCRATCH = SCRIPT.parent.parent / ".orbit" / "tmp"
NAMESPACE_DENIED = (
    "Bubblewrap capability probe failed: bwrap: No permissions to create a new "
    "namespace, likely because the kernel does not allow non-privileged user namespaces."
)

ORBIT_FIXTURE = r'''#!/usr/bin/env python3
import http.server, json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["SMOKE_FIXTURE_CALLS"], "a") as log:
    log.write(json.dumps(args) + "\n")
if args[:2] == ["doctor", "providers"]:
    print(pathlib.Path(os.environ["SMOKE_FIXTURE_ROWS"]).read_text())
elif args[:2] == ["workspace", "init"]:
    pathlib.Path(".gitignore").write_text(".orbit/\n")
elif args[:2] == ["web", "serve"]:
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"smoke fixture")
        def log_message(self, *_):
            pass
    http.server.HTTPServer(("127.0.0.1", int(args[args.index("--port") + 1])), Handler).serve_forever()
elif args[:2] == ["task", "add"]:
    sys.exit("independent task failure must not become a namespace skip")
elif args == ["--version"]:
    print("orbit smoke fixture")
elif args[0] not in ("init", "doctor", "config"):
    sys.exit("unexpected fixture command: " + repr(args))
'''

# A `web serve` fixture that mimics the npm `orbit` shim: it spawns the real
# server as a child and waits without forwarding signals. Terminating only
# the shim orphans the grandchild server.
_DASHBOARD_SERVER = r'''
import http.server, os, pathlib, sys
pathlib.Path(sys.argv[2]).write_text(str(os.getpid()))
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"smoke fixture")
    def log_message(self, *_):
        pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
'''

ORBIT_SHIM_FIXTURE = r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
with open(os.environ["SMOKE_FIXTURE_CALLS"], "a") as log:
    log.write(json.dumps(args) + "\n")
if args[:2] == ["doctor", "providers"]:
    print(pathlib.Path(os.environ["SMOKE_FIXTURE_ROWS"]).read_text())
elif args[:2] == ["workspace", "init"]:
    pathlib.Path(".gitignore").write_text(".orbit/\n")
elif args[:2] == ["web", "serve"]:
    child = subprocess.Popen([sys.executable, "-c", SERVER_CODE, args[args.index("--port") + 1], os.environ["SMOKE_DASHBOARD_PIDFILE"]])
    child.wait()
elif args == ["--version"]:
    print("orbit smoke fixture")
elif args[0] not in ("init", "doctor", "config"):
    sys.exit("unexpected fixture command: " + repr(args))
'''.replace("SERVER_CODE", repr(_DASHBOARD_SERVER))

NPM_FIXTURE = r'''#!/usr/bin/env python3
import os, pathlib, shutil, sys
if sys.argv[1:] == ["--version"]:
    print("10.0.0")
elif sys.argv[1:] == ["install", "-g", "@orbit-tools/cli"]:
    target = pathlib.Path(os.environ["npm_config_prefix"]) / "bin" / "orbit"
    target.parent.mkdir(parents=True)
    shutil.copy2(os.environ["SMOKE_FIXTURE_ORBIT"], target)
else:
    sys.exit("unexpected npm command")
'''


def provider(detail, ready=False):
    return {"name": "codex", "sandbox": "linux-bwrap", "sandbox_ready": ready,
            "sandbox_readiness_detail": detail}


class NestedSandboxRegression(unittest.TestCase):
    def run_smoke(self, rows, orbit_fixture=ORBIT_FIXTURE, extra_env=None):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="wsl2-regression-", dir=SCRATCH) as tmp:
            root = Path(tmp)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            for name, text in {
                "node": "#!/bin/sh\nexit 0\n",
                "uname": "#!/bin/sh\ncase $1 in -r) echo 6.8.0-fixture ;; -s) echo Linux ;; -m) echo x86_64 ;; esac\n",
                "npm": NPM_FIXTURE,
                "orbit-fixture": orbit_fixture,
            }.items():
                path = bin_dir / name
                path.write_text(text)
                path.chmod(0o755)
            rows_path = root / "providers.json"
            rows_path.write_text(json.dumps(rows))
            calls_path = root / "calls.jsonl"
            env = {
                "PATH": f"{bin_dir}:/usr/bin:/bin", "TMPDIR": tmp, "USER": "smoke",
                # Enter with an already cleared fixture environment so the
                # package fixtures are used instead of a real npm download.
                "ORBIT_SMOKE_CLEAN_ENV": "1", "ORBIT_SMOKE_POWERSHELL": "",
                "SMOKE_FIXTURE_ROWS": str(rows_path),
                "SMOKE_FIXTURE_CALLS": str(calls_path),
                "SMOKE_FIXTURE_ORBIT": str(bin_dir / "orbit-fixture"),
            }
            if extra_env:
                env.update(extra_env)
            completed = subprocess.run(
                ["bash", str(SCRIPT)], env=env, cwd=root, text=True,
                capture_output=True, timeout=30, check=False,
            )
            self.assertTrue(completed.stdout, completed.stderr)
            report = json.loads(completed.stdout)
            calls = [json.loads(line) for line in calls_path.read_text().splitlines()]
            self.assertFalse(list(root.glob("orbit-wsl2-smoke.*")), "smoke scratch must be cleaned")
            return completed.returncode, report, calls

    def test_namespace_denial_skips_sandbox_and_ship(self):
        for detail in (
            NAMESPACE_DENIED,
            "Bubblewrap capability probe failed: bwrap: Creating new namespace failed: Operation not permitted",
        ):
            with self.subTest(detail=detail):
                code, report, calls = self.run_smoke([provider(detail)])
                self.assertEqual(code, 0, report)
                self.assertEqual(report["status"], "pass")
                steps = {row["step"]: row for row in report["steps"]}
                for name in ("sandbox", "ship"):
                    self.assertEqual(steps[name]["status"], "skipped", report)
                    self.assertEqual(steps[name]["reason"], "nested_sandbox")
                    self.assertIn("nested or unprivileged sandbox", steps[name]["detail"])
                self.assertFalse(any(call[:2] == ["task", "add"] for call in calls))
                self.assertEqual(steps["dashboard"]["status"], "pass")

    def test_missing_or_misconfigured_bubblewrap_still_fails(self):
        cases = (
            [provider("trusted Bubblewrap not available at /usr/bin/bwrap")],
            [provider("Bubblewrap does not support the required --bind-fd and --ro-bind-fd")],
            [provider("Bubblewrap capability probe failed: bwrap: setting up uid map: Permission denied")],
            [provider("Bubblewrap capability probe failed: bwrap: Can't bind mount: Permission denied")],
            [provider(NAMESPACE_DENIED), provider("could not execute Bubblewrap: Permission denied")],
            [],
        )
        for rows in cases:
            with self.subTest(rows=rows):
                code, report, _ = self.run_smoke(rows)
                self.assertEqual(code, 1, report)
                self.assertEqual(report["status"], "fail")
                sandbox = next(row for row in report["steps"] if row["step"] == "sandbox")
                self.assertEqual(sandbox["status"], "fail", report)
                self.assertNotIn("reason", sandbox)

    def test_independent_ship_failure_is_not_skipped(self):
        code, report, calls = self.run_smoke([provider("capability probe succeeded", ready=True)])
        self.assertEqual(code, 1, report)
        self.assertEqual(report["status"], "fail")
        ship = next(row for row in report["steps"] if row["step"] == "ship")
        self.assertEqual(ship["status"], "fail", report)
        self.assertNotIn("reason", ship)
        self.assertTrue(any(call[:2] == ["task", "add"] for call in calls))

    def test_dashboard_server_process_does_not_survive_exit(self):
        # The npm-shim fixture spawns the real server as a grandchild and
        # does not forward signals. Reaping only the shim (the old behavior)
        # leaves the grandchild listening after the script exits.
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="wsl2-leftover-", dir=SCRATCH) as tmp:
            pidfile = Path(tmp) / "dashboard.pid"
            code, report, _ = self.run_smoke(
                [provider(NAMESPACE_DENIED)],
                orbit_fixture=ORBIT_SHIM_FIXTURE,
                extra_env={"SMOKE_DASHBOARD_PIDFILE": str(pidfile)},
            )
            self.assertEqual(code, 0, report)
            steps = {row["step"]: row for row in report["steps"]}
            self.assertEqual(steps["dashboard"]["status"], "pass", report)
            server_pid = int(pidfile.read_text().strip())
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and _process_alive(server_pid):
                    time.sleep(0.05)
                self.assertFalse(
                    _process_alive(server_pid),
                    f"dashboard server pid {server_pid} survived the smoke exit",
                )
            finally:
                if _process_alive(server_pid):
                    os.kill(server_pid, 9)


def _process_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:
        state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
    except OSError:
        return True
    return state != "Z"


class StubCodexContract(unittest.TestCase):
    """Drive the generated stub the way the codex cli executor does.

    The executor passes the prompt on stdin as response-contract prose, a
    line ``Execution envelope:``, then the envelope JSON (see
    crates/orbit-agent/src/providers/common.rs). The old stub ran
    ``json.load(sys.stdin)`` and crashed on the leading prose.
    """

    def extract_stub(self, root):
        text = SCRIPT.read_text(encoding="utf-8")
        start = text.index("<<'STUB'\n") + len("<<'STUB'\n")
        end = text.index("\nSTUB\n", start)
        stub = root / "codex"
        stub.write_text(text[start:end] + "\n", encoding="utf-8")
        stub.chmod(0o755)
        return stub

    def executor_stdin(self, envelope):
        prose = (
            "You are Orbit's agent executor.\n"
            "Read the execution envelope JSON and perform the requested work.\n"
            "Return exactly one JSON object and nothing else.\n"
        )
        return prose + "Execution envelope:\n" + json.dumps(envelope) + "\n"

    def run_stub(self, envelope):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="wsl2-stub-", dir=SCRATCH) as tmp:
            root = Path(tmp)
            stub = self.extract_stub(root)
            calls = root / "orbit-calls.jsonl"
            fake_orbit = root / "orbit"
            fake_orbit.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "with open(os.environ['STUB_ORBIT_CALLS'], 'a') as log:\n"
                "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                "print('{}')\n",
                encoding="utf-8",
            )
            fake_orbit.chmod(0o755)
            env = dict(os.environ)
            env["PATH"] = f"{root}:/usr/bin:/bin"
            env["STUB_ORBIT_CALLS"] = str(calls)
            completed = subprocess.run(
                [str(stub), "exec", "--json", "--sandbox", "workspace-write"],
                input=self.executor_stdin(envelope), cwd=root, env=env,
                text=True, capture_output=True, timeout=30, check=False,
            )
            result_file = root / "smoke-result.txt"
            result_text = result_file.read_text(encoding="utf-8") if result_file.exists() else None
            recorded = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            return completed, result_text, recorded

    def test_stub_reads_envelope_after_marker(self):
        envelope = {
            "schemaVersion": 1,
            "instruction": "Complete the Orbit task",
            "prompt": '{"task_id": "WSM-1"}',
            "input": {"task_id": "WSM-1", "workspace_path": "/tmp/smoke"},
            "run_id": "jrun-smoke",
            "tools": [],
            "required_tools": [],
            "model": "codex",
        }
        completed, result_text, recorded = self.run_stub(envelope)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(
            json.loads(completed.stdout),
            {"schemaVersion": 1, "status": "success", "result": {}, "error": None},
        )
        self.assertEqual(result_text, "Smoke implementation\n")
        updates = [c for c in recorded if c[:3] == ["tool", "run", "orbit.task.update"]]
        self.assertEqual(len(updates), 1, recorded)
        payload = json.loads(updates[0][updates[0].index("--input") + 1])
        self.assertEqual(payload["id"], "WSM-1")
        self.assertIn("execution_summary", payload)

    def test_stub_falls_back_to_task_object_id(self):
        envelope = {
            "schemaVersion": 1,
            "instruction": "Complete the Orbit task",
            "prompt": "{}",
            "input": {},
            "run_id": "jrun-smoke",
            "tools": [],
            "required_tools": [],
            "model": "codex",
            "task": {"id": "WSM-2"},
        }
        completed, _, recorded = self.run_stub(envelope)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        updates = [c for c in recorded if c[:3] == ["tool", "run", "orbit.task.update"]]
        payload = json.loads(updates[0][updates[0].index("--input") + 1])
        self.assertEqual(payload["id"], "WSM-2")


if __name__ == "__main__":
    unittest.main()
