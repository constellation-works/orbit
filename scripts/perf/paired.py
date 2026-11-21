#!/usr/bin/env python3
"""Paired base/candidate/control timing of one Orbit surface on a frozen fixture.

    paired.py --builds <builds-dir> --snapshot <snapshot-dir> --out <results-dir>
              (--cli '<orbit args>' | --http '<GET path>')
              [--parity '<orbit args>' --parity-mask <field>...]
              [--rounds 3] [--samples 10] [--warmup 1] [--gap-ms 250] [--bound-ms <n>]
              [--command-timeout 300] [--output-limit-bytes 16777216]

Three arms: the base and candidate binaries from build-pair.sh, and a control
that is the base binary copied to a second path, so the unchanged-build spread
is measured. Every round restores a fresh root from the snapshot. Holds the
host-wide perf lane for the whole run. A match requires a successful
observation from every arm (parity command exit 0, and HTTP 2xx when that
probe runs). A missing or failed observation refuses the run before sampling.
Every arm command runs in its own process group with a deadline and a stdout
cap; a stalled or flooding command is killed with its group and refuses the
run. A parity command that hits a bound is that arm's failed observation, so
parity.json still names the arm and bound. SIGTERM, SIGHUP or SIGINT stops
every child, `web serve` included.
See docs/runbooks/perf-timing-protocol.md.
"""

from __future__ import annotations

import argparse
import contextlib
import http.client
import importlib
import json
import math
import os
import re
import selectors
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# Loaded by name after the insert: `python3 -I` keeps this directory off sys.path.
sys.path.insert(0, str(Path(__file__).resolve().parent))
fixture = importlib.import_module("fixture")
common = importlib.import_module("perf_common")

SCHEMA_VERSION = 1
ARMS = ("base", "candidate", "control")
STARTUP_TIMEOUT_SECONDS = 60.0
HTTP_TIMEOUT_SECONDS = 300.0
# Every arm command (parity or sample) is a supervised child with a deadline and
# a stdout cap (STD-03 §R11–R15, §R22). Both are recorded in parity.json and
# paired-summary.json.
DEFAULT_COMMAND_TIMEOUT_SECONDS = 300.0
DEFAULT_OUTPUT_LIMIT_BYTES = 16 * 1024 * 1024
TERM_GRACE_SECONDS = 5.0
POST_EXIT_DRAIN_SECONDS = 5.0
# Keep epoll's millisecond timeout conversion in range when the command
# deadline is much farther away. pidfd readiness still wakes the selector as
# soon as the child exits.
SELECTOR_WAIT_SLICE_SECONDS = 1.0


# ── statistics ───────────────────────────────────────────────────────────────


def nearest_rank_p95(values: list[float]) -> float:
    """Nearest-rank p95 (ORB-25638): the ceil(0.95 n)-th smallest sample."""
    ordered = sorted(values)
    rank = max(1, math.ceil(0.95 * len(ordered)))
    return ordered[rank - 1]


def round_order(rounds: int) -> list[str]:
    """Balanced interleaving: forward, then reversed, alternating per round.

    With two arms over three rounds this is ORB-25652's A B B A A B.
    """
    order: list[str] = []
    for index in range(rounds):
        order.extend(ARMS if index % 2 == 0 else tuple(reversed(ARMS)))
    return order


def verdict(candidate: float, spread: float, bound: float | None) -> dict:
    if bound is None:
        return {"verdict": "informational", "reason": "no --bound-ms given"}
    if candidate > bound:
        return {"verdict": "fail", "reason": f"candidate median {candidate:.1f} ms > bound {bound:g} ms"}
    if candidate <= bound - spread:
        return {"verdict": "pass",
                "reason": f"candidate median {candidate:.1f} ms <= bound {bound:g} - control spread {spread:.1f} ms"}
    return {"verdict": "inconclusive",
            "reason": (f"candidate median {candidate:.1f} ms is within the control spread "
                       f"{spread:.1f} ms of the bound {bound:g} ms; rerun with more --rounds")}


# ── host pressure and network ────────────────────────────────────────────────


def orbit_leaf_count() -> int:
    """Processes carrying Orbit's managed-run marker, outside this harness."""
    own = {os.getpid(), os.getppid()}
    count = 0
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) in own:
            continue
        try:
            environ = (entry / "environ").read_bytes()
        except OSError:
            continue
        if b"ORBIT_MANAGED_RUN_CONTEXT=" in environ:
            count += 1
    return count


