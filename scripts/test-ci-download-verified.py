#!/usr/bin/env python3
"""Offline public download/installer regressions; no GitHub requests or state writes."""

import base64
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import subprocess
import ssl
import sys
import tarfile
import tempfile
import threading
import time
import unittest

import yaml


ROOT = Path(__file__).resolve().parent.parent
HELPER = ROOT / "scripts/ci-download-verified.sh"
URL = "https://downloads.example.invalid/pinned.tar.gz"
BODY = b"verified fixture bytes\n"

# Real Bash/Python run the public helper; real sha256/tar/version commands run
# the workflow installers. Short-deadline tests additionally gate startup below.
CURL = f"#!{sys.executable}\n" + r'''
import base64, json, os, sys, time
from pathlib import Path
args = sys.argv[1:]
root = Path(os.environ["DOWNLOAD_FIXTURE"])
calls = root / "calls.json"
history = json.loads(calls.read_text()) if calls.exists() else []
history.append(args)
rows = json.loads((root / "responses.json").read_text())
row = rows[min(len(history) - 1, len(rows) - 1)]

# A silent delay longer than the old outer caps must still answer. A row that
# never answers writes nothing; the test's hang wait, not a short cap, stops it.
if row.get("never_answers"):
    time.sleep(86400)
    sys.exit(0)
delay = row.get("delay_seconds") or 0
if delay:
    time.sleep(float(delay))

def signal(stage):
    if "DOWNLOAD_READY_FD" in os.environ:
        os.write(int(os.environ["DOWNLOAD_READY_FD"]), stage)

def note_elapsed():
    # A finished transport consumes a fixed slice of the injected clock so the
    # next backoff shares one deadline. 0.05s stays inside the 0.3s budget
    # (that attempt still finishes and refuses the 1s backoff) and makes the
    # 1.4s budget's second watchdog strictly under 0.4s without real time.
    clock = os.environ.get("ORBIT_CI_DOWNLOAD_FAKE_CLOCK")
    if not clock:
        return
    path = Path(clock)
    now = float(path.read_text())
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(f"{now + 0.05:.6f}\n")
    temporary.replace(path)

def block_until_deadline():
    # The helper arms the deadline after readiness, then this transport crosses
    # it. Wall time does not decide the outcome; the safety sleep only holds
    # the process until the helper's watchdog kills it.
    arm = os.environ.get("ORBIT_CI_DOWNLOAD_CLOCK_ARM_FD")
    wake = os.environ.get("ORBIT_CI_DOWNLOAD_CLOCK_WAKE_FD")
    clock = os.environ.get("ORBIT_CI_DOWNLOAD_FAKE_CLOCK")
    if not (arm and wake and clock):
        time.sleep(60)
        return
    data = b""
    while b"\n" not in data:
        chunk = os.read(int(arm), 64)
        if not chunk:
            break
        data += chunk
    end = float(data.strip() or "0")
    path = Path(clock)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(f"{end + 0.001:.6f}\n")
    temporary.replace(path)
    os.write(int(wake), b"X")
    time.sleep(60)

if row.get("pause_start"):
    signal(b"S")
    block_until_deadline()
# Cancellation must leave either the old complete receipt or the new one.
# The barrier exposes the old truncate-before-write failure without host load.
pending = root / "calls.pending"
pending.write_text("")
if row.get("pause_receipt"):
    signal(b"W")
    block_until_deadline()
pending.write_text(json.dumps(history))
pending.replace(calls)
output = args[args.index("--output") + 1] if "--output" in args else args[args.index("-o") + 1]
if "body" in row:
    Path(output).write_bytes(base64.b64decode(row["body"]))
signal(b"R")
if row.get("stall"):
    block_until_deadline()
print("fixture curl diagnostic", file=sys.stderr)
if "--write-out" in args:
    sys.stdout.write(row.get("http", "000"))
note_elapsed()
sys.exit(row.get("exit", 0))
'''


