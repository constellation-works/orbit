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

# A process's stable start identity: the kernel start time, which a reused PID
# never shares with the process that held the number before. Linux reads
# /proc; elsewhere `ps` reports the start time. None when it cannot be read.
_IDENTITY_SOURCE = r'''
def process_identity(pid):
    import subprocess
    try:
        with open(f"/proc/{pid}/stat", "rb") as handle:
            return "proc:" + handle.read().decode(errors="replace").rsplit(")", 1)[1].split()[19]
    except (OSError, IndexError):
        pass
    try:
        done = subprocess.run(["ps", "-o", "lstart=", "-p", str(pid)],
                              capture_output=True, text=True, timeout=5, check=False)
    except (OSError, subprocess.SubprocessError):
        return None
    started = done.stdout.strip()
    return "ps:" + started if done.returncode == 0 and started else None
'''
exec(_IDENTITY_SOURCE)

# A `web serve` fixture that mimics the npm `orbit` shim: it spawns the real
# server as a child and waits without forwarding signals. Terminating only
# the shim orphans the grandchild server. It records its PID together with
# its start identity so a later check never trusts the bare number.
_DASHBOARD_SERVER = _IDENTITY_SOURCE + r'''
import http.server, os, pathlib, sys
pathlib.Path(sys.argv[2]).write_text(f"{os.getpid()} {process_identity(os.getpid()) or ''}")
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

# Execute the real checker and EXIT cleanup with deterministic kernel/process
# substitutes in a child. No real PID reuse, group signals or timing load.
LIFECYCLE_PROBE = r'''
import contextlib, io, os, pathlib, signal, socket, subprocess, sys, time, types, urllib.request
from unittest import mock
script, scratch, scenario = sys.argv[1:]
source = pathlib.Path(script).read_text()
checker = source.split("<<'PYWEB'\n", 1)[1].split("\nPYWEB", 1)[0]
cleanup = source.split('python3 - "$scratch" <<\'PY\'\n', 1)[1].split("\nPY", 1)[0]
events = []
reaped = False
clock = 0
handlers = {}
class Process:
    pid = 123456789
    def poll(self):
        global reaped
        if scenario == 'early_exit':
            reaped = True
            events.append('reaped')
            return 1
        return None
    def wait(self, timeout=None):
        global reaped
        assert not reaped, events
        assert timeout == 10, timeout
        reaped = True
        events.append('reaped')
        return 1 if scenario == 'early_exit' else -signal.SIGTERM
def killpg(pid, sig):
    assert not reaped, f'signal {sig} after reap: {events}'
    assert pid == Process.pid
    events.append(('signal', sig))
    # The stubborn descendant keeps the group alive through the grace period.
def waitid(*args):
    assert not reaped
    return types.SimpleNamespace(si_pid=Process.pid) if scenario == 'early_exit' else None
def sleep(seconds):
    global clock
    clock += seconds
def connect(*args, **kwargs):
    if scenario == 'interrupt':
        handlers[signal.SIGTERM](signal.SIGTERM, None)
    if scenario == 'failure':
        raise OSError('injected connection failure')
    return contextlib.nullcontext()
def install(sig, handler):
    previous = handlers.get(sig, signal.SIG_DFL)
    handlers[sig] = handler
    return previous
with mock.patch.object(subprocess, 'Popen', return_value=Process()), \
     mock.patch.object(os, 'killpg', side_effect=killpg), \
     mock.patch.object(os, 'waitid', side_effect=waitid, create=True), \
     mock.patch.object(os, 'P_PID', 1, create=True), \
     mock.patch.object(os, 'WNOWAIT', 0x1000000, create=True), \
     mock.patch.object(time, 'sleep', side_effect=sleep), \
     mock.patch.object(time, 'monotonic', side_effect=lambda: clock), \
     mock.patch.object(signal, 'signal', side_effect=install), \
     mock.patch.object(socket, 'create_connection', side_effect=connect), \
     mock.patch.object(urllib.request, 'urlopen', return_value=contextlib.nullcontext(types.SimpleNamespace(status=200))):
    sys.argv = ['checker', str(pathlib.Path(scratch) / 'repo'), 'false', '']
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            exec(compile(checker, script + ':PYWEB', 'exec'), {})
    except (RuntimeError, SystemExit):
        assert scenario != 'normal'
    else:
        assert scenario == 'normal', f'{scenario} did not fail'
    assert reaped, events
    pathlib.Path(scratch, 'dashboard.pgid').write_text(str(Process.pid))
    sys.argv = ['cleanup', scratch]
    exec(compile(cleanup, script + ':cleanup', 'exec'), {})
    signals = [event[1] for event in events if isinstance(event, tuple)]
    assert signal.SIGTERM in signals and signal.SIGKILL in signals, events
    assert clock <= (13 if scenario == 'failure' else 3), clock