def host_pressure() -> dict:
    try:
        loadavg = Path("/proc/loadavg").read_text().strip()
    except OSError:
        loadavg = ""
    return {"at": common.now_iso(), "loadavg": loadavg, "orbit_leaf_processes": orbit_leaf_count()}


def network_isolation() -> tuple[list[str], dict]:
    """A prefix that runs a command without network, when the host allows it."""
    for prefix in (["unshare", "--map-current-user", "--net"], ["unshare", "-rn"]):
        try:
            probe = run_supervised([*prefix, "true"], cwd=None, env=None,
                                   timeout=10, output_limit=4096, capture_stderr=True)
        except OSError as error:
            last = str(error)
            continue
        if probe.ended == "exit" and probe.returncode == 0:
            return prefix, {"isolated": True, "method": " ".join(prefix)}
        last = probe.describe() if probe.ended != "exit" else \
            probe.stdout.decode("utf-8", "replace").strip()
    return [], {"isolated": False, "reason": f"unshare unavailable: {last}"}


# ── processes ────────────────────────────────────────────────────────────────


class HarnessCancelled(BaseException):
    """The harness got SIGTERM or SIGHUP; `finally` blocks stop every child."""

    def __init__(self, signum: int):
        super().__init__(signal.Signals(signum).name)
        self.signum = signum


def install_cancel_handlers() -> None:
    """Turn SIGTERM and SIGHUP into `HarnessCancelled` (SIGINT already raises).

    Unwinding runs every child's stop and the lane release, so cancelling the
    harness also stops its arm commands and `web serve` groups (STD-03 §R12).
    A repeated signal during that cleanup is ignored rather than cutting it short.
    """
    state = {"cancelled": False}

    def handler(signum, _frame):
        if state["cancelled"]:
            return
        state["cancelled"] = True
        raise HarnessCancelled(signum)

    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, handler)


@contextlib.contextmanager
def defer_spawn_cancellation():
    """Delay cancellation until the caller has acquired the Popen object.

    Mask signals only while swapping handlers: the child must inherit the
    original signal mask, so it can respond to group termination.
    """
    signals = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
    previous = {}
    pending = []

    def defer(signum, _frame):
        if not pending:
            pending.append(signum)

    old_mask = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
    try:
        for signum in signals:
            previous[signum] = signal.signal(signum, defer)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
    try:
        yield
    finally:
        old_mask = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
        try:
            for signum, handler in previous.items():
                signal.signal(signum, handler)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
        if pending:
            os.kill(os.getpid(), pending[0])


def leader_exited(pid: int) -> bool:
    """Whether our child `pid` exited, without reaping it.

    `WNOWAIT` leaves an exited leader as a zombie, which pins its process
    group id until it is reaped, so a group signal never lands on a reused id
    (STD-03 §R14).
    """
    try:
        info = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
    except ChildProcessError:
        return True
    return info is not None


def group_alive(pgid: int) -> bool:
    """Whether any non-zombie process may still be running in `pgid` (STD-03 §R12).

    Only an explicit kernel absence (`ESRCH`) or a complete `/proc` scan showing
    only zombie members proves the group has exited; any uncertain or unreadable
    probe counts as alive so the grace wait and final SIGKILL are never skipped.
    """
    if pgid <= 1:
        return False
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    try:
        with os.scandir("/proc") as entries:
            pid_names = [entry.name for entry in entries if entry.name.isdigit()]
    except OSError:
        return True
    found_member = False
    unknown_member = False
    for name in pid_names:
        try:
            with open(f"/proc/{name}/stat", "r", encoding="utf-8", errors="replace") as handle:
                stat = handle.read()
        except (FileNotFoundError, ProcessLookupError):
            continue
        except OSError:
            unknown_member = True
            continue
        right_paren = stat.rfind(")")
        if right_paren == -1:
            unknown_member = True
            continue
        fields = stat[right_paren + 1 :].split()
        if len(fields) < 3:
            unknown_member = True
            continue
        try:
            member_pgid = int(fields[2])
        except ValueError:
            unknown_member = True
            continue
        if member_pgid != pgid:
            continue
        found_member = True
        if fields[0] not in ("Z", "X"):
            return True
    if unknown_member:
        return True
    if found_member:
        return False
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def stop_group(process: subprocess.Popen, grace: float = TERM_GRACE_SECONDS) -> None:
    """SIGTERM the child's group, wait a bounded grace, SIGKILL the group, reap.

    Must run while the leader is still unreaped, so its zombie pins the pgid
    across the grace wait and final SIGKILL (§R12, §R14). Survival is probed
    at the group level rather than inferred from the leader's exit.
    """
    try:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + grace
        while group_alive(process.pid) and time.monotonic() < deadline:
            time.sleep(0.02)
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=10)


