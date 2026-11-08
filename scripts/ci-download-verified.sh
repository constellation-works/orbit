#!/usr/bin/env bash
# Public CI download boundary. Python owns one monotonic deadline across curl,
# backoff and verification; curl owns TLS and HTTPS-only redirect handling.
# ORBIT_CI_DOWNLOAD_FAKE_CLOCK, when pointed at a file holding a decimal
# second count, replaces that clock. The deadline self-test advances the file
# so stop, reap, cancellation and backoff outcomes do not follow host
# scheduling. Unset, the clock is time.monotonic and backoff is time.sleep.
#
# Transient responses retry until the budget is spent; every other failure ends
# at once. The last line says which: `outcome=failed class=transient` when the
# budget ran out on transient responses, `class=permanent` otherwise. A wrong
# pin or a moved URL is the change's own failure, so a workspace declares only
# the transient line in `workflow.ci_infrastructure_signatures`.
set -euo pipefail
exec python3 - "$@" <<'PY'
import argparse
import hashlib
import math
import os
from pathlib import Path
import re
import select
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

# Test-only clock. Production leaves the variable unset and never reads it.
_FAKE_CLOCK = os.environ.get("ORBIT_CI_DOWNLOAD_FAKE_CLOCK")


def monotonic():
    if not _FAKE_CLOCK:
        return time.monotonic()
    try:
        return float(Path(_FAKE_CLOCK).read_text())
    except (OSError, ValueError) as error:
        raise ValueError("fake download clock is unreadable") from error


def _write_clock(value):
    path = Path(_FAKE_CLOCK)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(f"{value:.6f}\n")
    temporary.replace(path)


def clock_sleep(seconds):
    if _FAKE_CLOCK:
        _write_clock(monotonic() + float(seconds))
    # Deadline tests replace time.sleep with a recorder that returns without
    # a wall delay while the fake clock is set. Production calls the real sleep.
    time.sleep(seconds)


def run_transport(command, timeout):
    """Run curl until it exits or the download clock passes `timeout`.

    The real clock uses subprocess.run, which kills and reaps on expiry.
    The fake clock cannot use that wall-time timeout: the self-test advances
    time only after this process has armed `timeout`, then wakes this wait.
    A clock that jumps past the armed deadline expires the attempt without
    waiting out the budget. The numeric timeout is the same value either way.
    """
    if not _FAKE_CLOCK:
        return subprocess.run(command, stdout=subprocess.PIPE, timeout=timeout, check=False)

    arm_read, arm_write = os.pipe()
    wake_read, wake_write = os.pipe()
    os.set_inheritable(arm_read, True)
    os.set_inheritable(wake_write, True)
    env = os.environ.copy()
    env["ORBIT_CI_DOWNLOAD_CLOCK_ARM_FD"] = str(arm_read)
    env["ORBIT_CI_DOWNLOAD_CLOCK_WAKE_FD"] = str(wake_write)
    open_fds = [arm_read, arm_write, wake_read, wake_write]
    proc = None
    try:
        proc = subprocess.Popen(command, stdout=subprocess.PIPE, env=env,
                                pass_fds=(arm_read, wake_write))
        event = getattr(proc, "deadline_event", None)
        if event is not None:
            event["timeout"] = timeout
        os.close(arm_read)
        open_fds.remove(arm_read)
        os.close(wake_write)
        open_fds.remove(wake_write)
        end = monotonic() + timeout
        try:
            os.write(arm_write, f"{end:.6f}\n".encode())
        except BrokenPipeError:
            pass
        os.close(arm_write)
        open_fds.remove(arm_write)
        chunks = []
        while True:
            exited = proc.poll() is not None
            stdout_open = proc.stdout is not None and not proc.stdout.closed
            if exited and not stdout_open:
                break
            if not exited and monotonic() >= end:
                raise subprocess.TimeoutExpired(command, timeout)
            watch = []
            if stdout_open:
                watch.append(proc.stdout)
            if not exited:
                watch.append(wake_read)
            if not watch:
                break
            # Hang fuse only. The deadline above is the injected clock, not this wait.
            readable, _, _ = select.select(watch, [], [], 60)
            if proc.stdout in readable:
                data = os.read(proc.stdout.fileno(), 65536)
                if data:
                    chunks.append(data)
                else:
                    proc.stdout.close()
            if wake_read in readable:
                os.read(wake_read, 64)
            elif not readable and proc.poll() is None:
                raise subprocess.TimeoutExpired(command, timeout)
        proc.wait()
        return subprocess.CompletedProcess(command, proc.returncode, b"".join(chunks), None)
    except BaseException:
        # Same contract as subprocess.run: a failed wait must not leave curl.
        if proc is not None and proc.poll() is None:
            try:
                proc.kill()
            except ProcessLookupError:
                pass
            proc.wait()
        raise
    finally:
        for fd in open_fds:
            try:
                os.close(fd)
            except OSError:
                pass


