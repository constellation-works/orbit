#!/usr/bin/env python3
"""CLI regressions for paired.py: parity and child-process supervision.

Drives ``scripts/perf/paired.py`` with fake arm binaries. A match requires an
actual successful observation from every arm. Missing observations, failed
probes and differing observations refuse the run before a bounded verdict.
A stalled parity or sample command, or one that floods stdout, is refused
within its bound with its whole process group reaped and the lane released;
cancelling the harness stops the HTTP server it started (ORB-26380).
"""

from __future__ import annotations

import hashlib
import json
import fcntl
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PAIRED = REPO / "scripts" / "perf" / "paired.py"
SCRATCH = REPO / ".orbit" / "tmp"

# Role is baked in so the control copy of the base binary agrees with base,
# while the candidate binary can fail or differ on its own.
ARM_SCRIPT = """#!/bin/sh
role={role}
marks={marks}
mode=$1
case "$mode" in
  stall|probe-stall)
    # A descendant in the arm's group, then a leader that never finishes.
    sleep 600 &
    printf '%s %s\\n' "$$" "$!" > "$marks/$mode-$role.pids"
    sleep 600
    ;;
  probe-flood)
    head -c 200000 /dev/zero
    exit 0
    ;;
  web)
    exec {python} -I {web_server} "$marks/web-$role.pid"
    ;;
  probe-same)
    printf 'observed\\n'
    exit 0
    ;;
  probe-fail)
    exit 9
    ;;
  probe-one-fail)
    if [ "$role" = candidate ]; then
      exit 9
    fi
    printf 'observed\\n'
    exit 0
    ;;
  probe-differ)
    printf '%s\\n' "$role"
    exit 0
    ;;
  timed)
    printf 'timed %s\\n' "$role"
    exit 0
    ;;
  *)
    printf 'unexpected %s\\n' "$mode" >&2
    exit 3
    ;;
esac
"""


# Stands in for `orbit web serve`: announces its address, answers `/`, and
# stalls on `/slow` so the harness can be cancelled while it is serving.
WEB_SERVER = """
import http.server, os, sys, time
from pathlib import Path
pid_file = Path(sys.argv[1])
marks = pid_file.parent
role = pid_file.stem.removeprefix("web-")
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/slow":
            time.sleep(600)
        if self.path == "/destination":
            (marks / "destination-visited").write_text("yes")
        disconnect = self.path == "/disconnect" or (
            self.path == "/disconnect-candidate" and role == "candidate"
        )
        if self.path == "/disconnect-after-parity":
            counter = marks / "disconnect-requests"
            count = int(counter.read_text()) if counter.exists() else 0
            counter.write_text(str(count + 1))
            disconnect = count >= 3
        if disconnect:
            self.close_connection = True
            return
        redirect = self.path == "/redirect"
        if self.path == "/redirect-after-parity":
            counter = marks / "requests"
            count = int(counter.read_text()) if counter.exists() else 0
            counter.write_text(str(count + 1))
            redirect = count >= 3
        if redirect:
            self.send_response(302)
            self.send_header("Location", "/destination")
            self.end_headers()
            self.wfile.write(b"original redirect body")
            return
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
with open(pid_file, "w") as handle:
    handle.write(str(os.getpid()))
with open(marks / "web-all.pids", "a") as handle:
    handle.write(f"{os.getpid()}\\n")
print(f"http://127.0.0.1:{server.server_address[1]}", flush=True)
server.serve_forever()
"""