class SupervisedResult:
    """How one supervised command ended (STD-03 §R13)."""

    def __init__(self, ended: str, returncode: int | None, stdout: bytes,
                 elapsed_ms: float, timeout: float, output_limit: int):
        self.ended = ended  # "exit" | "timeout" | "output_limit"
        self.returncode = returncode
        self.stdout = stdout
        self.elapsed_ms = elapsed_ms
        self.timeout = timeout
        self.output_limit = output_limit

    def describe(self) -> str:
        if self.ended == "timeout":
            return f"timed out after {self.timeout:g} s; its process group was killed"
        if self.ended == "output_limit":
            return (f"wrote more than {self.output_limit} bytes of stdout; "
                    "its process group was killed")
        if self.returncode is not None and self.returncode < 0:
            return f"was killed by signal {-self.returncode}"
        return f"exited {self.returncode}"


def run_supervised(argv: list[str], *, cwd, env, timeout: float, output_limit: int,
                   capture_stderr: bool = False) -> SupervisedResult:
    """Run `argv` as its own process-group leader, draining stdout while waiting.

    The command has a wall-clock deadline and a stdout byte cap; reaching
    either SIGTERMs, then SIGKILLs, its whole group. A clean exit still sweeps
    the group, so a descendant left behind (one holding stdout open, say) is
    killed. Every path reaps the leader before returning or raising.
    `elapsed_ms` runs from spawn to the leader's exit.
    """
    started = time.perf_counter()
    process = None
    stdout = None
    try:
        with defer_spawn_cancellation():
            process = subprocess.Popen(
                argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT if capture_stderr else subprocess.DEVNULL,
                start_new_session=True,
            )
        pending = bytearray()
        ended = "exit"
        exited_at: float | None = None
        stdout = process.stdout
        assert stdout is not None
        try:
            pidfd = os.pidfd_open(process.pid)
        except (AttributeError, OSError):
            pidfd = None
        try:
            deadline = time.monotonic() + timeout
            drain_until: float | None = None
            open_stdout = True
            with selectors.DefaultSelector() as selector:
                selector.register(stdout, selectors.EVENT_READ, "stdout")
                if pidfd is not None:
                    selector.register(pidfd, selectors.EVENT_READ, "exit")
                while True:
                    now = time.monotonic()
                    if exited_at is None and leader_exited(process.pid):
                        exited_at = time.perf_counter()
                        # Descendants could keep stdout open; sweep them now,
                        # while the zombie leader still pins the group id.
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        drain_until = now + POST_EXIT_DRAIN_SECONDS
                        if pidfd is not None:
                            selector.unregister(pidfd)
                    if exited_at is not None and (not open_stdout or now >= drain_until):
                        break
                    if exited_at is None and now >= deadline:
                        ended = "timeout"
                        break
                    limit = drain_until if exited_at is not None else deadline
                    # Without a pidfd, poll the leader every 10 ms.
                    wait = min(
                        limit - now,
                        0.01 if pidfd is None else SELECTOR_WAIT_SLICE_SECONDS,
                    )
                    for key, _mask in selector.select(max(wait, 0)):
                        if key.data != "stdout":
                            continue
                        chunk = os.read(stdout.fileno(), 65536)
                        if not chunk:
                            selector.unregister(stdout)
                            open_stdout = False
                            continue
                        pending.extend(chunk)
                        if len(pending) > output_limit:
                            ended = "output_limit"
                    if ended == "output_limit":
                        break
        finally:
            if pidfd is not None:
                os.close(pidfd)
        if ended != "exit" or exited_at is None:
            stop_group(process)
        else:
            process.wait(timeout=10)
    except BaseException:
        if process is not None and process.returncode is None:
            stop_group(process)
        raise
    finally:
        if stdout is not None:
            stdout.close()
        elif process is not None and process.stdout is not None:
            process.stdout.close()
    finish = exited_at if exited_at is not None and ended == "exit" else time.perf_counter()
    return SupervisedResult(ended, process.returncode if ended == "exit" else None,
                      bytes(pending[:output_limit]), (finish - started) * 1000,
                      timeout, output_limit)


