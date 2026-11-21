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
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
PAIRED = REPO / "scripts" / "perf" / "paired.py"
SCRATCH = REPO / ".orbit" / "tmp"

# The child's protected-root guard reads the registry under the passwd home
# (`pwd.getpwuid`), which an overridden HOME does not move. Every child therefore
# starts with this patch, which points the passwd home at the fixture home named by
# PAIRED_TEST_PASSWD_HOME; the guard itself runs unchanged against fixture-owned
# registry data and never reads the invoking account's registry (STD-03 §R20).
PASSWD_HOME_PATCH = """
import os, pwd
_getpwuid = pwd.getpwuid
def _fixture_getpwuid(uid):
    entry = _getpwuid(uid)
    return pwd.struct_passwd((entry.pw_name, entry.pw_passwd, entry.pw_uid,
                              entry.pw_gid, entry.pw_gecos,
                              os.environ["PAIRED_TEST_PASSWD_HOME"], entry.pw_shell))
pwd.getpwuid = _fixture_getpwuid
"""
PAIRED_BOOTSTRAP = PASSWD_HOME_PATCH + """
import runpy, sys
script = sys.argv[1]
sys.argv = [script] + sys.argv[2:]
runpy.run_path(script, run_name="__main__")
"""

# A dedicated Linux subreaper owns the entire cancellation fixture, including
# sessions created by paired.py. Its direct children cannot have their PIDs
# reused until waitpid reaps them; pidfds keep each signal bound to that child.
# SIGUSR1 requests emergency cleanup, while the other signals are forwarded to
# the harness so the cancellation assertions still exercise paired.py itself.
CANCELLATION_OWNER = r"""
import ctypes, os, signal, subprocess, sys, time
if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
    raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER")
harness = None
abort = False
def forward(signum, _frame):
    global abort
    if signum == signal.SIGUSR1:
        abort = True
    elif harness is not None:
        try:
            harness.send_signal(signum)
        except ProcessLookupError:
            pass
for sig in (signal.SIGUSR1, signal.SIGTERM, signal.SIGINT):
    signal.signal(sig, forward)
harness = subprocess.Popen(sys.argv[1:])
status = None
while status is None and not abort:
    status = harness.poll()
    time.sleep(0.01)
if status is None:
    harness.kill()
    status = harness.wait()
# The harness is gone, so all surviving descendants eventually become our
# direct children. Never reap a child before pinning and signalling it.
deadline = time.monotonic() + 10
while True:
    children = open("/proc/self/task/%d/children" % os.getpid()).read().split()
    if not children:
        break
    for child in children:
        try:
            fd = os.pidfd_open(int(child))
            try:
                signal.pidfd_send_signal(fd, signal.SIGKILL)
            finally:
                os.close(fd)
        except ProcessLookupError:
            pass
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            break
        if pid == 0:
            break
    if time.monotonic() >= deadline:
        raise RuntimeError("fixture descendants did not exit within 10 seconds")
    time.sleep(0.01)
sys.exit(status if status >= 0 else 128 - status)
"""

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
  ignore-term)
    exec {python} -I {ignore_term_driver} "$marks/ignore-$role.pid"
    ;;
  graceful-descendant)
    exec {python} -I {graceful_driver} "$marks/graceful-$role"
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


# Signal and PID markers that a cancellation test reads while the writer may
# still be running. The writer creates a temp sibling, pauses before flushing
# (the 200 ms gap keeps that window open on every run), and renames the value
# into place. A reader that sees the marker's name therefore sees the whole
# value; creating the marker first would publish an empty file (ORB-26449).
MARKER_PUBLISH = """
import os, pathlib, time
def publish_marker(path, text):
    path = pathlib.Path(path)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w") as handle:
        time.sleep(0.2)
        handle.write(text)
    os.replace(tmp, path)
"""

