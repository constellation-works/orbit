#!/usr/bin/env python3
"""Exercise dashboard fixture setup through the real CLI and its failure boundary."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/visual-fixture-server.sh"
SEED = ROOT / "crates/orbit-web/visual/fixtures/busy-workspace.json"
SCRATCH = ROOT / ".orbit/tmp"


class VisualFixtureServerTest(unittest.TestCase):
    def test_seed_readback_and_runner_protocol(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
            env = {**os.environ, "ORBIT_SCRATCH_DIR": tmp}
            with open(Path(tmp) / "stdout", "w+") as stdout, open(Path(tmp) / "stderr", "w+") as stderr:
                server = subprocess.Popen([str(SCRIPT), "busy-workspace"], cwd=ROOT, env=env,
                                          stdout=stdout, stderr=stderr, start_new_session=True)
                try:
                    deadline = time.monotonic() + 90
                    output = ""
                    while time.monotonic() < deadline:
                        stdout.seek(0)
                        output = stdout.read()
                        if "ORBIT_VISUAL_TASK=" in output or server.poll() is not None:
                            break
                        time.sleep(0.1)
                    stderr.seek(0)
                    diagnostics = stderr.read()
                    self.assertIn("ORBIT_VISUAL_URL=http://127.0.0.1:", output, diagnostics)
                    fixture = next(Path(tmp).glob("visual-fixture-busy-workspace-*"))
                    env["HOME"] = str(fixture / "home")
                    orbit = env.get("ORBIT_VISUAL_ORBIT", "orbit")
                    result = subprocess.run([orbit, "--root", str(fixture / "root"), "--json", "task", "list"],
                                            cwd=fixture / "work", env=env, capture_output=True, text=True, check=True)
                    actual = {task["title"]: task["status"] for task in json.loads(result.stdout)}
                    expected = {task["title"]: task["status"] for task in json.loads(SEED.read_text())["tasks"]}
                    self.assertEqual(actual, expected)
                    first_id = (fixture / "task-ids").read_text().splitlines()[0]
                    self.assertIn(f"ORBIT_VISUAL_TASK={first_id}\n", output)
                finally:
                    if server.poll() is None:
                        os.killpg(server.pid, signal.SIGTERM)
                    server.wait(timeout=15)

    def test_faketime_launch_uses_frozen_absolute_timestamp(self):
        # Launch-boundary regression for ORB-20505: the script used to run
        # `faketime -f "$scratch/faketime.rc"` and write "@<epoch>" there.
        # `-f` passes its argument directly as FAKETIME (no file is read),
        # so a pathname is rejected, and "@..." is a running clock.
        # A stub faketime first on PATH records argv/env and execs the
        # real orbit; it does not emulate libfaketime.
        SCRATCH.mkdir(parents=True, exist_ok=True)
        for frozen_env, expected in (
            (None, "2026-01-01 12:00:00"),
            ("2027-06-15T08:30:00Z", "2027-06-15 08:30:00"),
        ):
            with self.subTest(frozen=frozen_env or "default"), tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
                stub_bin = Path(tmp) / "stub-bin"
                stub_bin.mkdir()
                record = Path(tmp) / "faketime.argv"
                stub = stub_bin / "faketime"
                stub.write_text(
                    "#!/bin/sh\n"
                    "printf '%s\\n' \"$@\" > \"$FAKETIME_STUB_RECORD\"\n"
                    "printf 'FAKETIME_DONT_FAKE_MONOTONIC=%s\\nTZ=%s\\n' "
                    "\"${FAKETIME_DONT_FAKE_MONOTONIC:-}\" \"${TZ:-}\" "
                    "> \"$FAKETIME_STUB_RECORD.env\"\n"
                    "while [ $# -gt 0 ]; do\n"
                    "  case \"$1\" in\n"
                    "    -f) shift 2; break ;;\n"
                    "    --exclude-monotonic) shift ;;\n"
                    "    *) break ;;\n"
                    "  esac\n"
                    "done\n"
                    "exec \"$@\"\n"
                )
                stub.chmod(0o755)
                env = {
                    **os.environ,
                    "ORBIT_SCRATCH_DIR": tmp,
                    "FAKETIME_STUB_RECORD": str(record),
                    "PATH": str(stub_bin) + os.pathsep + os.environ.get("PATH", ""),
                }
                if frozen_env is None:
                    env.pop("ORBIT_VISUAL_FROZEN_INSTANT", None)
                else:
                    env["ORBIT_VISUAL_FROZEN_INSTANT"] = frozen_env
                with open(Path(tmp) / "stdout", "w+") as stdout, open(Path(tmp) / "stderr", "w+") as stderr:
                    server = subprocess.Popen([str(SCRIPT), "busy-workspace"], cwd=ROOT, env=env,
                                              stdout=stdout, stderr=stderr, start_new_session=True)
                    try:
                        deadline = time.monotonic() + 90
                        output = ""
                        while time.monotonic() < deadline:
                            stdout.seek(0)
                            output = stdout.read()
                            if "ORBIT_VISUAL_URL=" in output or server.poll() is not None:
                                break
                            time.sleep(0.1)
                        stderr.seek(0)
                        diagnostics = stderr.read()
                        self.assertIn("ORBIT_VISUAL_URL=http://127.0.0.1:", output, diagnostics)
                        self.assertIn("server_clock=faketime", diagnostics)
                        argv = record.read_text().splitlines()
                        self.assertGreaterEqual(len(argv), 3, argv)
                        self.assertEqual(argv[0], "-f", argv)
                        self.assertEqual(argv[1], expected, argv)
                        self.assertRegex(argv[1], r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$")
                        self.assertNotIn("/", argv[1], argv)
                        self.assertFalse(argv[1].startswith("@"), argv)
                        # Remaining argv is the real orbit serve command.
                        self.assertIn("web", argv)
                        self.assertIn("serve", argv)
                        recorded_env = (Path(str(record) + ".env")).read_text()
                        self.assertIn("FAKETIME_DONT_FAKE_MONOTONIC=1", recorded_env)
                        self.assertIn("TZ=UTC", recorded_env)
                    finally:
                        if server.poll() is None:
                            os.killpg(server.pid, signal.SIGTERM)
                        server.wait(timeout=15)

    def test_seed_failure_never_announces_readiness(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        for operation, response in (
            ("task add", 'echo "injected add failure" >&2; exit 9'),
            ("task update", 'echo "injected update failure" >&2; exit 9'),
            ("task show", "printf '%s\\n' '{\"status\":\"wrong\"}'; exit 0"),
        ):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory(dir=SCRATCH) as tmp:
                wrapper = Path(tmp) / "orbit-fail"
                wrapper.write_text(f'#!/bin/sh\ncase " $* " in *" {operation} "*) {response};; esac\nexec orbit "$@"\n')
                wrapper.chmod(0o755)
                result = subprocess.run([str(SCRIPT), "busy-workspace"], cwd=ROOT,
                                        env={**os.environ, "ORBIT_SCRATCH_DIR": tmp, "ORBIT_VISUAL_ORBIT": str(wrapper)},
                                        capture_output=True, text=True, timeout=90)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("expected proposed, got wrong" if operation == "task show" else "injected", result.stderr)
                self.assertNotIn("ORBIT_VISUAL_URL=", result.stdout)



if __name__ == "__main__":
    unittest.main()