print(events)
'''


def provider(detail, ready=False):
    return {"name": "codex", "sandbox": "linux-bwrap", "sandbox_ready": ready,
            "sandbox_readiness_detail": detail}


class NestedSandboxRegression(unittest.TestCase):
    def test_dashboard_owner_sweeps_before_reap_on_every_exit(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        for scenario in ("normal", "early_exit", "failure", "interrupt"):
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
                scratch = Path(tmp) / "smoke"
                (scratch / "repo").mkdir(parents=True)
                result = subprocess.run(
                    [os.sys.executable, "-c", LIFECYCLE_PROBE, str(SCRIPT), str(scratch), scenario],
                    capture_output=True, text=True, timeout=20, check=False)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def run_smoke(self, rows, orbit_fixture=ORBIT_FIXTURE, extra_env=None):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="wsl2-regression-", dir=SCRATCH) as tmp:
            root = Path(tmp)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            # Keep the script and command fixtures on the interpreter running
            # this suite (the macOS system Python can lack waitid/WNOWAIT).
            (bin_dir / "python3").symlink_to(os.sys.executable)
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
            server_pid, identity = _read_fixture_process(pidfile)
            self.assertTrue(identity, "fixture server recorded no start identity")
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and _fixture_process_state(server_pid, identity) != "exited":
                    time.sleep(0.05)
                self.assertEqual(
                    _fixture_process_state(server_pid, identity), "exited",
                    f"dashboard server pid {server_pid} survived the smoke exit",
                )
            finally:
                _cleanup_fixture_process(server_pid, identity)

    def test_reused_pid_is_the_original_process_having_exited(self):
        killed = []
        state = _fixture_process_state(
            4242, "proc:100", alive=lambda pid: True, identity_of=lambda pid: "proc:999")
        self.assertEqual(state, "exited")
        self.assertFalse(_cleanup_fixture_process(
            4242, "proc:100", alive=lambda pid: True, identity_of=lambda pid: "proc:999",
            kill=lambda pid, sig: killed.append((pid, sig))))
        self.assertEqual(killed, [], "a reused PID must receive no signal")

    def test_unreadable_identity_fails_verification_and_grants_no_cleanup(self):
        for recorded, current in (("proc:100", None), (None, "proc:100"), ("", "")):
            with self.subTest(recorded=recorded, current=current):
                killed = []
                state = _fixture_process_state(
                    4242, recorded, alive=lambda pid: True, identity_of=lambda pid: current)
                self.assertEqual(state, "unverifiable")
                self.assertFalse(_cleanup_fixture_process(
                    4242, recorded, alive=lambda pid: True, identity_of=lambda pid: current,
                    kill=lambda pid, sig: killed.append((pid, sig))))
                self.assertEqual(killed, [], "an unproven identity must receive no signal")

    def test_matching_live_identity_is_killed_and_dead_process_is_exited(self):
        killed = []
        self.assertEqual(_fixture_process_state(
            4242, "proc:100", alive=lambda pid: True, identity_of=lambda pid: "proc:100"), "alive")
        self.assertTrue(_cleanup_fixture_process(
            4242, "proc:100", alive=lambda pid: True, identity_of=lambda pid: "proc:100",
            kill=lambda pid, sig: killed.append((pid, sig))))
        self.assertEqual(killed, [(4242, 9)])
        self.assertEqual(_fixture_process_state(
            4242, "proc:100", alive=lambda pid: False, identity_of=lambda pid: None), "exited")


def _read_fixture_process(pidfile):
    """The PID and start identity the fixture server recorded about itself."""
    pid, _, identity = Path(pidfile).read_text().strip().partition(" ")
    return int(pid), identity or None


def _fixture_process_state(pid, recorded, *, alive=None, identity_of=None):
    """`exited`, `alive` or `unverifiable` for the process a PID file recorded.

    A live occupant counts only when its start identity equals the recorded one.
    A different identity is a reused PID, so the original process has exited. A
    live occupant whose identity, or the recorded one, cannot be read proves
    nothing and is `unverifiable`: verification fails and nothing may signal it."""
    alive = _process_alive if alive is None else alive
    identity_of = _process_identity if identity_of is None else identity_of
    if not alive(pid):
        return "exited"
    current = identity_of(pid)
    if not recorded or not current:
        return "unverifiable"
    return "alive" if current == recorded else "exited"


def _cleanup_fixture_process(pid, recorded, *, alive=None, identity_of=None, kill=os.kill):
    """SIGKILL the fixture server only while its recorded identity is live."""
    if _fixture_process_state(pid, recorded, alive=alive, identity_of=identity_of) != "alive":
        return False
    try:
        kill(pid, 9)
    except ProcessLookupError:
        return False
    return True


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


_process_identity = process_identity


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
