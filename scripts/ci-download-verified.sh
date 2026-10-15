#!/usr/bin/env bash
# Public CI download boundary. Python owns one monotonic deadline across curl,
# backoff and verification; curl owns TLS and HTTPS-only redirect handling.
set -euo pipefail
exec python3 - "$@" <<'PY'
import argparse
import hashlib
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

parser = argparse.ArgumentParser(description="Download pinned HTTPS bytes with bounded transient recovery")
parser.add_argument("url")
parser.add_argument("sha256")
parser.add_argument("destination", type=Path, help="new file; existing destinations are refused")
parser.add_argument("--budget-seconds", type=float, default=60,
                    help="total download/backoff/verification budget, >0 and <=60 (default: 60)")
args = parser.parse_args()
started = time.monotonic()
deadline = started + args.budget_seconds
attempts = 0
outcome = "failed"


def log(message):
    print(f"ci-download: {message}", file=sys.stderr, flush=True)


def remaining():
    seconds = deadline - time.monotonic()
    if seconds <= 0:
        raise ValueError("total budget exhausted")
    return seconds


def download():
    global attempts
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

    # Same filesystem as the destination: publish only verified bytes, without
    # overwriting an existing file. Never resume or append an earlier attempt.
    with tempfile.TemporaryDirectory(prefix=".ci-download-", dir=args.destination.parent) as scratch:
        for attempt in range(1, 4):
            timeout = min(21, remaining())
            # Let curl report its explicit timeout exit before the process
            # watchdog fires. Only curl's conclusive exit 28 is retryable.
            curl_timeout = max(0.001, min(20, timeout - 0.1))
            attempts = attempt
            candidate = Path(scratch) / str(attempt)
            log(f"attempt={attempt}/3 started remaining_seconds={remaining():.3f}")
            command = [
                "curl", "--disable", "--globoff", "--proto", "=https", "--proto-redir", "=https",
                "--tlsv1.2", "--location", "--max-redirs", "5", "--fail",
                "--silent", "--show-error", "--retry", "0",
                "--connect-timeout", str(min(10, curl_timeout)), "--max-time", str(curl_timeout),
                "--output", str(candidate), "--write-out", "%{http_code}", "--url", args.url,
            ]
            attempt_started = time.monotonic()
            try:
                result = subprocess.run(command, stdout=subprocess.PIPE,
                                        timeout=min(timeout, remaining()), check=False)
            except subprocess.TimeoutExpired:
                # subprocess.run kills and reaps curl before returning. No
                # status means an inconclusive outcome, so do not retry it.
                log(f"attempt={attempt} outcome=process_timeout seconds={time.monotonic() - attempt_started:.3f}")
                raise ValueError("curl process deadline exceeded") from None
            status = result.stdout.decode("ascii", errors="replace")
            log(f"attempt={attempt} curl_exit={result.returncode} http={status!r} "
                f"seconds={time.monotonic() - attempt_started:.3f}")
            remaining()
            if not re.fullmatch(r"[0-9]{3}", status):
                raise ValueError("inconclusive HTTP status")
            if result.returncode == 0 and 200 <= int(status) < 300:
                digest = hashlib.sha256()
                with candidate.open("rb") as stream:
                    while chunk := stream.read(1024 * 1024):
                        remaining()
                        digest.update(chunk)
                if digest.hexdigest() != args.sha256.lower():
                    raise ValueError("SHA-256 mismatch; refusing download")
                remaining()
                os.link(candidate, args.destination)
                log(f"attempt={attempt} outcome=verified")
                return
            # Only explicit curl DNS/connect/timeout errors, or supported
            # transient HTTP failures. TLS, auth, partial transfers and every
            # unknown outcome fail closed. Never retry verification/install.
            transient = (
                result.returncode in (6, 7, 28) and (status == "000" or 200 <= int(status) < 400)
                or result.returncode == 22 and int(status) in (429, 500, 502, 503, 504)
            )
            if not transient:
                raise ValueError("permanent or unsupported download failure")
            if attempt == 3:
                raise ValueError("attempt limit exhausted")
            delay = attempt  # one and two seconds, within the same deadline
            if remaining() <= delay:
                raise ValueError("total budget cannot accommodate backoff")
            log(f"attempt={attempt} outcome=transient retry_delay_seconds={delay}")
            time.sleep(delay)


try:
    download()
    outcome = "verified"
except (OSError, ValueError) as error:
    log(f"failure={error}")
finally:
    log(f"outcome={outcome} attempts={attempts} total_seconds={time.monotonic() - started:.3f} "
        f"budget_seconds={args.budget_seconds}")
sys.exit(0 if outcome == "verified" else 1)
PY