class HttpRequestError(Exception):
    """An arm HTTP server failed to return a complete HTTP response."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """Keep a redirect response without parsing or requesting its Location."""

    def _keep_response(self, req, fp, code, msg, headers):
        return None

    http_error_301 = _keep_response
    http_error_302 = _keep_response
    http_error_303 = _keep_response
    http_error_307 = _keep_response
    http_error_308 = _keep_response


class Server:
    """One arm's `orbit web serve` on its restored root (after ORB-25672)."""

    def __init__(self, binary: Path, info: dict, env: dict):
        self.opener = urllib.request.build_opener(NoRedirect())
        self.binary = binary
        self.info = info
        self.env = env
        self.process = None

    def start(self) -> None:
        binary, info, env = self.binary, self.info, self.env
        try:
            with defer_spawn_cancellation():
                self.process = subprocess.Popen(
                    [str(binary), "web", "serve", "--no-open", "--port", "0",
                     "--workspace", info["workspace"]],
                    cwd=info["cwd"], env=env,
                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                    start_new_session=True,
                )
            line = self._announcement(STARTUP_TIMEOUT_SECONDS)
            match = re.search(r"http://(\S+)", line)
            if not match:
                raise RuntimeError(f"web serve did not announce its address: {line!r}")
            self.base = "http://" + match.group(1).rstrip("/")
            self._wait_ready()
        except BaseException:
            if self.process is not None:
                self.stop()
            raise

    def _announcement(self, timeout: float) -> str:
        deadline = time.monotonic() + timeout
        pending = bytearray()
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("web serve did not announce its address in time")
                if selector.select(min(remaining, 0.1)):
                    chunk = os.read(self.process.stdout.fileno(), 4096)
                    pending.extend(chunk)
                    if b"\n" in pending:
                        return pending.split(b"\n", 1)[0].decode("utf-8", "replace")
                    if not chunk:
                        raise RuntimeError("web serve closed stdout before announcing")
                if self._leader_exited():
                    raise RuntimeError("web serve exited before announcing its address")

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + STARTUP_TIMEOUT_SECONDS
        while True:
            try:
                with urllib.request.urlopen(self.base + "/", timeout=10) as response:
                    response.read()
                    return
            except urllib.error.HTTPError:
                return  # the server answered
            except (urllib.error.URLError, OSError):
                if time.monotonic() >= deadline:
                    raise RuntimeError("web serve never answered on its address")
                time.sleep(0.1)

    def get(self, path: str) -> tuple[int, bytes]:
        try:
            try:
                with self.opener.open(self.base + path, timeout=HTTP_TIMEOUT_SECONDS) as response:
                    return response.status, response.read()
            except urllib.error.HTTPError as error:
                with error:
                    return error.code, error.read()
        except (urllib.error.URLError, http.client.HTTPException, OSError) as error:
            raise HttpRequestError(f"{type(error).__name__}: {error}") from error

    def _leader_exited(self) -> bool:
        return leader_exited(self.process.pid)

    def stop(self) -> None:
        if self.process is None or getattr(self, "_stopped", False):
            return
        self._stopped = True
        try:
            stop_group(self.process, grace=10)
        finally:
            if self.process.stdout is not None:
                self.process.stdout.close()


# ── parity ───────────────────────────────────────────────────────────────────


def mask_value(value, masks: list[str], path: str = ""):
    if isinstance(value, dict):
        return {
            key: mask_value(item, masks, f"{path}.{key}" if path else key)
            for key, item in sorted(value.items())
            if key not in masks and (f"{path}.{key}" if path else key) not in masks
        }
    if isinstance(value, list):
        return [mask_value(item, masks, path) for item in value]
    return value


def normalize_output(raw: bytes, masks: list[str], root: str) -> dict:
    text = raw.decode("utf-8", "replace").replace(root, "<root>")
    try:
        parsed = json.loads(text)
    except ValueError:
        return {"format": "text", "value": text}
    return {"format": "json", "value": mask_value(parsed, masks)}


def arm_observation_failure(record: dict) -> str | None:
    """Why `record` is not a successful observation, or None when it is.

    A match needs one from every arm. CLI mode with no `--parity` stores an
    empty record: that is a missing observation, not an agreement. A parity
    command must exit 0, and an HTTP probe must return a 2xx status. Identical
    non-zero exits are a failed probe even when stdout is empty on every arm.
    """
    if "command" not in record and "http" not in record:
        return "missing observation"
    reasons: list[str] = []
    if "command" in record:
        command = record["command"]
        if "stopped" in command:
            reasons.append(command["stopped"])
        elif command.get("exit") != 0:
            reasons.append(f"command exited {command.get('exit')}")
    if "http" in record:
        if "error" in record["http"]:
            reasons.append(f"http error: {record['http']['error']}")
        else:
            status = record["http"].get("status")
            if not isinstance(status, int) or status < 200 or status >= 300:
                reasons.append(f"http status {status}")
    return "; ".join(reasons) if reasons else None


