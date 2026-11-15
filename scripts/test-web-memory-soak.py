#!/usr/bin/env python3
"""CLI edges of the dashboard memory soak that must fail before any measurement."""

import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parent.parent
SOAK = ROOT / "scripts" / "web-memory-soak.py"


def proc_identity(pid: int):
    """Return ``(pgrp, start_time)`` for *pid* from /proc, or ``None``.

    The start time is the STD-03 §R14 start identity that distinguishes a
    live process from a later process reusing the same numeric pid.
    """
    try:
        text = Path(f"/proc/{pid}/stat").read_text()
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return None
    try:
        fields = text.rsplit(")", 1)[1].split()
        return int(fields[2]), int(fields[19])
    except (IndexError, ValueError):
        return None


def load_soak():
    spec = importlib.util.spec_from_file_location("web_memory_soak", SOAK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WebMemorySoakCliTests(unittest.TestCase):
    def run_soak(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-B", str(SOAK), "--bin", "/bin/true", *args],
            text=True,
            capture_output=True,
            timeout=30,
        )

    def assert_usage(self, result: subprocess.CompletedProcess[str], needle: str) -> None:
        combined = result.stdout + result.stderr
        self.assertEqual(result.returncode, 2, combined)
        self.assertIn("usage:", combined)
        self.assertIn(needle, combined)
        self.assertNotIn("Traceback", combined)
        self.assertNotIn("ValueError", combined)

    def test_rounds_below_3_are_a_usage_error(self):
        for rounds in ("1", "2"):
            with self.subTest(rounds=rounds):
                result = self.run_soak("--rounds", rounds)
                self.assert_usage(result, "at least 3")

    def test_rounds_3_passes_the_parser(self):
        # A bad --server-env stops argparse, so round 3 never starts a soak.
        result = self.run_soak("--rounds", "3", "--server-env", "NOEQUALS")
        self.assert_usage(result, "KEY=VALUE")
        self.assertNotIn("at least 3", result.stdout + result.stderr)

    def test_server_env_without_equals_is_a_usage_error(self):
        result = self.run_soak("--server-env", "MALLOC_ARENA_MAX")
        self.assert_usage(result, "KEY=VALUE")

    def test_server_env_binding_is_accepted_when_it_has_equals(self):
        # argparse converts in argument order: validate the binding before the
        # deliberately bad rounds value stops execution without a fixture.
        for binding in ("MALLOC_ARENA_MAX=2", "EMPTY=", "VALUE=a=b"):
            with self.subTest(binding=binding):
                result = self.run_soak("--server-env", binding, "--rounds", "2")
                self.assert_usage(result, "at least 3")
                self.assertNotIn("error: argument --server-env", result.stdout + result.stderr)

    @unittest.skipUnless(sys.platform.startswith("linux"), "soak startup uses Linux /proc")
    def test_unannounced_backend_is_stopped_and_reaped(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        for mode in ("silent", "partial", "invalid", "exited"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(dir=scratch) as tmp:
                work = Path(tmp)
                for name in ("home", "repo", "root"):
                    (work / name).mkdir()
                (work / "fixture.json").write_text(json.dumps({"counts": {}}))
                record = work / "pids.json"
                backend = work / "idle-backend"
                backend.write_text(
                    f"#!{sys.executable}\n"
                    "import json, os, signal\n"
                    "from pathlib import Path\n"
                    "child = os.fork()\n"
                    "if child == 0:\n"
                    "    signal.pause()\n"
                    "    os._exit(0)\n"
                    "def stop(*_):\n"
                    "    os.kill(child, signal.SIGTERM)\n"
                    "    os.waitpid(child, 0)\n"
                    "    os._exit(0)\n"
                    "signal.signal(signal.SIGTERM, stop)\n"
                    "def start_time(pid):\n"
                    "    return int(Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19])\n"
                    f"Path({str(record)!r}).write_text(json.dumps({{'pids': [os.getpid(), child], 'start_times': [start_time(os.getpid()), start_time(child)]}}))\n"
                    + ("os.write(1, b'http://127.0.0.1:')\n" if mode == "partial" else "")
                    + ("os.write(1, b'not an announcement\\n')\n" if mode == "invalid" else "")
                    + ("stop()\n" if mode == "exited" else "signal.pause()\n")
                )
                backend.chmod(0o755)
                process = subprocess.Popen(
                    [sys.executable, "-B", str(SOAK), "--bin", str(backend),
                     "--work-dir", str(work), "--startup-timeout", "0.3"],
                    cwd=work, env={**os.environ, "HOME": str(work / "home")},
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    start_new_session=True,
                )
                try:
                    stdout, stderr = process.communicate(timeout=10)
                    self.assertTrue(record.exists(), stdout + stderr)
                    pids = json.loads(record.read_text())["pids"]
                    self.assertEqual(process.returncode, 1, stdout + stderr)
                    self.assertIn("did not announce its address", stderr)
                    if mode in ("silent", "partial"):
                        self.assertIn("timed out", stderr)
                    self.assertNotIn("Traceback", stderr)
                    for pid in pids:
                        self.assertFalse(Path(f"/proc/{pid}").exists(), f"backend PID {pid} not reaped")
                    self.assertNotIn("Soak:", stdout)
                finally:
                    # A failing candidate must not leak either the harness or
                    # its backend (which owns a separate process group). The
                    # backend group is signalled only while its recorded
                    # start identity still matches: after the pids above are
                    # verified absent (or the id was reused) there is nothing
                    # owned left to signal (STD-03 §R14).
                    if record.exists():
                        try:
                            data = json.loads(record.read_text())
                            leader = data["pids"][0]
                            identity = proc_identity(leader)
                            if identity is not None and identity == (leader, data["start_times"][0]):
                                os.killpg(leader, signal.SIGTERM)
                        except (ProcessLookupError, KeyError, TypeError, ValueError):
                            pass
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGKILL)
                    process.communicate(timeout=5)


class _RecordingStdout:
    def __init__(self, events):
        self.events = events

    def close(self):
        self.events.append("stdout-close")


class _FakeProcess:
    """Leader whose exit (returncode set) is decoupled from reaping.

    ``wait``/``poll`` are the reaping operations, as on ``Popen``; a
    recording ``killpg`` sets the exit status on SIGTERM without reaping,
    mirroring a leader that died but is still an unreaped zombie.
    """

    def __init__(self, events, pid=987_654, stdout=None):
        self.events = events
        self.pid = pid
        self.returncode = None
        self.reaped = False
        self.stdout = stdout if stdout is not None else _RecordingStdout(events)

    def wait(self, timeout=None):
        self.events.append("wait")
        self.reaped = True
        if self.returncode is None:
            self.returncode = 0
        return self.returncode

    def poll(self):
        self.events.append("poll")
        self.reaped = True
        if self.returncode is None:
            self.returncode = 0
        return self.returncode


class ServerOwnershipTests(unittest.TestCase):
    """Deterministic signal/reap ordering through the Server boundary.

    No real process is signalled and no pid churn or load is used: a fake
    leader records whether it had been reaped at each group signal.
    """

    def make_server(self, soak, fake):
        server = object.__new__(soak.Server)
        server.process = fake
        server._stopped = False
        server._leader_wait_unavailable = False
        return server

    def recording_killpg(self, fake, events):
        def killpg(pgid, sig):
            events.append((sig, fake.reaped))
            if sig == signal.SIGTERM and fake.returncode is None:
                fake.returncode = 0

        return killpg

    def test_stop_signals_group_before_reaping_leader(self):
        soak = load_soak()
        events = []
        fake = _FakeProcess(events)
        server = self.make_server(soak, fake)
        with mock.patch("os.killpg", self.recording_killpg(fake, events)):
            server.stop()
            server.stop()
        signals = [entry for entry in events if isinstance(entry, tuple)]
        self.assertEqual(
            signals,
            [(signal.SIGTERM, False), (signal.SIGKILL, False)],
            f"every group signal must precede reaping: {events}",
        )
        self.assertNotIn("poll", events)
        self.assertEqual(events.count("wait"), 1, events)
        self.assertLess(events.index("wait"), events.index("stdout-close"))
        self.assertTrue(fake.reaped)

    @unittest.skipUnless(sys.platform.startswith("linux"), "announcement uses /proc-era waitid")
    def test_announcement_does_not_reap_before_constructor_cleanup(self):
        soak = load_soak()
        events = []
        read_fd, write_fd = os.pipe()
        fake = _FakeProcess(events, stdout=os.fdopen(read_fd, "rb"))
        fake.returncode = 7
        server = self.make_server(soak, fake)
        try:
            with self.assertRaisesRegex(RuntimeError, "exited 7"):
                server.announcement(timeout=5)
            self.assertFalse(fake.reaped, events)
            self.assertNotIn("poll", events)
            self.assertNotIn("wait", events)
            with mock.patch("os.killpg", self.recording_killpg(fake, events)):
                server.stop()
        finally:
            os.close(write_fd)
            if not fake.reaped:
                try:
                    fake.stdout.close()
                except Exception:
                    pass
        signals = [entry for entry in events if isinstance(entry, tuple)]
        self.assertEqual(
            signals,
            [(signal.SIGTERM, False), (signal.SIGKILL, False)],
            f"constructor cleanup must signal before reaping: {events}",
        )


if __name__ == "__main__":
    unittest.main()