# This test-only python3 launcher executes the public helper's stdin unchanged.
# The helper's fake clock, not this launcher, decides backoff and the watchdog.
# Readiness is still an explicit fixture stage so cancellation barriers record
# the same ordering. Cases that need no backoff or deadline keep the real python3
# and never install this launcher.
DEADLINE_PYTHON = r'''
import builtins, json, os, subprocess, sys, time
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

source = sys.stdin.read()
root = Path(os.environ["DOWNLOAD_FIXTURE"])
events = []
real_popen = subprocess.Popen
real_import = builtins.__import__

class ReadyPopen(real_popen):
    def __init__(self, *args, **kwargs):
        read_fd, write_fd = os.pipe()
        os.set_inheritable(write_fd, True)
        env = dict(kwargs.get("env") or os.environ)
        env["DOWNLOAD_READY_FD"] = str(write_fd)
        kwargs["env"] = env
        kwargs["pass_fds"] = tuple(kwargs.get("pass_fds") or ()) + (write_fd,)
        try:
            super().__init__(*args, **kwargs)
        finally:
            os.close(write_fd)
        try:
            stage = os.read(read_fd, 1).decode()
            expected = os.environ.get("DOWNLOAD_EXPECT_READY", "R")
            attempt = sum("pid" in event for event in events)
            if not stage or stage != expected[min(attempt, len(expected) - 1)]:
                raise RuntimeError(f"unexpected fixture readiness: {stage!r}")
            self.deadline_event = {"stage": stage, "pid": self.pid, "startup_seconds": 0.0}
            events.append(self.deadline_event)
        except BaseException:
            self.kill()
            self.wait()
            raise
        finally:
            os.close(read_fd)

    def kill(self):
        event = getattr(self, "deadline_event", None)
        if event is not None:
            event["timed_out"] = True
        return super().kill()

def run(*args, **kwargs):
    raise RuntimeError("deadline tests must use the injected clock, not wall-clock subprocess.run")

def sleep(delay):
    events.append({"backoff": delay})
    if not os.environ.get("ORBIT_CI_DOWNLOAD_FAKE_CLOCK"):
        time.sleep(delay)

def import_module(name, globals=None, *args, **kwargs):
    if name == "time" and globals and globals.get("__name__") == "__main__":
        return SimpleNamespace(monotonic=lambda: None, sleep=sleep)
    return real_import(name, globals, *args, **kwargs)

sys.argv = ["-", *sys.argv[2:]]
try:
    with patch.object(builtins, "__import__", import_module), \
            patch.object(subprocess, "Popen", ReadyPopen), patch.object(subprocess, "run", run):
        exec(compile(source, "ci-download-verified.sh:stdin", "exec"), {"__name__": "__main__"})
finally:
    for event in events:
        if "pid" not in event:
            continue
        try:
            os.waitpid(event["pid"], os.WNOHANG)
        except ChildProcessError:
            event["reaped"] = True
        else:
            event["reaped"] = False
    (root / "deadline-events.json").write_text(json.dumps(events))
    clock = os.environ.get("ORBIT_CI_DOWNLOAD_FAKE_CLOCK")
    active = float(Path(clock).read_text()) if clock else 0.0
    (root / "deadline-summary.json").write_text(json.dumps({
        "active_seconds": active,
        "excluded_startup_seconds": 0.0,
    }))
'''


def response(exit_code=0, http="200", body=BODY, **extra):
    row = {"exit": exit_code, "http": http, **extra}
    if body is not None:
        row["body"] = base64.b64encode(body).decode()
    return row


# The old outer caps were 3s, 5s, 8s and 10s. A correct run on a busy host
# crossed them. Output refreshes the quiet window, so a slow child is not a
# hang. Silence with no exit is. 120s is the saturated-host fixture step
# deadline; no correct local fixture run approaches it. 300s stops a child
# that writes forever. 12s is longer than every removed cap.
HANG_QUIET_SECONDS = 120
HANG_ABSOLUTE_SECONDS = 300
SLOW_FIXTURE_SECONDS = 12