IGNORE_TERM_DRIVER = MARKER_PUBLISH + r"""
import os, pathlib, signal, sys, time
pid_file = pathlib.Path(sys.argv[1])
# The handler is installed before the PID is staged: publish_marker pauses before
# it writes, and a supervision deadline that lands in that pause must still reach
# this handler rather than the default disposition (ORB-26485).
signal.signal(signal.SIGTERM, lambda signum, frame: publish_marker(pid_file.with_suffix(".term"), str(signum)))
publish_marker(pid_file, str(os.getpid()))
time.sleep(600)
"""


# Leader exits immediately on SIGTERM while one descendant performs a short
# graceful shutdown and a sibling ignores SIGTERM until the group is swept.
GRACEFUL_DRIVER = r"""
import os, pathlib, signal, subprocess, sys, time

prefix = pathlib.Path(sys.argv[1])
child_code = '''
import os, pathlib, signal, sys, time
prefix = pathlib.Path(sys.argv[1])
mode = sys.argv[2]
if mode == "graceful":
    shutting_down = False
    def handle_term(_signum, _frame):
        global shutting_down
        shutting_down = True
    signal.signal(signal.SIGTERM, handle_term)
    prefix.with_suffix(".graceful.pid").write_text(str(os.getpid()))
    while not shutting_down:
        time.sleep(0.02)
    time.sleep(0.3)
    prefix.with_suffix(".done").write_text("graceful-done\\n")
else:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    prefix.with_suffix(".stubborn.pid").write_text(str(os.getpid()))
    time.sleep(600)
'''
graceful = subprocess.Popen([sys.executable, "-I", "-c", child_code, str(prefix), "graceful"])
stubborn = subprocess.Popen([sys.executable, "-I", "-c", child_code, str(prefix), "stubborn"])
for suffix in (".graceful.pid", ".stubborn.pid"):
    marker = prefix.with_suffix(suffix)
    while not marker.exists() or not marker.read_text():
        time.sleep(0.01)
exiting = False
def leader_term(_signum, _frame):
    global exiting
    exiting = True
signal.signal(signal.SIGTERM, leader_term)
prefix.with_suffix(".leader.pid").write_text(str(os.getpid()))
while not exiting:
    time.sleep(0.02)
os._exit(0)
"""