def process_gone(pid: int) -> bool:
    """Whether `pid` is gone or a zombie awaiting its (non-harness) reaper."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except FileNotFoundError:
        return True
    return stat.rsplit(")", 1)[1].split()[0] in ("Z", "X")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_snapshot(snapshot: Path) -> None:
    """A digest-verified fixture root small enough for a CLI parity run."""
    orbit_dir = snapshot / "checkout" / ".orbit"
    orbit_dir.mkdir(parents=True)
    (orbit_dir / "config.yaml").write_text("workspace_id: test-storage\n")
    (snapshot / "home").mkdir()
    files = []
    for path in snapshot.rglob("*"):
        if not path.is_file():
            continue
        files.append({
            "path": path.relative_to(snapshot).as_posix(),
            "size": path.stat().st_size,
            "sha256": sha256_file(path),
        })
    files.sort(key=lambda entry: entry["path"])
    manifest = {
        "schemaVersion": 1,
        "kind": "orbit-perf-snapshot",
        "source": {
            "selector": "test-ws",
            "registry_id": "test-ws",
            "name": "test-ws",
            "storage_id": "test-storage",
            "global_root": "/fixture-source/home/.orbit",
            "repo_root": "/fixture-source/checkout",
            "orbit_dir": "/fixture-source/checkout/.orbit",
            "home": "/fixture-source/home",
        },
        "snapshot_epoch": 1,
        "files": files,
    }
    (snapshot / "snapshot-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def write_builds(builds: Path, marks: Path) -> dict[str, Path]:
    builds.mkdir()
    web_server = builds.parent / "web_server.py"
    web_server.write_text(WEB_SERVER)
    binaries = {}
    revisions = {}
    for role, commit in (("base", "a"), ("candidate", "c")):
        path = builds / role
        path.write_text(ARM_SCRIPT.format(
            role=role, marks=marks, python=sys.executable, web_server=web_server))
        path.chmod(0o755)
        binaries[role] = path
        revisions[role] = {
            "binary": role,
            "binary_sha256": sha256_file(path),
            "commit": commit * 40,
            "tree": ("b" if role == "base" else "d") * 40,
        }
    (builds / "provenance.json").write_text(json.dumps(
        {"schemaVersion": 1, "kind": "orbit-perf-build-pair", "revisions": revisions},
        indent=2, sort_keys=True,
    ) + "\n")
    return binaries


class PairedCliTestCase(unittest.TestCase):
    def setUp(self) -> None:
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="paired-cli-", dir=SCRATCH)
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.builds = root / "builds"
        self.snapshot = root / "snapshot"
        self.snapshot.mkdir()
        write_snapshot(self.snapshot)
        self.marks = root / "marks"
        self.marks.mkdir()
        self.binaries = write_builds(self.builds, self.marks)
        self.lane_lock = root / "lane.lock"

    def paired_argv(self, out: Path, surface: tuple[str, ...], *extra: str) -> list[str]:
        return [sys.executable, "-I", str(PAIRED),
                "--builds", str(self.builds),
                "--snapshot", str(self.snapshot),
                "--out", str(out),
                "--rounds", "1", "--samples", "1", "--warmup", "0", "--gap-ms", "0",
                "--lane-wait", "5", "--bound-ms", "600000",
                *surface, *extra]

    def paired_env(self) -> dict[str, str]:
        # A private lock: this regression must not take the host perf lane.
        env = os.environ.copy()
        env["ORBIT_PERF_LANE_LOCK"] = str(self.lane_lock)
        return env

    def run_paired(self, *extra: str, surface: tuple[str, ...] = ("--cli", "timed"),
                   ) -> tuple[subprocess.CompletedProcess[str], Path]:
        out = Path(self.temp.name) / f"out-{len(extra)}-{extra[0] if extra else 'cli'}-{surface[-1]}"
        result = subprocess.run(
            self.paired_argv(out, surface, *extra),
            cwd=REPO,
            env=self.paired_env(),
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        return result, out

    def assert_lane_released(self) -> None:
        fd = os.open(self.lane_lock, os.O_RDWR)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)  # raises if still held
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)

    def assert_reaped(self, pids: list[int]) -> None:
        deadline = time.monotonic() + 10
        while not all(process_gone(pid) for pid in pids):
            self.assertLess(time.monotonic(), deadline,
                            f"arm processes still running: {pids}")
            time.sleep(0.05)

    def recorded_web_pids(self) -> list[int]:
        pids_file = self.marks / "web-all.pids"
        self.assertTrue(pids_file.exists(), "expected fake web servers to record PIDs")
        return [int(line) for line in pids_file.read_text().splitlines() if line.strip()]


class PairedCliParityTest(PairedCliTestCase):
    def assert_no_passing_verdict(
        self, result: subprocess.CompletedProcess[str], out: Path,
    ) -> dict:
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        report = json.loads((out / "parity.json").read_text())
        self.assertIs(report["match"], False)
        self.assertFalse((out / "paired-summary.json").exists(), result.stdout)
        self.assertNotIn('"verdict": "pass"', result.stdout)
        return report

    def test_redirect_is_recorded_as_original_response_and_refused(self) -> None:
        result, out = self.run_paired(surface=("--http", "/redirect"))
        report = self.assert_no_passing_verdict(result, out)
        for arm in ("base", "candidate", "control"):
            self.assertEqual(report["arms"][arm]["http"], {
                "status": 302,
                "body": {"format": "text", "value": "original redirect body"},
            })
            self.assertEqual(report["observation_failures"][arm], "http status 302")
        self.assertFalse((self.marks / "destination-visited").exists())

    def test_redirect_during_sampling_is_refused(self) -> None:
        result, out = self.run_paired(surface=("--http", "/redirect-after-parity"))
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("sample 0 exited 302", result.stderr)
        self.assertTrue(json.loads((out / "parity.json").read_text())["match"])
        self.assertFalse((out / "paired-summary.json").exists())
        self.assertFalse((self.marks / "destination-visited").exists())

    def test_http_disconnect_during_parity_writes_refusal_report_and_reaps_server(self) -> None:
        result, out = self.run_paired(surface=("--http", "/disconnect"))
        report = self.assert_no_passing_verdict(result, out)
        self.assertFalse((out / "raw.json").exists())
        for arm in ("base", "candidate", "control"):
            self.assertIn(arm, report["observation_failures"])
            self.assertIn("RemoteDisconnected", report["observation_failures"][arm])
            self.assertIn("RemoteDisconnected", report["arms"][arm]["http"].get("error", ""))
        pids = self.recorded_web_pids()
        self.assertEqual(len(pids), 3)
        self.assert_reaped(pids)
        self.assert_lane_released()

    def test_single_arm_http_disconnect_during_parity_writes_arm_failure_and_reaps_server(self) -> None:
        result, out = self.run_paired(surface=("--http", "/disconnect-candidate"))
        report = self.assert_no_passing_verdict(result, out)
        self.assertFalse((out / "raw.json").exists())
        self.assertIn("RemoteDisconnected", report["observation_failures"].get("candidate", ""))
        self.assertNotIn("base", report["observation_failures"])
        self.assertNotIn("control", report["observation_failures"])
        self.assertIn("candidate", report["differing_arms"])
        self.assertEqual(report["arms"]["base"]["http"]["status"], 200)
        self.assertEqual(report["arms"]["control"]["http"]["status"], 200)
        self.assertIn("RemoteDisconnected", report["arms"]["candidate"]["http"].get("error", ""))
        pids = self.recorded_web_pids()
        self.assertEqual(len(pids), 3)
        self.assert_reaped(pids)
        self.assert_lane_released()

    def test_http_disconnect_during_sampling_is_refused(self) -> None:
        result, out = self.run_paired(surface=("--http", "/disconnect-after-parity"))
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("RemoteDisconnected", result.stderr)
        self.assertTrue(json.loads((out / "parity.json").read_text())["match"])
        self.assertFalse((out / "paired-summary.json").exists())
        pids = self.recorded_web_pids()
        self.assertEqual(len(pids), 4)
        self.assert_reaped(pids)
        self.assert_lane_released()

    def test_omitted_parity_does_not_match_different_timed_stdout(self) -> None:
        base = subprocess.run(
            [self.binaries["base"], "timed"], capture_output=True, check=False, timeout=10)
        candidate = subprocess.run(
            [self.binaries["candidate"], "timed"], capture_output=True, check=False, timeout=10)
        self.assertEqual(base.returncode, 0)
        self.assertEqual(candidate.returncode, 0)
        self.assertNotEqual(base.stdout, candidate.stdout)

        result, out = self.run_paired()
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(set(report["observation_failures"]), {"base", "candidate", "control"})
        self.assertTrue(all(
            reason == "missing observation"
            for reason in report["observation_failures"].values()))
        self.assertEqual(report["arms"]["base"], {})

    def test_identical_failed_probes_do_not_match(self) -> None:
        for role in ("base", "candidate"):
            probe = subprocess.run(
                [self.binaries[role], "probe-fail"], capture_output=True, check=False, timeout=10)
            self.assertEqual(probe.returncode, 9)
            self.assertEqual(probe.stdout, b"")

        result, out = self.run_paired("--parity", "probe-fail")
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(set(report["observation_failures"]), {"base", "candidate", "control"})
        self.assertTrue(all(
            reason == "command exited 9" for reason in report["observation_failures"].values()))
        self.assertEqual(report["arms"]["candidate"]["command"]["exit"], 9)

    def test_one_arm_failed_probe_does_not_match(self) -> None:
        result, out = self.run_paired("--parity", "probe-one-fail")
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(report["observation_failures"].get("candidate"), "command exited 9")
        self.assertNotIn("base", report["observation_failures"])
        self.assertNotIn("control", report["observation_failures"])
        self.assertIn("candidate", report["differing_arms"])

    def test_differing_successful_observations_do_not_match(self) -> None:
        result, out = self.run_paired("--parity", "probe-differ")
        report = self.assert_no_passing_verdict(result, out)
        self.assertEqual(report["observation_failures"], {})
        self.assertIn("candidate", report["differing_arms"])
        self.assertEqual(report["arms"]["base"]["command"]["exit"], 0)
        self.assertEqual(report["arms"]["candidate"]["command"]["exit"], 0)
        self.assertNotEqual(
            report["arms"]["base"]["command"]["output"],
            report["arms"]["candidate"]["command"]["output"])

    def test_identical_successful_observations_match_and_can_pass(self) -> None:
        result, out = self.run_paired("--parity", "probe-same")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        report = json.loads((out / "parity.json").read_text())
        self.assertIs(report["match"], True)
        self.assertEqual(report["observation_failures"], {})
        self.assertEqual(report["differing_arms"], [])
        summary = json.loads((out / "paired-summary.json").read_text())
        self.assertEqual(summary["verdict"], "pass")
        self.assertIn('"verdict": "pass"', result.stdout)


class PairedCliSupervisionTest(PairedCliTestCase):
    def assert_stall_refused(self, mode: str, *extra: str, surface=("--cli", "timed")) -> None:
        started = time.monotonic()
        result, out = self.run_paired(*extra, "--command-timeout", "1", surface=surface)
        elapsed = time.monotonic() - started
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("base", result.stderr)
        self.assertIn("timed out after 1 s", result.stderr)
        self.assertIn("--command-timeout", result.stderr)
        self.assertLess(elapsed, 60, "the refusal must come within the command bound")
        self.assertFalse((out / "paired-summary.json").exists())
        self.assert_lane_released()
        leader, descendant = (int(pid) for pid in
                              (self.marks / f"{mode}-base.pids").read_text().split())
        self.assert_reaped([leader, descendant])

    def test_stalled_parity_command_is_refused_and_reaped(self) -> None:
        self.assert_stall_refused("probe-stall", "--parity", "probe-stall")

    def test_stalled_sample_command_is_refused_and_reaped(self) -> None:
        self.assert_stall_refused("stall", "--parity", "probe-same", surface=("--cli", "stall"))

    def test_output_over_limit_is_refused(self) -> None:
        result, out = self.run_paired("--parity", "probe-flood", "--output-limit-bytes", "1024")
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("wrote more than 1024 bytes of stdout", result.stderr)
        self.assertIn("--output-limit-bytes", result.stderr)
        self.assert_lane_released()

    def test_reported_limits_are_recorded(self) -> None:
        result, out = self.run_paired(
            "--parity", "probe-same", "--command-timeout", "30", "--output-limit-bytes", "4096")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        expected = {"command_timeout_seconds": 30.0, "output_limit_bytes": 4096}
        self.assertEqual(json.loads((out / "parity.json").read_text())["limits"], expected)
        protocol = json.loads((out / "paired-summary.json").read_text())["protocol"]
        self.assertEqual(
            {key: protocol[key] for key in expected}, expected)

    def test_cancelled_harness_stops_its_http_server(self) -> None:
        out = Path(self.temp.name) / "out-cancel"
        harness = subprocess.Popen(
            self.paired_argv(out, ("--http", "/slow")),
            cwd=REPO, env=self.paired_env(),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

        def cleanup() -> None:
            if harness.poll() is None:
                harness.kill()
            harness.communicate(timeout=30)
        self.addCleanup(cleanup)

        pid_file = self.marks / "web-base.pid"
        deadline = time.monotonic() + 60
        while not (pid_file.exists() and pid_file.read_text()):
            self.assertIsNone(harness.poll(), "harness exited before its server started")
            self.assertLess(time.monotonic(), deadline, "server never started")
            time.sleep(0.05)
        server_pid = int(pid_file.read_text())

        def stop_leaked_server() -> None:
            # Only a server this test started: its cmdline names our pid file.
            try:
                cmdline = Path(f"/proc/{server_pid}/cmdline").read_bytes()
            except FileNotFoundError:
                return
            if str(pid_file).encode() in cmdline:
                os.kill(server_pid, signal.SIGKILL)
        self.addCleanup(stop_leaked_server)
        time.sleep(0.5)  # let the harness reach its stalled GET
        harness.send_signal(signal.SIGTERM)
        _stdout, stderr = harness.communicate(timeout=60)
        self.assertEqual(harness.returncode, 128 + signal.SIGTERM, stderr)
        self.assertIn("cancelled by SIGTERM", stderr)
        self.assert_reaped([server_pid])
        self.assert_lane_released()
        self.assertFalse(out.exists(), "a cancelled run leaves no partial output")


if __name__ == "__main__":
    unittest.main()
