#!/usr/bin/env python3
"""CLI edges of the dashboard memory soak that must fail before any measurement."""

import json
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SOAK = ROOT / "scripts" / "web-memory-soak.py"


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
                    f"Path({str(record)!r}).write_text(json.dumps([os.getpid(), child]))\n"
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
                    pids = json.loads(record.read_text())
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
                    # its backend (which owns a separate process group).
                    if record.exists():
                        try:
                            os.killpg(json.loads(record.read_text())[0], signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGKILL)
                    process.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