# Stands in for `orbit web serve`: announces its address, answers `/`, and
# stalls on `/slow` so the harness can be cancelled while it is serving.
WEB_SERVER = MARKER_PUBLISH + """
import http.server, os, signal, subprocess, sys, time
from pathlib import Path
pid_file = Path(sys.argv[1])
marks = pid_file.parent
role = pid_file.stem.removeprefix("web-")
if (marks / "ignore-web-term").exists():
    def record_term(signum, _frame):
        publish_marker(marks / f"web-{role}.term", str(signum))
    signal.signal(signal.SIGTERM, record_term)
    child = subprocess.Popen([
        sys.executable, "-c",
        "import pathlib, signal, sys, time; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        "pathlib.Path(sys.argv[1]).write_text('ready'); time.sleep(600)",
        str(marks / f"web-{role}.descendant.ready"),
    ])
    publish_marker(marks / f"web-{role}.descendant.pid", str(child.pid))
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/slow":
            (marks / f"web-{role}.slow").touch()
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
publish_marker(pid_file, str(os.getpid()))
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
    graceful_driver = builds.parent / "graceful_driver.py"
    graceful_driver.write_text(GRACEFUL_DRIVER)
    ignore_term_driver = builds.parent / "ignore_term_driver.py"
    ignore_term_driver.write_text(IGNORE_TERM_DRIVER)
    binaries = {}
    revisions = {}
    for role, commit in (("base", "a"), ("candidate", "c")):
        path = builds / role
        # The arm script is shell source, so every interpolated path is quoted:
        # a checkout or scratch path with a space must stay one word.
        path.write_text(ARM_SCRIPT.format(
            role=role,
            marks=shlex.quote(str(marks)),
            python=shlex.quote(sys.executable),
            web_server=shlex.quote(str(web_server)),
            graceful_driver=shlex.quote(str(graceful_driver)),
            ignore_term_driver=shlex.quote(str(ignore_term_driver)),
        ))
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
    # Prefix of the disposable root under SCRATCH; subclasses may vary it.
    root_prefix = "paired-cli-"

    def setUp(self) -> None:
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix=self.root_prefix, dir=SCRATCH)
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
        self.home = root / "home"
        self.home.mkdir()
        self.register_checkouts()

    def register_checkouts(self, *orbit_dirs: Path) -> None:
        """Write the fixture-owned registry the child's protected-root guard reads."""
        registry = self.home / ".orbit" / "workspaces.json"
        registry.parent.mkdir(parents=True, exist_ok=True)
        registry.write_text(json.dumps(
            {"checkouts": [{"orbit_dir": str(path)} for path in orbit_dirs]}))

    def paired_argv(self, out: Path, surface: tuple[str, ...], *extra: str) -> list[str]:
        return [sys.executable, "-I", "-c", PAIRED_BOOTSTRAP, str(PAIRED),
                "--builds", str(self.builds),
                "--snapshot", str(self.snapshot),
                "--out", str(out),
                "--rounds", "1", "--samples", "1", "--warmup", "0", "--gap-ms", "0",
                "--lane-wait", "5", "--bound-ms", "600000",
                *surface, *extra]

    def paired_env(self) -> dict[str, str]:
        # No inherited Orbit or Git authority reaches the child (STD-03 §R20): it
        # gets its own HOME, passwd home (PASSWD_HOME_PATCH), Orbit root and
        # registry, and a private lane lock so this regression never takes the
        # host perf lane. The child's protected-root guard runs unchanged.
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("ORBIT_", "GIT_"))}
        env["HOME"] = str(self.home)
        env["PAIRED_TEST_PASSWD_HOME"] = str(self.home)
        env["ORBIT_ROOT"] = str(self.home / ".orbit")
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

    def start_cancellation_harness(self, out: Path, surface: tuple[str, ...],
                                   *extra: str) -> subprocess.Popen:
        owner = subprocess.Popen(
            [sys.executable, "-I", "-c", CANCELLATION_OWNER,
             *self.paired_argv(out, surface, *extra)],
            cwd=REPO, env=self.paired_env(),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

        def cleanup() -> None:
            if owner.poll() is None:
                owner.send_signal(signal.SIGUSR1)
            owner.communicate(timeout=30)
        self.addCleanup(cleanup)
        return owner

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
        result, out = self.run_paired("--parity", "probe-same",
                                     "--qualifying-task-count", "indexed_tasks=14819")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        report = json.loads((out / "parity.json").read_text())
        self.assertIs(report["match"], True)
        self.assertEqual(report["observation_failures"], {})
        self.assertEqual(report["differing_arms"], [])
        summary = json.loads((out / "paired-summary.json").read_text())
        self.assertEqual(summary["verdict"], "pass")
        evidence = json.loads((out / "snapshot-manifest-summary.json").read_text())
        self.assertEqual(evidence["manifest"]["file_count"], 1)
        self.assertEqual(evidence["manifest"]["size_bytes"],
                         Path(evidence["manifest"]["path_on_host"]).stat().st_size)
        self.assertEqual(evidence["qualifying_task_counts"], {"indexed_tasks": 14819})
        self.assertEqual(evidence["snapshot"]["source_workspace"], "test-ws")
        self.assertEqual(evidence["revisions"]["base"], "a" * 40)
        self.assertEqual(evidence["revisions"]["candidate"], "c" * 40)
        self.assertLessEqual((out / "snapshot-manifest-summary.json").stat().st_size,
                             1_048_576)
        self.assertEqual(summary["snapshot"]["manifest_summary"],
                         str((out / "snapshot-manifest-summary.json").resolve()))
        self.assertIn('"verdict": "pass"', result.stdout)

    def test_inherited_orbit_root_does_not_refuse_isolated_parity(self) -> None:
        # An ORBIT_ROOT exported by the invoking shell that encloses this run's
        # scratch must not make the harness refuse its own disposable restore.
        with mock.patch.dict(os.environ, {"ORBIT_ROOT": str(SCRATCH)}):
            result, out = self.run_paired("--parity", "probe-same")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertIs(json.loads((out / "parity.json").read_text())["match"], True)

    def test_registered_checkout_on_host_does_not_refuse_isolated_parity(self) -> None:
        # The guard reads the fixture-owned registry only: the invoking account's
        # registry, even one naming this checkout, cannot refuse the run. The
        # fixture registry here names an unrelated directory.
        self.register_checkouts(self.home / "unrelated" / ".orbit")
        result, out = self.run_paired("--parity", "probe-same")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertIs(json.loads((out / "parity.json").read_text())["match"], True)

    def test_fixture_registered_protected_destination_is_still_refused(self) -> None:
        # The real guard stays effective: a checkout the fixture registry names
        # protects the scratch root enclosed in it.
        self.register_checkouts(REPO / ".orbit")
        result, out = self.run_paired("--parity", "probe-same")
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("refusing to restore into", result.stderr)
        self.assertIn(str(REPO / ".orbit"), result.stderr)
        self.assertNotIn('"verdict": "pass"', result.stdout)
        self.assertFalse((out / "paired-summary.json").exists())

    def test_explicit_empty_parity_command_is_refused_in_http_mode(self) -> None:
        # ORB-26398: an explicit empty `--parity` names no command, so it must not
        # stand in for an omitted one and let HTTP-only observations match.
        result, out = self.run_paired("--parity", "", surface=("--http", "/ok"))
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("--parity", result.stderr)
        self.assertNotIn('"verdict": "pass"', result.stdout)
        self.assertFalse(out.exists(), "a refused usage error creates no output")
        self.assertFalse((self.marks / "web-all.pids").exists(),
                         "no arm server may start for an empty parity command")


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

    def test_timed_out_cli_preserves_grace_after_leader_exits(self) -> None:
        def cleanup_leaked_group() -> None:
            marker_bytes = str(self.marks).encode()
            for suffix in (".leader.pid", ".graceful.pid", ".stubborn.pid"):
                pid_file = self.marks / f"graceful-base{suffix}"
                if not pid_file.exists():
                    continue
                pid = int(pid_file.read_text())
                try:
                    cmdline = Path(f"/proc/{pid}/cmdline").read_bytes()
                except FileNotFoundError:
                    continue
                if marker_bytes in cmdline:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
        self.addCleanup(cleanup_leaked_group)

        started = time.monotonic()
        result, out = self.run_paired(
            "--parity", "probe-same", "--command-timeout", "1",
            surface=("--cli", "graceful-descendant"),
        )
        elapsed = time.monotonic() - started
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("base", result.stderr)
        self.assertIn("timed out after 1 s", result.stderr)
        self.assertIn("--command-timeout", result.stderr)
        self.assertLess(elapsed, 60, "the refusal must come within the command bound")
        self.assertFalse((out / "paired-summary.json").exists())
        self.assert_lane_released()

        leader_pid = int((self.marks / "graceful-base.leader.pid").read_text())
        graceful_pid = int((self.marks / "graceful-base.graceful.pid").read_text())
        stubborn_pid = int((self.marks / "graceful-base.stubborn.pid").read_text())
        self.assert_reaped([leader_pid, graceful_pid, stubborn_pid])
        done_file = self.marks / "graceful-base.done"
        self.assertTrue(
            done_file.exists(),
            "in-group descendant did not finish its graceful shutdown after leader exit",
        )
        self.assertEqual(done_file.read_text(), "graceful-done\n")

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

    def test_large_command_timeout_allows_short_probe(self) -> None:
        result, out = self.run_paired("--parity", "probe-same", "--command-timeout", "3000000")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        report = json.loads((out / "parity.json").read_text())
        self.assertIs(report["match"], True)
        self.assertEqual(report["limits"]["command_timeout_seconds"], 3_000_000.0)

    def test_cancellation_during_process_acquisition(self) -> None:
        # Trace Popen's return, before its caller can install a cleanup guard.
        driver = PASSWD_HOME_PATCH + """
import os, runpy, signal, subprocess, sys
script, kind, pidfile, signum = sys.argv[1:5]
sys.argv = [script] + sys.argv[5:]
spawned = None
def trace(frame, event, arg):
    global spawned
    if spawned is not None and event == "call" and frame.f_code.co_name == "clean_root":
        try:
            os.kill(spawned, 0)
        except ProcessLookupError:
            state = "reaped"
        else:
            state = "live"
        with open(pidfile + ".cleanup", "w") as output:
            output.write(state)
        sys.settrace(None)
    if spawned is None and event == "return" and frame.f_code is subprocess.Popen.__init__.__code__:
        args = frame.f_locals.get("args", [])
        if (kind == "server" and "web" in args) or (kind == "arm" and "probe-stall" in args):
            spawned = frame.f_locals["self"].pid
            with open(pidfile, "w") as output:
                output.write(str(spawned))
            os.kill(os.getpid(), int(signum))
    return trace
sys.settrace(trace)
runpy.run_path(script, run_name="__main__")
"""
        for kind, signum in (("arm", signal.SIGTERM), ("server", signal.SIGINT)):
            with self.subTest(kind=kind):
                out = Path(self.temp.name) / ("out-acquire-" + kind)
                pidfile = Path(self.temp.name) / ("acquire-" + kind + ".pid")
                surface = ("--http", "/slow") if kind == "server" else ("--cli", "stall")
                args = self.paired_argv(out, surface, "--parity",
                                        "probe-same" if kind == "server" else "probe-stall")
                result = subprocess.run(
                    [sys.executable, "-I", "-c", driver, str(PAIRED), kind,
                     str(pidfile), str(signum), *args[5:]],
                    cwd=REPO, env=self.paired_env(), capture_output=True, text=True, timeout=60,
                )
                self.assertTrue(pidfile.exists(), result.stderr)
                pid = int(pidfile.read_text())
                try:
                    self.assertEqual(result.returncode, 128 + signum, result.stderr)
                    self.assertTrue(process_gone(pid), f"owned leader {pid} survived cancellation")
                    self.assertEqual(Path(str(pidfile) + ".cleanup").read_text(), "reaped")
                    self.assert_lane_released()
                    self.assertFalse(out.exists())
                finally:
                    if not process_gone(pid):
                        os.killpg(pid, signal.SIGKILL)

    def test_interrupt_during_server_stop_kills_group_and_preserves_cancellation(self) -> None:
        (self.marks / "ignore-web-term").touch()
        out = Path(self.temp.name) / "out-cancel"
        harness = self.start_cancellation_harness(out, ("--http", "/slow"))

        pid_file = self.marks / "web-base.pid"

        def pin_fixture_process(pid: int, marker: Path) -> int:
            try:
                pidfd = os.pidfd_open(pid)
            except (AttributeError, OSError) as error:
                self.fail(f"could not pin fixture process {pid}: {error}")
            try:
                cmdline = Path(f"/proc/{pid}/cmdline").read_bytes()
            except OSError as error:
                os.close(pidfd)
                self.fail(f"could not verify fixture process {pid}: {error}")
            if str(marker).encode() not in cmdline:
                os.close(pidfd)
                self.fail(f"fixture process {pid} did not name {marker}")
            # Keep the descriptor until the leaked-process cleanup has run. It
            # continues to identify this process if it exits and its PID is reused.
            self.addCleanup(os.close, pidfd)
            return pidfd

        deadline = time.monotonic() + 60
        while not (pid_file.exists() and pid_file.read_text()):
            self.assertIsNone(harness.poll(), "harness exited before its server started")
            self.assertLess(time.monotonic(), deadline, "server never started")
            time.sleep(0.05)
        server_pid = int(pid_file.read_text())
        server_pidfd = pin_fixture_process(server_pid, pid_file)
        descendant_file = self.marks / "web-base.descendant.pid"
        deadline = time.monotonic() + 10
        while not descendant_file.exists():
            self.assertIsNone(harness.poll(), "server exited before recording its descendant")
            self.assertLess(time.monotonic(), deadline, "server descendant was not started")
            time.sleep(0.01)
        descendant_pid = int(descendant_file.read_text())
        descendant_ready = self.marks / "web-base.descendant.ready"
        descendant_pidfd = pin_fixture_process(descendant_pid, descendant_ready)
        deadline = time.monotonic() + 10
        while not descendant_ready.exists():
            self.assertIsNone(harness.poll(), "server descendant exited before becoming ready")
            self.assertLess(time.monotonic(), deadline, "server descendant did not become ready")
            time.sleep(0.01)

        def stop_leaked_server() -> None:
            # Signal the pinned fixture processes individually. A numeric PID or
            # process-group ID could be reused after the cmdline check above.
            for pidfd in (server_pidfd, descendant_pidfd):
                try:
                    signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        self.addCleanup(stop_leaked_server)
        slow_file = self.marks / "web-base.slow"
        deadline = time.monotonic() + 10
        while not slow_file.exists():
            self.assertIsNone(harness.poll(), "harness exited before starting its stalled GET")
            self.assertLess(time.monotonic(), deadline, "server did not start its stalled GET")
            time.sleep(0.01)
        harness.send_signal(signal.SIGTERM)

        term_file = self.marks / "web-base.term"
        deadline = time.monotonic() + 10
        while not term_file.exists():
            self.assertIsNone(harness.poll(), "harness exited before server cleanup started")
            self.assertLess(time.monotonic(), deadline, "server did not receive SIGTERM")
            time.sleep(0.01)
        self.assertEqual(term_file.read_text(), str(signal.SIGTERM))
        harness.send_signal(signal.SIGINT)
        _stdout, stderr = harness.communicate(timeout=60)
        self.assertEqual(harness.returncode, 128 + signal.SIGINT, stderr)
        self.assertIn("cancelled by SIGINT", stderr)
        self.assert_reaped([server_pid, descendant_pid])
        self.assert_lane_released()
        self.assertFalse(out.exists(), "a cancelled run leaves no partial output")

    def test_interrupt_during_supervised_stop_kills_group_and_preserves_cancellation(self) -> None:
        out = Path(self.temp.name) / "out-supervised-cancel"
        harness = self.start_cancellation_harness(
            out, ("--cli", "ignore-term"),
            "--parity", "probe-same", "--command-timeout", "1")

        pid_file = self.marks / "ignore-base.pid"
        deadline = time.monotonic() + 60
        while not pid_file.exists():
            self.assertIsNone(harness.poll(), "harness exited before starting the supervised child")
            self.assertLess(time.monotonic(), deadline, "supervised child never started")
            time.sleep(0.01)
        leader_pid = int(pid_file.read_text())

        term_file = self.marks / "ignore-base.term"
        deadline = time.monotonic() + 60
        while not term_file.exists():
            self.assertIsNone(harness.poll(), "harness exited before supervised cleanup started")
            self.assertLess(time.monotonic(), deadline, "supervised child did not receive SIGTERM")
            time.sleep(0.01)
        self.assertEqual(term_file.read_text(), str(signal.SIGTERM))
        harness.send_signal(signal.SIGINT)
        _stdout, stderr = harness.communicate(timeout=60)
        self.assertEqual(harness.returncode, 128 + signal.SIGINT, stderr)
        self.assertIn("cancelled by SIGINT", stderr)
        self.assert_reaped([leader_pid])
        self.assert_lane_released()
        self.assertFalse((out / "paired-summary.json").exists())

    def test_cancellation_setup_failure_reaps_owned_processes(self) -> None:
        for method, marker in (
            ("test_interrupt_during_server_stop_kills_group_and_preserves_cancellation", "web-base.pid"),
            ("test_interrupt_during_supervised_stop_kills_group_and_preserves_cancellation", "ignore-base.pid"),
        ):
            with self.subTest(method=method):
                case = type(self)(method)
                case.setUp()
                observed = []
                owner = None
                start = case.start_cancellation_harness
                read_text = Path.read_text

                def start_owner(*args, **kwargs):
                    nonlocal owner
                    owner = start(*args, **kwargs)
                    return owner

                def fail_pid_read(path, *args, **kwargs):
                    if path == case.marks / marker:
                        observed.append(int(read_text(path)))
                        descendant = case.marks / "web-base.descendant.pid"
                        if descendant.exists():
                            observed.append(int(read_text(descendant)))
                        children = Path(f"/proc/{owner.pid}/task/{owner.pid}/children")
                        observed.extend(int(pid) for pid in read_text(children).split())
                        raise ValueError("injected marker read failure")
                    return read_text(path, *args, **kwargs)

                def verify_cleanup():
                    self.assertTrue(observed, "the failure must follow fixture startup")
                    self.assertIsNotNone(owner.returncode)
                    self.assert_reaped(observed)
                    case.assert_lane_released()

                case.addCleanup(verify_cleanup)
                case.start_cancellation_harness = start_owner
                started = time.monotonic()
                try:
                    with mock.patch.object(Path, "read_text", fail_pid_read):
                        with self.assertRaisesRegex(ValueError, "injected marker read failure"):
                            getattr(case, method)()
                finally:
                    self.assertTrue(case.doCleanups())
                self.assertLess(time.monotonic() - started, 30)

    def test_sigterm_during_fixture_pid_staging_reaches_handler(self) -> None:
        # ORB-26485: a supervision deadline can expire while the ignore-term fixture
        # is still staging its PID. The `<pid>.tmp` sibling exists for the whole
        # staging pause, so deliver the SIGTERM once it appears and require both
        # markers to become complete. The handler must already be installed then.
        pid_file = self.marks / "staged-base.pid"
        fixture = subprocess.Popen(
            [sys.executable, "-I", str(self.builds.parent / "ignore_term_driver.py"),
             str(pid_file)],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
        )

        def cleanup() -> None:
            if fixture.poll() is None:
                fixture.kill()
            fixture.communicate(timeout=30)
        self.addCleanup(cleanup)

        staging = pid_file.with_name(pid_file.name + ".tmp")
        deadline = time.monotonic() + 60
        while not staging.exists():
            self.assertIsNone(fixture.poll(), "fixture exited before staging its PID")
            self.assertLess(time.monotonic(), deadline, "fixture never started staging its PID")
            time.sleep(0.001)
        fixture.send_signal(signal.SIGTERM)

        term_file = self.marks / "staged-base.term"
        deadline = time.monotonic() + 60
        while not (term_file.exists() and pid_file.exists()):
            returncode = fixture.poll()
            self.assertIsNone(returncode, f"fixture exited {returncode} before publishing its markers")
            self.assertLess(time.monotonic(), deadline, "fixture markers never became readable")
            time.sleep(0.001)
        self.assertEqual(int(pid_file.read_text()), fixture.pid)
        self.assertEqual(term_file.read_text(), str(signal.SIGTERM))


class PairedCliSpacedPathTest(PairedCliTestCase):
    # ORB-26414: the fake arms are shell scripts that name their marks
    # directory, interpreter and helper scripts in shell source. A root path
    # containing a space must not split those words, or the arms never record.
    root_prefix = "paired cli spaced "

    def test_stall_arm_records_markers_under_spaced_root(self) -> None:
        result, out = self.run_paired("--parity", "probe-same", "--command-timeout", "1",
                                      surface=("--cli", "stall"))
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("timed out after 1 s", result.stderr)
        self.assertFalse((out / "paired-summary.json").exists())
        leader, descendant = (int(pid) for pid in
                              (self.marks / "stall-base.pids").read_text().split())
        self.assert_reaped([leader, descendant])
        self.assert_lane_released()

    def test_http_arms_record_markers_under_spaced_root(self) -> None:
        result, out = self.run_paired(surface=("--http", "/ok"))
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        report = json.loads((out / "parity.json").read_text())
        self.assertIs(report["match"], True)
        for arm in ("base", "candidate", "control"):
            self.assertEqual(report["arms"][arm]["http"]["status"], 200)
        pids = self.recorded_web_pids()
        self.assertGreaterEqual(len(pids), 3)
        self.assert_reaped(pids)
        self.assert_lane_released()


if __name__ == "__main__":
    unittest.main()