parser = argparse.ArgumentParser(description="Download pinned HTTPS bytes with bounded transient recovery")
parser.add_argument("url")
parser.add_argument("sha256")
parser.add_argument("destination", type=Path, help="new file; existing destinations are refused")
parser.add_argument("--budget-seconds", type=float, default=60,
                    help="total download/backoff/verification budget, >0 and <=60 (default: 60)")
parser.add_argument("--cache-file", type=Path, default=None,
                    help="verified archive restored from this file when present "
                         "(SHA-256 still checked) and populated on a verified download")
args = parser.parse_args()
started = monotonic()
deadline = started + args.budget_seconds
attempts = 0
outcome = "failed"
saw_transient = False
saw_definitive = False
failure_class = "permanent"
MAX_BACKOFF_SECONDS = 5


class BudgetSpent(ValueError):
    """The shared deadline ended the download, not a response."""


def log(message):
    print(f"ci-download: {message}", file=sys.stderr, flush=True)


def remaining():
    seconds = deadline - monotonic()
    if seconds <= 0:
        raise BudgetSpent("total budget exhausted")
    return seconds


def download():
    global attempts, saw_transient, saw_definitive
    if not math.isfinite(args.budget_seconds) or not 0 < args.budget_seconds <= 60:
        raise ValueError("budget must be finite, >0 and <=60 seconds")
    url = urlsplit(args.url)
    # These setup downloads are public GETs, never credential-bearing requests.
    if (url.scheme != "https" or not url.hostname or url.username is not None
            or url.password is not None or url.fragment or url.port == 0
            or any(char.isspace() or ord(char) < 32 for char in args.url)):
        raise ValueError("expected a public HTTPS URL without credentials or fragment")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", args.sha256):
        raise ValueError("expected a pinned SHA-256 digest")
    if os.path.lexists(args.destination):
        raise ValueError("destination already exists; refusing stale bytes")
    if args.cache_file is not None and args.cache_file == args.destination:
        raise ValueError("cache file must differ from the destination")

    def verified_digest(path):
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                remaining()
                digest.update(chunk)
        return digest.hexdigest()

    # Warm path: a restored archive is published only after the same SHA-256
    # check as a download. A tampered cache entry fails closed instead of
    # falling through to the network, so corruption is never masked.
    if args.cache_file is not None and os.path.lexists(args.cache_file):
        if verified_digest(args.cache_file) != args.sha256.lower():
            raise ValueError("SHA-256 mismatch; refusing cached archive")
        remaining()
        try:
            os.link(args.cache_file, args.destination)
        except OSError:
            import shutil
            shutil.copyfile(args.cache_file, args.destination)
        log("cache=hit outcome=verified")
        return

    # Same filesystem as the destination: publish only verified bytes, without
    # overwriting an existing file. Never resume or append an earlier attempt.
    with tempfile.TemporaryDirectory(prefix=".ci-download-", dir=args.destination.parent) as scratch:
        attempt = 0
        while True:
            attempt += 1
            timeout = min(21, remaining())
            # Let curl report its explicit timeout exit before the process
            # watchdog fires. Only curl's conclusive exit 28 is retryable.
            curl_timeout = max(0.001, min(20, timeout - 0.1))
            attempts = attempt
            candidate = Path(scratch) / str(attempt)
            log(f"attempt={attempt} started remaining_seconds={remaining():.3f}")
            command = [
                "curl", "--disable", "--globoff", "--proto", "=https", "--proto-redir", "=https",
                "--tlsv1.2", "--location", "--max-redirs", "5", "--fail",
                "--silent", "--show-error", "--retry", "0",
                "--connect-timeout", str(min(10, curl_timeout)), "--max-time", str(curl_timeout),
                "--output", str(candidate), "--write-out", "%{http_code}", "--url", args.url,
            ]
            attempt_started = monotonic()
            try:
                result = run_transport(command, min(timeout, remaining()))
            except subprocess.TimeoutExpired:
                # The transport wait kills and reaps curl before returning. No
                # status means an inconclusive outcome, so do not retry it.
                log(f"attempt={attempt} outcome=process_timeout seconds={monotonic() - attempt_started:.3f}")
                raise BudgetSpent("curl process deadline exceeded") from None
            status = result.stdout.decode("ascii", errors="replace")
            log(f"attempt={attempt} curl_exit={result.returncode} http={status!r} "
                f"seconds={monotonic() - attempt_started:.3f}")
            conclusive = re.fullmatch(r"[0-9]{3}", status) is not None
            # Only explicit curl DNS/connect/timeout errors, or supported
            # transient HTTP failures. TLS, auth, partial transfers and every
            # unknown outcome fail closed. Never retry verification/install.
            transient = conclusive and (
                result.returncode in (6, 7, 28) and (status == "000" or 200 <= int(status) < 400)
                or result.returncode == 22 and int(status) in (429, 500, 502, 503, 504)
            )
            if not conclusive:
                raise ValueError("inconclusive HTTP status")
            if result.returncode == 0 and 200 <= int(status) < 300:
                # A definitive success ends transient retry: later budget
                # exhaustion during verification is the change's failure.
                saw_definitive = True
                if verified_digest(candidate) != args.sha256.lower():
                    raise ValueError("SHA-256 mismatch; refusing download")
                remaining()
                os.link(candidate, args.destination)
                # Populate the cache with the same verified bytes. A store
                # failure never fails a verified download; the next run
                # simply misses again.
                if args.cache_file is not None and not os.path.lexists(args.cache_file):
                    try:
                        args.cache_file.parent.mkdir(parents=True, exist_ok=True)
                        os.link(candidate, args.cache_file)
                        log("cache=stored")
                    except OSError as store_error:
                        log(f"cache=store_failed error={store_error}")
                log(f"attempt={attempt} outcome=verified")
                return
            if not transient:
                raise ValueError("permanent or unsupported download failure")
            # Only transient responses reach the budget check: a conclusive
            # permanent response above already raised, so the budget cannot
            # mask it as BudgetSpent.
            saw_transient = True
            remaining()
            # Growing, capped, within the same deadline: the deadline is the
            # only limit on attempts.
            delay = min(attempt, MAX_BACKOFF_SECONDS)
            if remaining() <= delay:
                raise BudgetSpent("total budget cannot accommodate backoff")
            log(f"attempt={attempt} outcome=transient retry_delay_seconds={delay}")
            clock_sleep(delay)


try:
    download()
    outcome = "verified"
except (OSError, ValueError) as error:
    log(f"failure={error}")
    if isinstance(error, BudgetSpent) and saw_transient and not saw_definitive:
        failure_class = "transient"
finally:
    detail = "" if outcome == "verified" else f" class={failure_class}"
    log(f"outcome={outcome}{detail} attempts={attempts} total_seconds={monotonic() - started:.3f} "
        f"budget_seconds={args.budget_seconds}")
sys.exit(0 if outcome == "verified" else 1)
PY