def parity_refusal_detail(parity: dict) -> str:
    """The parenthetical of a parity refusal, covering failures and differences."""
    parts: list[str] = []
    failures = parity.get("observation_failures") or {}
    if failures:
        rendered = ", ".join(f"{arm}: {failures[arm]}" for arm in ARMS if arm in failures)
        parts.append(f"no successful observation ({rendered})")
    if parity["differing_arms"]:
        parts.append(f"{', '.join(parity['differing_arms'])} differ from base")
    return "; ".join(parts) if parts else "match is false"


# ── run ──────────────────────────────────────────────────────────────────────


class Harness:
    def __init__(self, args):
        self.args = args
        self.builds = Path(args.builds).absolute()
        self.snapshot = Path(args.snapshot).absolute()
        self.out = Path(args.out).absolute()
        self.masks = list(args.parity_mask or [])
        self.cli_args = shlex.split(args.cli) if args.cli else None
        self.http_path = args.http
        self.qualifying_task_counts = dict(args.qualifying_task_count or [])
        self.reset_info = None

    def prepare(self, created: common.CreatedDirs) -> None:
        provenance = json.loads((self.builds / "provenance.json").read_text())
        self.provenance = provenance
        self.binaries: dict[str, Path] = {}
        for role in ("base", "candidate"):
            entry = provenance["revisions"][role]
            path = self.builds / entry["binary"]
            if common.sha256_file(path) != entry["binary_sha256"]:
                raise common.PerfRefusal(f"{path} does not match provenance.json sha256")
            self.binaries[role] = path
        self.manifest = fixture.verify_snapshot(self.snapshot)
        common.require_absent_or_empty(self.out)
        created.create(self.out, output=True)
        self.work = created.create(self.out / ".work")
        control = self.work / "control" / "orbit"
        control.parent.mkdir()
        shutil.copy2(self.binaries["base"], control)
        self.binaries["control"] = control
        self.binary_sha = {arm: common.sha256_file(path) for arm, path in self.binaries.items()}
        if self.binary_sha["control"] != self.binary_sha["base"]:
            raise common.PerfRefusal("control copy differs from the base binary")
        if self.cli_args is not None:
            self.net_prefix, self.network = network_isolation()
        else:
            self.net_prefix, self.network = [], {
                "isolated": False,
                "reason": "HTTP mode: the timed server listens on host loopback for the "
                          "harness client, so it stays in the host network namespace",
            }

    @staticmethod
    def _sync_tree(src: Path, dest: Path, *extra_args: str) -> None:
        try:
            subprocess.run(["rsync", "-aX", *extra_args, "--",
                            f"{src}/", f"{dest}/"], check=True)
        except (OSError, subprocess.CalledProcessError) as error:
            raise common.PerfRefusal(f"fixture reset failed: {error}") from error

    def restore(self, label: str) -> dict:
        if self.args.keep:
            return fixture.restore_root(self.snapshot, self.work / f"root-{label}", self.manifest)
        root = self.work / "root"
        template = self.work / "pristine"
        if self.reset_info is None:
            self.reset_info = fixture.restore_root(self.snapshot, root, self.manifest)
            # The populated root can carry the filesystem to the gate threshold.
            # restore_root gates only the root it creates, so gate the pristine
            # copy before its full tree is written. Copy extended attributes too
            # so resets preserve whatever a full restore produced.
            common.disk_gate(template)
            self._sync_tree(root, template)
        else:
            # Checksums detect equal-size, equal-mtime changes made by an arm;
            # deletion removes newly created files, and -X clears or restores
            # extended attributes left by an earlier arm.
            self._sync_tree(template, root, "--checksum", "--delete")
        return self.reset_info

    def drop(self, info: dict) -> None:
        # A successful observation reuses the root; an interrupted one cleans it
        # after its child has been reaped. --keep retains each labelled root.
        if not self.args.keep and sys.exc_info()[0] is not None:
            fixture.clean_root(Path(info["root"]))

    def limits(self) -> dict:
        return {"command_timeout_seconds": self.args.command_timeout,
                "output_limit_bytes": self.args.output_limit_bytes}

    def supervised_arm(self, arm: str, info: dict, env: dict, argv: list[str]) -> SupervisedResult:
        return run_supervised(
            [*self.net_prefix, str(self.binaries[arm]), *argv],
            cwd=info["cwd"], env=env,
            timeout=self.args.command_timeout, output_limit=self.args.output_limit_bytes,
        )

    @staticmethod
    def bound_reason(result: SupervisedResult) -> str:
        """How a bounded command ended, naming the flag that set the bound it hit."""
        flag = "--command-timeout" if result.ended == "timeout" else "--output-limit-bytes"
        return f"{result.describe()} ({flag})"

    def cli_once(self, arm: str, info: dict, env: dict, argv: list[str],
                 what: str) -> tuple[float, int, bytes]:
        """One supervised sample; a timeout or output overflow refuses the run."""
        result = self.supervised_arm(arm, info, env, argv)
        if result.ended != "exit":
            raise common.PerfRefusal(f"{arm} {what} {self.bound_reason(result)}; failing the run")
        return result.elapsed_ms, result.returncode, result.stdout

    def parity_command(self, arm: str, info: dict, env: dict) -> dict:
        """The parity command's record for one arm.

        A bound hit is that arm's failed observation, not an abort: parity() still
        writes parity.json, which names the arm, the bound and match false (ORB-26413).
        """
        result = self.supervised_arm(arm, info, env, shlex.split(self.args.parity))
        if result.ended != "exit":
            if result.ended == "timeout":
                bound = {"name": "command_timeout_seconds", "value": self.args.command_timeout}
            else:
                bound = {"name": "output_limit_bytes", "value": self.args.output_limit_bytes}
            return {"exit": None, "stopped": self.bound_reason(result), "bound": bound}
        return {"exit": result.returncode,
                "output": normalize_output(result.stdout, self.masks, info["root"])}

    def parity(self) -> dict:
        arms = {}
        for arm in ARMS:
            info = self.restore(f"parity-{arm}")
            env = fixture.isolated_env(info)
            record: dict = {}
            try:
                if self.args.parity is not None:
                    record["command"] = self.parity_command(arm, info, env)
                if self.http_path:
                    server = Server(self.binaries[arm], info, env)
                    try:
                        server.start()
                        try:
                            status, body = server.get(self.http_path)
                        except HttpRequestError as error:
                            record["http"] = {"error": str(error)}
                        else:
                            record["http"] = {"status": status,
                                              "body": normalize_output(body, self.masks, info["root"])}
                    finally:
                        server.stop()
            finally:
                self.drop(info)
            arms[arm] = record
        failures = {}
        for arm in ARMS:
            reason = arm_observation_failure(arms[arm])
            if reason:
                failures[arm] = reason
        reference = json.dumps(arms["base"], sort_keys=True)
        differing = [arm for arm in ARMS if json.dumps(arms[arm], sort_keys=True) != reference]
        # Identical failures compare equal, so content equality alone is not a match.
        return {
            "schemaVersion": SCHEMA_VERSION,
            "kind": "orbit-perf-parity",
            "command": self.args.parity,
            "http_path": self.http_path,
            "masks": self.masks,
            "limits": self.limits(),
            "match": not failures and not differing,
            "differing_arms": differing,
            "observation_failures": failures,
            "arms": arms,
        }

    def sample_round(self, arm: str, round_index: int) -> tuple[list[dict], dict]:
        info = self.restore(f"{arm}-{round_index}")
        env = fixture.isolated_env(info)
        before = host_pressure()
        samples: list[dict] = []
        gap = self.args.gap_ms / 1000
        try:
            if self.cli_args is not None:
                server = None

                def timed():
                    return self.cli_once(arm, info, env, self.cli_args,
                                         f"round {round_index} sample {len(samples)}")[:2]
            else:
                server = Server(self.binaries[arm], info, env)

                def timed():
                    started = time.perf_counter()
                    try:
                        status, _body = server.get(self.http_path)
                    except HttpRequestError as error:
                        raise common.PerfRefusal(
                            f"{arm} round {round_index} sample {len(samples)} failed: {error}; "
                            "failing the run"
                        ) from error
                    ms = (time.perf_counter() - started) * 1000
                    return ms, 0 if 200 <= status < 300 else status
            try:
                if server is not None:
                    server.start()
                total = self.args.warmup + self.args.samples
                for index in range(total):
                    if index:
                        time.sleep(gap)
                    ms, code = timed()
                    warm = index < self.args.warmup
                    samples.append({"arm": arm, "round": round_index, "index": index,
                                    "warmup": warm, "ms": round(ms, 3), "exit": code})
                    if code != 0:
                        raise common.PerfRefusal(
                            f"{arm} round {round_index} sample {index} exited {code}; failing the run")
            finally:
                if server is not None:
                    server.stop()
        finally:
            after = host_pressure()
            self.drop(info)
        return samples, {"arm": arm, "round": round_index, "before": before, "after": after}

    def run(self) -> int:
        manifest_summary_path = self.out / fixture.MANIFEST_SUMMARY
        # Size the encoded text before writing it: a refused summary must never sit
        # at the evidence path, and --keep retains the output directory after a refusal.
        manifest_summary_text = common.json_text(
            fixture.manifest_evidence_summary(
                self.snapshot,
                self.manifest,
                {role: self.provenance["revisions"][role]["commit"]
                 for role in ("base", "candidate")},
                self.qualifying_task_counts,
            ),
        )
        encoded_bytes = len(manifest_summary_text.encode("utf-8"))
        if encoded_bytes > fixture.MAX_ARTIFACT_BYTES:
            raise common.PerfRefusal(
                f"{manifest_summary_path} would be {encoded_bytes} bytes, over the "
                f"{fixture.MAX_ARTIFACT_BYTES}-byte artifact limit; nothing was written. "
                "Shorten the --qualifying-task-count names to fit")
        manifest_summary_path.write_text(manifest_summary_text)
        parity = self.parity()
        common.write_json(self.out / "parity.json", parity)
        if not parity["match"]:
            raise common.PerfRefusal(
                "parity mismatch between arms "
                f"({parity_refusal_detail(parity)}); refusing to sample. "
                f"See {self.out / 'parity.json'}")
        order = round_order(self.args.rounds)
        seen = {arm: 0 for arm in ARMS}
        raw: list[dict] = []
        pressure: list[dict] = []
        for arm in order:
            samples, reading = self.sample_round(arm, seen[arm])
            seen[arm] += 1
            raw.extend(samples)
            pressure.append(reading)
        round_p95: dict[str, list[float]] = {arm: [] for arm in ARMS}
        for arm in ARMS:
            for index in range(self.args.rounds):
                values = [s["ms"] for s in raw
                          if s["arm"] == arm and s["round"] == index and not s["warmup"]]
                round_p95[arm].append(nearest_rank_p95(values))
        medians = {arm: statistics.median(values) for arm, values in round_p95.items()}
        control_range = max(round_p95["control"]) - min(round_p95["control"])
        control_offset = abs(medians["control"] - medians["base"])
        spread = max(control_range, control_offset)
        outcome = verdict(medians["candidate"], spread, self.args.bound_ms)
        surface = {"mode": "cli", "args": self.args.cli} if self.cli_args is not None \
            else {"mode": "http", "path": self.http_path}
        common.write_json(self.out / "raw.json", {
            "schemaVersion": SCHEMA_VERSION,
            "kind": "orbit-perf-raw",
            "surface": surface,
            "samples": raw,
        })
        summary = {
            "schemaVersion": SCHEMA_VERSION,
            "kind": "orbit-perf-paired-summary",
            "surface": surface,
            "protocol": {"rounds": self.args.rounds, "samples": self.args.samples,
                         "warmup": self.args.warmup, "gap_ms": self.args.gap_ms,
                         "aggregation": "per-round nearest-rank p95; per-arm median of round p95s",
                         **self.limits()},
            "round_order": order,
            "revisions": {role: {k: self.provenance["revisions"][role][k]
                                 for k in ("commit", "tree", "binary_sha256")}
                          for role in ("base", "candidate")},
            "binary_sha256": self.binary_sha,
            "snapshot": {"path": str(self.snapshot),
                         "snapshot_epoch": self.manifest["snapshot_epoch"],
                         "source_workspace": self.manifest["source"]["registry_id"],
                         "manifest_summary": str(manifest_summary_path)},
            "round_p95_ms": round_p95,
            "median_p95_ms": medians,
            "control_spread_ms": {"value": spread, "control_range": control_range,
                                  "control_vs_base": control_offset},
            "candidate_minus_base_ms": medians["candidate"] - medians["base"],
            "bound_ms": self.args.bound_ms,
            **outcome,
            "network": self.network,
            "host_pressure": pressure,
            "lane": self.lane_holder,
            **common.host_identity(),
        }
        common.write_json(self.out / "paired-summary.json", summary)
        print(json.dumps({"verdict": summary["verdict"], "median_p95_ms": medians,
                          "control_spread_ms": spread, "results": str(self.out)}))
        return 0