def _kill_group(proc):
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        return
    except PermissionError:
        if proc.poll() is None:
            try:
                proc.kill()
            except ProcessLookupError:
                return
    if proc.poll() is None:
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                proc.kill()
            except ProcessLookupError:
                pass
            proc.wait(timeout=5)


def _read_chunk(stream):
    try:
        chunk = os.read(stream.fileno(), 65536)
    except BlockingIOError:
        return b"", False
    except OSError:
        return b"", True
    if not chunk:
        return b"", True
    return chunk, False


def _take(stream, bucket):
    progressed = False
    while True:
        chunk, eof = _read_chunk(stream)
        if chunk:
            bucket.append(chunk)
            progressed = True
        if eof or not chunk:
            return progressed, eof


def run_step(argv, *, step, env=None, cwd=None, text=False, check=False, hang_quiet=None):
    """Run argv until it exits.

    A fixed wall-clock cap failed correct fixture runs when the host was slow.
    The quiet window refreshes whenever the child writes. Silence with no exit,
    or a run that reaches the absolute ceiling, is a hang and names `step`.
    """
    if hang_quiet is None:
        quiet_budget = HANG_QUIET_SECONDS
        absolute_budget = HANG_ABSOLUTE_SECONDS
    else:
        quiet_budget = hang_quiet
        absolute_budget = hang_quiet
    proc = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=0,
        start_new_session=True,
    )
    stdout_chunks = []
    stderr_chunks = []
    streams = [proc.stdout, proc.stderr]
    started = time.monotonic()
    quiet_deadline = started + quiet_budget
    absolute_deadline = started + absolute_budget
    try:
        for stream in streams:
            os.set_blocking(stream.fileno(), False)
        while proc.poll() is None or streams:
            now = time.monotonic()
            if now >= quiet_deadline or now >= absolute_deadline:
                _kill_group(proc)
                for stream in list(streams):
                    bucket = stdout_chunks if stream is proc.stdout else stderr_chunks
                    _take(stream, bucket)
                detail = b"".join(stderr_chunks).decode("utf-8", errors="replace")[-2000:]
                raise AssertionError(f"hung step: {step}\n{detail}".rstrip())
            if not streams:
                if proc.poll() is not None:
                    break
                remaining = min(quiet_deadline, absolute_deadline) - time.monotonic()
                if remaining > 0:
                    time.sleep(min(0.25, remaining))
                continue
            timeout = max(0, min(quiet_deadline, absolute_deadline) - time.monotonic())
            try:
                readable, _, _ = select.select(streams, [], [], min(timeout, 0.25))
            except InterruptedError:
                continue
            if not readable and proc.poll() is not None:
                for stream in list(streams):
                    bucket = stdout_chunks if stream is proc.stdout else stderr_chunks
                    progressed, eof = _take(stream, bucket)
                    if progressed:
                        quiet_deadline = time.monotonic() + quiet_budget
                    if eof:
                        streams.remove(stream)
                        stream.close()
                continue
            for stream in readable:
                bucket = stdout_chunks if stream is proc.stdout else stderr_chunks
                progressed, eof = _take(stream, bucket)
                if progressed:
                    quiet_deadline = time.monotonic() + quiet_budget
                if eof:
                    streams.remove(stream)
                    stream.close()
        returncode = proc.wait()
    finally:
        for stream in (proc.stdout, proc.stderr):
            try:
                stream.close()
            except OSError:
                pass
        if proc.poll() is None:
            _kill_group(proc)
    stdout = b"".join(stdout_chunks)
    stderr = b"".join(stderr_chunks)
    if text:
        stdout = stdout.decode("utf-8", errors="replace")
        stderr = stderr.decode("utf-8", errors="replace")
    result = subprocess.CompletedProcess(argv, returncode, stdout, stderr)
    if check and returncode != 0:
        raise subprocess.CalledProcessError(returncode, argv, stdout, stderr)
    return result


class VerifiedDownloadTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=scratch, prefix="download-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        curl = self.bin / "curl"
        curl.write_text(CURL)
        curl.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        DOWNLOAD_FIXTURE=str(self.root), TMPDIR=str(self.root))
        self.destination = self.root / "archive"

    def configure(self, rows):
        (self.root / "responses.json").write_text(json.dumps(rows))
        (self.root / "calls.json").write_text("[]")

    def calls(self):
        return json.loads((self.root / "calls.json").read_text())

    def invoke(self, *, budget=60, digest=None, url=URL, deadline=False, stage="R",
               hang_quiet=None, step="verified download"):
        env = self.env
        if deadline:
            deadline_bin = self.root / "deadline-bin"
            deadline_bin.mkdir(exist_ok=True)
            launcher = deadline_bin / "python3"
            launcher.write_text(f"#!{sys.executable}\n" + DEADLINE_PYTHON)
            launcher.chmod(0o755)
            clock = self.root / "fake-clock"
            clock.write_text("0.000000\n")
            env = dict(env, PATH=f"{deadline_bin}:{env['PATH']}",
                       DOWNLOAD_EXPECT_READY=stage,
                       ORBIT_CI_DOWNLOAD_FAKE_CLOCK=str(clock))
        return run_step([str(HELPER), url, digest or hashlib.sha256(BODY).hexdigest(),
                         str(self.destination), "--budget-seconds", str(budget)],
                        env=env, text=True, step=step, hang_quiet=hang_quiet)

    def deadline_events(self, *, active_limit=2):
        events = json.loads((self.root / "deadline-events.json").read_text())
        summary = json.loads((self.root / "deadline-summary.json").read_text())
        self.assertLess(summary["active_seconds"], active_limit, summary)
        for event in events:
            if "pid" in event:
                self.assertTrue(event["reaped"], event)
        return events

    def backoffs(self, *, active_limit=2):
        # Backoff sleeps run on the virtual clock, so the delays are recorded
        # rather than waited out.
        return [event["backoff"] for event in self.deadline_events(active_limit=active_limit)
                if "backoff" in event]

    def assert_clean(self):
        self.assertEqual(list(self.root.glob(".ci-download-*")), [], "attempt bytes must be cleaned")

    def test_transient_table_recovers_with_fresh_bytes_and_backoff(self):
        rows = [(6, "000"), (7, "000"), (28, "200"),
                *((22, str(status)) for status in (429, 500, 502, 503, 504))]
        for code, status in rows:
            with self.subTest(curl_exit=code, http=status):
                self.configure([response(code, status, body=b"partial old bytes"), response()])
                result = self.invoke(deadline=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.destination.read_bytes(), BODY)
                calls = self.calls()
                self.assertEqual(len(calls), 2)
                self.assertEqual(self.backoffs(), [1])
                self.assertNotEqual(calls[0][calls[0].index("--output") + 1],
                                    calls[1][calls[1].index("--output") + 1])
                self.assert_clean()
                self.destination.unlink()

    def test_permanent_and_inconclusive_table_never_retries_or_publishes(self):
        rows = [(22, str(status)) for status in (401, 403, 404, 501, 505)]
        # Bad URL, TLS handshake/certificate, credentials, partial transfer,
        # unknown curl exit, absent status, and contradictory success/error.
        rows += [(3, "000"), (35, "000"), (60, "000"), (67, "000"),
                 (18, "200"), (99, "000"), (22, ""), (0, "000"), (0, "503"), (28, "403")]
        for code, status in rows:
            with self.subTest(curl_exit=code, http=status):
                self.configure([response(code, status), response()])
                result = self.invoke()
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(self.calls()), 1)
                self.assertFalse(self.destination.exists())
                self.assert_clean()

    def test_checksum_and_stale_partial_bytes_cannot_be_success(self):
        for rows in ([response(body=b"corrupt")],
                     [response(6, "000"), response(body=None)]):
            with self.subTest(rows=rows):
                self.configure(rows)
                result = self.invoke(deadline=True)
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(self.calls()), len(rows))
                self.assertFalse(self.destination.exists())
                self.assert_clean()
        self.configure([response()])
        self.destination.write_bytes(BODY)
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [], "an existing archive must not bypass verification")
        self.assertEqual(self.destination.read_bytes(), BODY)

    def test_invalid_public_inputs_are_refused_before_transport(self):
        for kwargs in ({"url": "http://example.invalid/x"}, {"url": "https://u:p@example.invalid/x"},
                       {"url": "https://[bad/x"}, {"digest": "bad"},
                       {"budget": 0}, {"budget": 61}, {"budget": "nan"}):
            with self.subTest(kwargs=kwargs):
                self.configure([response()])
                self.assertNotEqual(self.invoke(**kwargs).returncode, 0)
                self.assertEqual(self.calls(), [])
                self.assertFalse(self.destination.exists())

    def test_attempt_and_wall_bounds_stop_and_reap_transport(self):
        self.configure([response(6, "000")])
        result = self.invoke(deadline=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.calls()), 3)
        self.assertEqual(self.backoffs(active_limit=4), [1, 2])
        self.assertFalse(self.destination.exists())
        self.assert_clean()
        for row in (response(6, "000"), response(stall=True)):
            with self.subTest(row=row):
                self.configure([row])
                result = self.invoke(budget=0.3, deadline=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(self.calls()), 1)
                events = self.deadline_events()
                self.assertEqual(len(events), 1)
                self.assertEqual(events[0]["stage"], "R")
                if row.get("stall"):
                    self.assertTrue(events[0]["timed_out"])
                else:
                    self.assertFalse(events[0].get("timed_out", False),
                                     "insufficient backoff budget must stop after transport exits")
                self.assertFalse(self.destination.exists())
                self.assert_clean()

        # Backoff and a second transport must share the original deadline,
        # rather than each receiving a fresh per-attempt budget.
        self.configure([response(6, "000"), response(stall=True)])
        result = self.invoke(budget=1.4, deadline=True)
        self.assertNotEqual(result.returncode, 0)
        calls = self.calls()
        self.assertEqual(len(calls), 2)
        self.assertLess(float(calls[1][calls[1].index("--max-time") + 1]), 0.4)
        events = self.deadline_events()
        self.assertEqual([event.get("stage", event.get("backoff")) for event in events],
                         ["R", 1, "R"])
        self.assertLess(events[2]["timeout"], 0.4)
        self.assertTrue(events[2]["timed_out"])
        self.assertFalse(self.destination.exists())
        self.assert_clean()

    def test_cancellation_barriers_expose_incomplete_receipt_and_late_start(self):
        for mode in ("legacy", "atomic", "startup"):
            with self.subTest(mode=mode):
                rows = ([response(6, "000"), response(pause_start=True)] if mode == "startup"
                        else [response(pause_receipt=True)])
                self.configure(rows)
                # Counterfactual: the same barrier with the old in-place write
                # deterministically leaves invalid JSON when the helper kills it.
                curl = self.bin / "curl"
                curl.write_text(CURL.replace('pending = root / "calls.pending"', 'pending = calls')
                                if mode == "legacy" else CURL)
                result = self.invoke(budget=1.4 if mode == "startup" else 0.3, deadline=True,
                                     stage="RS" if mode == "startup" else "W")
                self.assertNotEqual(result.returncode, 0, result.stderr)
                if mode == "legacy":
                    with self.assertRaises(json.JSONDecodeError):
                        self.calls()
                elif mode == "startup":
                    self.assertEqual(len(self.calls()), 1, "second startup has not completed a receipt")
                else:
                    self.assertEqual(self.calls(), [], "no completed receipt before readiness")
                events = self.deadline_events()
                self.assertEqual([event.get("stage", event.get("backoff")) for event in events],
                                 ["R", 1, "S"] if mode == "startup" else ["W"])
                self.assertTrue(events[-1]["timed_out"])
                self.assertFalse(self.destination.exists())
                self.assert_clean()

    def test_curl_invocation_keeps_https_tls_and_disables_hidden_retries(self):
        self.configure([response()])
        self.assertEqual(self.invoke().returncode, 0)
        args = self.calls()[0]
        self.assertEqual(args[0], "--disable", "ignore curlrc that could weaken TLS or add retries")
        for option, value in (("--proto", "=https"), ("--proto-redir", "=https"),
                              ("--retry", "0"), ("--url", URL), ("--max-redirs", "5")):
            self.assertEqual(args[args.index(option) + 1], value)
        self.assertIn("--tlsv1.2", args)
        self.assertNotIn("--insecure", args)
        self.assertLessEqual(float(args[args.index("--max-time") + 1]), 20)

    def _historical_single_attempt(self, *, hang_quiet=None):
        # Historical public installer boundary: a single GET before checksum.
        return run_step(["bash", "-euc", 'curl -LsSf "$URL" -o "$ARCHIVE"\n'
                         'echo "$SHA  $ARCHIVE" | sha256sum -c -'],
                        env=dict(self.env, URL=URL, ARCHIVE=str(self.destination),
                                 SHA=hashlib.sha256(BODY).hexdigest()),
                        step="historical single-attempt curl", hang_quiet=hang_quiet)

    def test_old_single_attempt_cannot_recover_the_same_dns_fixture(self):
        self.configure([response(6, "000"), response()])
        result = self._historical_single_attempt()
        self.assertEqual(result.returncode, 6)
        self.assertEqual(len(self.calls()), 1)

    def _openssl_certificate(self, *, env=None, hang_quiet=None):
        cert, key = self.root / "cert.pem", self.root / "key.pem"
        config = self.root / "cert.cnf"
        # Config extensions work with both OpenSSL and macOS LibreSSL; avoid
        # depending on the newer openssl req -addext command-line option.
        config.write_text("[req]\ndistinguished_name=dn\n[dn]\nCN=127.0.0.1\n"
                          "[fixture]\nsubjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:TRUE\n")
        run_step(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                  "-keyout", str(key), "-out", str(cert), "-days", "1",
                  "-subj", "/CN=127.0.0.1", "-config", str(config), "-extensions", "fixture"],
                 check=True, env=env, step="openssl fixture certificate", hang_quiet=hang_quiet)
        return cert, key

    def _openssl_shim_env(self, sleep_seconds):
        real = shutil.which("openssl")
        self.assertIsNotNone(real, "openssl is required")
        shim_dir = self.root / f"openssl-shim-{sleep_seconds}"
        shim_dir.mkdir()
        shim = shim_dir / "openssl"
        shim.write_text("#!/bin/sh\n"
                        f"sleep {int(sleep_seconds)}\n"
                        f"exec {shlex.quote(real)} \"$@\"\n")
        shim.chmod(0o755)
        return dict(os.environ, PATH=f"{shim_dir}{os.pathsep}{os.environ['PATH']}")

    def test_real_curl_tls_http_partial_and_redirect_boundaries(self):
        cert, key = self._openssl_certificate()
        requests = []
        statuses = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def handle(self):
                try:
                    super().handle()
                except ConnectionResetError:
                    # A client rejecting the fixture CA can reset the TLS
                    # connection before sending an HTTP request.
                    pass

            def do_GET(self):
                requests.append(self.path)
                status = statuses.pop(0)
                self.send_response(status)
                if status == 302:
                    self.send_header("Location", "http://127.0.0.1/forbidden-downgrade")
                partial = self.path == "/partial"
                self.send_header("Content-Length", str(len(BODY) + (1 if partial else 0)))
                self.end_headers()
                self.wfile.write(BODY)

            def log_message(self, *args):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(cert, key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01})
        thread.start()
        try:
            # Use real curl and a fixture CA, preserving certificate/hostname
            # verification. All requests stay on this loopback TLS server.
            self.env.update(PATH=os.environ["PATH"], CURL_CA_BUNDLE=str(cert), NO_PROXY="127.0.0.1")
            endpoint = f"https://127.0.0.1:{server.server_port}"
            # Partial transfers and HTTPS downgrade redirects must refuse
            # publication regardless of the TLS backend's curl exit code.
            cases = [("/recover", [503, 200], True),
                     ("/forbidden", [403], False),
                     ("/partial", [200], False),
                     ("/redirect", [302], False)]
            for path, codes, succeeds in cases:
                with self.subTest(path=path):
                    requests.clear()
                    statuses[:] = codes
                    result = self.invoke(url=endpoint + path)
                    self.assertEqual(result.returncode == 0, succeeds, result.stderr)
                    self.assertEqual(requests, [path] * len(codes), "no extra replay or downgrade request")
                    self.assertEqual(self.destination.exists(), succeeds)
                    if succeeds:
                        self.assertEqual(self.destination.read_bytes(), BODY)
                        self.destination.unlink()
                    self.assert_clean()
            self.env.pop("CURL_CA_BUNDLE")
            requests.clear()
            result = self.invoke(url=endpoint + "/untrusted")
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(requests, [], "untrusted TLS must fail before an HTTP request")
            self.assert_clean()
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def _workflow_steps(self):
        # Several jobs install the same tools; each copy must be the one tested.
        workflow = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        steps = {}
        for job in workflow["jobs"].values():
            for step in job["steps"]:
                name = step.get("name")
                if name in steps and (name or "").startswith("Install "):
                    self.assertEqual(step, steps[name], f"every copy of {name!r} must match")
                steps.setdefault(name, step)
        return steps

    def _ensure_cargo_shim(self):
        cargo = self.bin / "cargo"
        if cargo.exists():
            return
        # Execute workflow source in an isolated checkout; public helper is real.
        cargo.write_text('#!/usr/bin/env bash\nsub="$1"; shift\nexec "$CARGO_HOME/bin/cargo-$sub" "$@"\n')
        cargo.chmod(0o755)

    def _install_case(self, steps, tool, prefix, member, mode, *, hang_quiet=None,
                      delay_seconds=None, never_answers=False):
        self._ensure_cargo_shim()
        with tempfile.TemporaryDirectory(dir=self.root, prefix=f"case-{tool}-{mode}-") as case_dir:
            case_root = Path(case_dir)
            scripts = case_root / ".orbit/tmp/workflow-ci/scripts"
            scripts.mkdir(parents=True)
            (scripts / HELPER.name).symlink_to(HELPER)
            scratch_before = sorted((case_root / ".orbit/tmp").iterdir())
            cargo_home = case_root / "cargo"
            version = {"version": "9.9.9", "version-prefix": "1.2.34"}.get(mode, "1.2.3")
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w:gz") as archive:
                content = f"#!/bin/sh\necho '{tool} {version}'\n".encode()
                info = tarfile.TarInfo(member)
                info.size, info.mode = len(content), 0o755
                archive.addfile(info, io.BytesIO(content))
            body = b"not an archive" if mode == "archive" else stream.getvalue()
            digest = hashlib.sha256(b"bad" if mode == "digest" else body).hexdigest()
            extra = {}
            if delay_seconds:
                extra["delay_seconds"] = delay_seconds
            if never_answers:
                extra["never_answers"] = True
            self.configure([response(body=body, **extra)])
            env = dict(self.env, CARGO_HOME=str(cargo_home),
                       PATH=f"{self.bin}:{cargo_home / 'bin'}:{os.environ['PATH']}",
                       TMPDIR=str(case_root))
            env.update({f"{prefix}_VERSION": "1.2.3", f"{prefix}_LINUX_SHA256": digest})
            result = run_step(["bash", "-euo", "pipefail", "-c", steps[f"Install {tool}"]["run"]],
                              cwd=case_root, env=env, text=True,
                              step=f"Install {tool}", hang_quiet=hang_quiet)
            self.assertEqual(result.returncode == 0, mode == "valid", result.stderr)
            self.assertEqual(len(self.calls()), 1, "never retry extraction, digest or version failures")
            # Archive member basename is the installed binary (rg for ripgrep).
            installed = cargo_home / "bin" / Path(member).name
            if mode in ("digest", "archive"):
                self.assertFalse(installed.exists(), "unverified bytes must never be installed")
            tmp_dir = case_root / ".orbit/tmp"
            self.assertEqual(sorted(tmp_dir.iterdir()), scratch_before,
                             "installer trap cleans both outcomes")

    def test_actual_linux_install_steps_verify_extract_and_refuse_invalid_archives_or_versions(self):
        steps = self._workflow_steps()
        tools = [("cargo-nextest", "NEXTEST", "cargo-nextest"),
                 ("cargo-deny", "CARGO_DENY", "cargo-deny-1.2.3-x86_64-unknown-linux-musl/cargo-deny"),
                 ("uv", "UV", "uv-x86_64-unknown-linux-gnu/uv"),
                 ("ripgrep", "RIPGREP", "ripgrep-1.2.3-x86_64-unknown-linux-musl/rg")]
        for tool, prefix, member in tools:
            for mode in ("valid", "digest", "archive", "version", "version-prefix"):
                with self.subTest(tool=tool, mode=mode):
                    self._install_case(steps, tool, prefix, member, mode)

    def test_silent_hang_names_the_step(self):
        # Short ceiling: startup slowness still reports the hung step, and the
        # suite does not wait out the production ceiling.
        self.configure([response(never_answers=True)])
        with self.subTest(step="verified download"):
            with self.assertRaisesRegex(AssertionError, r"hung step: verified download"):
                self.invoke(hang_quiet=2)
        with self.subTest(step="historical single-attempt curl"):
            with self.assertRaisesRegex(AssertionError, r"hung step: historical single-attempt curl"):
                self._historical_single_attempt(hang_quiet=2)
        with self.subTest(step="openssl fixture certificate"):
            with self.assertRaisesRegex(AssertionError, r"hung step: openssl fixture certificate"):
                self._openssl_certificate(env=self._openssl_shim_env(30), hang_quiet=2)
        with self.subTest(step="Install cargo-nextest"):
            with self.assertRaisesRegex(AssertionError, r"hung step: Install cargo-nextest"):
                self._install_case(self._workflow_steps(), "cargo-nextest", "NEXTEST",
                                   "cargo-nextest", "valid", hang_quiet=2, never_answers=True)

    def test_silent_hang_when_direct_child_exits_with_background_descendant(self):
        started = time.monotonic()
        cpu_start = time.process_time()
        with self.assertRaisesRegex(AssertionError, r"hung step: background descendant"):
            run_step(
                ["bash", "-c", "sleep 6 & exit 0"],
                step="background descendant",
                hang_quiet=2,
            )
        elapsed = time.monotonic() - started
        cpu_elapsed = time.process_time() - cpu_start
        self.assertLess(elapsed, 5, "hung step must fail within quiet budget, not wait out descendant")
        self.assertLess(cpu_elapsed, 1.0, "wait loop must not busy-spin once deadline has passed")

    def test_deterministic_slowdown_beyond_legacy_caps_still_passes(self):
        with self.subTest(step="verified download"):
            self.configure([response(delay_seconds=SLOW_FIXTURE_SECONDS)])
            result = self.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.destination.read_bytes(), BODY)
        with self.subTest(step="historical single-attempt curl"):
            self.configure([response(6, "000", delay_seconds=SLOW_FIXTURE_SECONDS)])
            result = self._historical_single_attempt()
            self.assertEqual(result.returncode, 6)
            self.assertEqual(len(self.calls()), 1)
        with self.subTest(step="openssl fixture certificate"):
            cert, _key = self._openssl_certificate(env=self._openssl_shim_env(SLOW_FIXTURE_SECONDS))
            self.assertGreater(cert.stat().st_size, 0)
        with self.subTest(step="Install cargo-nextest"):
            self._install_case(self._workflow_steps(), "cargo-nextest", "NEXTEST", "cargo-nextest",
                               "valid", delay_seconds=SLOW_FIXTURE_SECONDS)


if __name__ == "__main__":
    unittest.main()