def positive(value: str) -> int:
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def positive_float(value: str) -> float:
    number = float(value)
    if not number > 0 or math.isinf(number):
        raise argparse.ArgumentTypeError("must be a finite number above 0")
    return number


def named_count(value: str) -> tuple[str, int]:
    name, separator, raw_count = value.rpartition("=")
    if not separator or not name.strip():
        raise argparse.ArgumentTypeError("expected NAME=COUNT")
    try:
        count = int(raw_count)
    except ValueError:
        raise argparse.ArgumentTypeError("COUNT must be a non-negative integer") from None
    if count < 0:
        raise argparse.ArgumentTypeError("COUNT must be a non-negative integer")
    return name, count


def parity_command_value(value: str) -> str:
    """A --parity value must be one shell-quoted orbit command (ORB-26398, ORB-26439).

    An omitted flag never reaches this check (argparse leaves it None), so an explicit
    value that splits to no words is refused rather than standing in for no command.
    """
    try:
        words = shlex.split(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            f"not a valid shell-quoted orbit command ({error}); check the quotes, "
            "e.g. 'doctor --json'") from None
    if not words:
        raise argparse.ArgumentTypeError(
            "takes an orbit command; leave it out to skip the parity command")
    return value


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--builds", required=True, help="build-pair.sh output directory")
    parser.add_argument("--snapshot", required=True, help="fixture.py snapshot directory")
    parser.add_argument("--out", required=True, help="new results directory")
    surface = parser.add_mutually_exclusive_group(required=True)
    surface.add_argument("--cli", help="orbit arguments to time, e.g. 'doctor --json'")
    surface.add_argument("--http", help="GET path to time on each arm's own web serve")
    parser.add_argument(
        "--parity", type=parity_command_value,
        help="orbit arguments run once per arm before sampling; a CLI match needs "
             "this command to exit 0 on every arm")
    parser.add_argument("--parity-mask", nargs="+", default=[],
                        help="JSON field names (or dotted paths) removed before comparing")
    parser.add_argument("--rounds", type=positive, default=3)
    parser.add_argument("--samples", type=positive, default=10)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--gap-ms", type=int, default=250)
    parser.add_argument("--bound-ms", type=float)
    parser.add_argument("--qualifying-task-count", action="append", type=named_count,
                        default=[], metavar="NAME=COUNT",
                        help="record one checked fixture qualifying-task count; repeat as needed")
    parser.add_argument("--lane-wait", type=float,
                        default=float(os.environ.get("ORBIT_PERF_LANE_WAIT_SECONDS",
                                                     common.DEFAULT_LANE_WAIT_SECONDS)))
    parser.add_argument("--command-timeout", type=positive_float,
                        default=DEFAULT_COMMAND_TIMEOUT_SECONDS,
                        help="seconds one parity or CLI sample command may run before its "
                             "process group is killed and the run refused")
    parser.add_argument("--output-limit-bytes", type=positive, default=DEFAULT_OUTPUT_LIMIT_BYTES,
                        help="stdout bytes one parity or CLI sample command may write before "
                             "its process group is killed and the run refused")
    parser.add_argument("--keep", action="store_true", help="keep restored roots and work dirs")
    args = parser.parse_args(argv)
    count_names = [name for name, _count in args.qualifying_task_count]
    if len(count_names) != len(set(count_names)):
        parser.error("--qualifying-task-count names must be unique")
    if args.http and not args.http.startswith("/"):
        parser.error("--http takes a path starting with /")
    os.umask(0o077)
    created = common.CreatedDirs(args.keep)
    ok = False
    install_cancel_handlers()
    try:
        with common.Lane("paired.py", args.lane_wait) as lane:
            harness = Harness(args)
            harness.lane_holder = lane.holder
            harness.prepare(created)
            status = harness.run()
            ok = True
            return status
    except common.PerfRefusal as refusal:
        print(f"paired.py: {refusal}", file=sys.stderr)
        return 2
    except HarnessCancelled as cancelled:
        print(f"paired.py: cancelled by {cancelled}; stopped every arm process", file=sys.stderr)
        return 128 + cancelled.signum
    except KeyboardInterrupt:
        print("paired.py: cancelled by SIGINT; stopped every arm process", file=sys.stderr)
        return 128 + signal.SIGINT
    finally:
        work = Path(args.out).absolute() / ".work"
        if ok and not args.keep and work in created.paths:
            created.remove(work)
        # A refused run keeps parity.json for diagnosis; other partial output goes.
        keep_partial = (Path(args.out).absolute() / "parity.json").exists()
        if not ok and keep_partial and not args.keep:
            created.remove(work)
            created.keep_on_success.add(Path(args.out).absolute())
            created.finish(True)
        else:
            created.finish(ok)


if __name__ == "__main__":
    sys.exit(main())
