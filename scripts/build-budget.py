#!/usr/bin/env python3
"""Admit repository build commands to a host-wide, cross-worktree budget."""

from __future__ import annotations

import fcntl
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import uuid
from typing import NoReturn


DEFAULT_BUILD_SLOTS = 2
DEFAULT_CARGO_JOBS = 4
MAX_BUILD_SLOTS = 128
MAX_CARGO_JOBS = 1024
MAX_TEST_THREADS = 1024
TEST_THREADS_ENV = "ORBIT_TEST_THREADS"
TEST_THREAD_VARIABLES = ("NEXTEST_TEST_THREADS", "RUST_TEST_THREADS")
DEFAULT_WAIT_REPORT_INTERVAL_SECONDS = 45.0
TEST_WAIT_REPORT_INTERVAL_ENV = "_ORBIT_BUILD_BUDGET_TEST_WAIT_INTERVAL_SECONDS"
WAIT_DIRECTORY_ENV = "ORBIT_ACTIVITY_BUILD_BUDGET_DIR"
LOW_PRIORITY_ENV = "ORBIT_BUILD_LOW_PRIORITY"
# Wrapped commands run at this niceness or higher, so Orbit's control processes win CPU contention.
CHILD_NICENESS = 10
# Lowest best-effort IO level. The idle class is avoided: it can starve compiles while Orbit's
# store keeps issuing IO.
CHILD_IONICE_ARGUMENTS = ("-c", "2", "-n", "7")


class ManagedWait:
    """Heartbeat only admission time, independently of provider output buffering."""

    def __init__(self) -> None:
        directory = os.environ.get(WAIT_DIRECTORY_ENV)
        self.path = Path(directory) / f"{uuid.uuid4().hex}.json" if directory else None
        self.started_monotonic_ms = time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1_000_000
        self.started = time.monotonic()
        self.next_update = 0.0
        self.lock_file = None
        if self.path is not None:
            try:
                self.lock_file = self.path.with_suffix(".lock").open("a+")
                fcntl.flock(self.lock_file, fcntl.LOCK_EX)
            except OSError:
                if self.lock_file is not None:
                    self.lock_file.close()
                self.lock_file = None
                self.path = None

    def update(self, finished: bool = False) -> None:
        now = time.monotonic()
        if not finished and now < self.next_update:
            return
        self.next_update = now + 0.1
        if self.path is None:
            return
        # Atomic replacement gives the reader a complete snapshot. A killed
        # wrapper stops heartbeating, so it cannot keep extending the deadline.
        temporary = self.path.with_suffix(".pending")
        try:
            temporary.write_text(json.dumps({
                "started_monotonic_ms": self.started_monotonic_ms,
                "elapsed_ms": int((now - self.started) * 1000),
                "finished": finished,
            }), encoding="utf-8")
            temporary.replace(self.path)
        except OSError:
            # The ordinary lock budget still works without managed telemetry.
            pass
        finally:
            if finished and self.lock_file is not None:
                self.lock_file.close()
                self.lock_file = None


def fail(message: str, status: int = 64) -> NoReturn:
    print(f"build-budget: {message}", file=sys.stderr)
    raise SystemExit(status)


def positive_integer(name: str, value: str, maximum: int) -> int:
    if not value.isascii() or not value.isdecimal() or value.startswith("0"):
        fail(f"{name} must be a decimal integer from 1 through {maximum}; got {value!r}")

    parsed = int(value)
    if parsed > maximum:
        fail(f"{name} must be a decimal integer from 1 through {maximum}; got {value!r}")

    return parsed


def command_arguments(arguments: list[str]) -> list[str]:
    if arguments == ["--help"] or arguments == ["-h"]:
        print(
            "usage: scripts/build-budget.py -- COMMAND [ARG ...]\n"
            "       scripts/build-budget.py --cargo-jobs\n"
            "\n"
            "Configuration: ORBIT_BUILD_SLOTS overrides <budget-dir>/slots "
            "(default 2); ORBIT_CARGO_JOBS, then CARGO_BUILD_JOBS, override "
            "<budget-dir>/cargo-jobs (default 4); ORBIT_TEST_THREADS overrides "
            "<budget-dir>/test-threads (default unset). The budget directory is "
            "ORBIT_BUILD_BUDGET_DIR or ~/.orbit/cache/build-budget. "
            "ORBIT_BUILD_BUDGET=0 bypasses admission. ORBIT_BUILD_LOW_PRIORITY=0 keeps "
            "the caller's CPU and IO priority (default: niceness at least 10, and best-effort "
            "IO level 7 on Linux)."
        )
        raise SystemExit(0)

    if not arguments or arguments[0] != "--" or len(arguments) == 1:
        fail("expected -- followed by a command")

    return arguments[1:]


def budget_directory_path() -> Path:
    configured = os.environ.get("ORBIT_BUILD_BUDGET_DIR")
    if configured:
        return Path(configured).expanduser()
    return Path.home() / ".orbit" / "cache" / "build-budget"


def optional_host_setting(directory: Path, filename: str, name: str, maximum: int) -> int | None:
    path = directory / filename
    try:
        value = path.read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        return None
    except (OSError, UnicodeError) as error:
        fail(f"cannot read {name} from {path}: {error}", 73)

    return positive_integer(name, value, maximum)


def host_setting(
    directory: Path,
    filename: str,
    name: str,
    default: int,
    maximum: int,
) -> int:
    value = optional_host_setting(directory, filename, name, maximum)
    return default if value is None else value


def configured_budget() -> tuple[int, int, int | None, bool]:
    directory = budget_directory_path()
    if directory.is_symlink():
        fail(f"lock directory must not be a symbolic link: {directory}")

    if "ORBIT_BUILD_SLOTS" in os.environ:
        slots = positive_integer("ORBIT_BUILD_SLOTS", os.environ["ORBIT_BUILD_SLOTS"], MAX_BUILD_SLOTS)
    else:
        slots = host_setting(
            directory,
            "slots",
            "ORBIT_BUILD_SLOTS",
            DEFAULT_BUILD_SLOTS,
            MAX_BUILD_SLOTS,
        )

    if "ORBIT_CARGO_JOBS" in os.environ:
        jobs_source = os.environ["ORBIT_CARGO_JOBS"]
    elif "CARGO_BUILD_JOBS" in os.environ:
        jobs_source = os.environ["CARGO_BUILD_JOBS"]
    else:
        jobs_source = str(
            host_setting(
                directory,
                "cargo-jobs",
                "ORBIT_CARGO_JOBS/CARGO_BUILD_JOBS",
                DEFAULT_CARGO_JOBS,
                MAX_CARGO_JOBS,
            )
        )
    jobs = positive_integer("ORBIT_CARGO_JOBS/CARGO_BUILD_JOBS", jobs_source, MAX_CARGO_JOBS)

    if TEST_THREADS_ENV in os.environ:
        test_threads = positive_integer(TEST_THREADS_ENV, os.environ[TEST_THREADS_ENV], MAX_TEST_THREADS)
    else:
        test_threads = optional_host_setting(directory, "test-threads", TEST_THREADS_ENV, MAX_TEST_THREADS)

    enabled = os.environ.get("ORBIT_BUILD_BUDGET", "1")
    if enabled not in {"0", "1"}:
        fail(f"ORBIT_BUILD_BUDGET must be 0 or 1; got {enabled!r}")

    return slots, jobs, test_threads, enabled == "1"


def low_priority_enabled() -> bool:
    value = os.environ.get(LOW_PRIORITY_ENV, "1")
    if value not in {"0", "1"}:
        fail(f"{LOW_PRIORITY_ENV} must be 0 or 1; got {value!r}")
    return value == "1"


def lock_directory() -> Path:
    directory = budget_directory_path()

    if directory.is_symlink():
        fail(f"lock directory must not be a symbolic link: {directory}")

    try:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    except OSError as error:
        fail(f"cannot create lock directory {directory}: {error}", 73)

    return directory


def wait_report_interval() -> float:
    """Use a short interval only when the process test explicitly requests it."""
    value = os.environ.get(TEST_WAIT_REPORT_INTERVAL_ENV)
    if value is not None:
        try:
            interval = float(value)
        except ValueError:
            interval = 0.0
        if math.isfinite(interval) and interval > 0:
            return interval
    return DEFAULT_WAIT_REPORT_INTERVAL_SECONDS


def report_wait(message: str) -> None:
    try:
        print(f"build-budget: {message}", file=sys.stderr, flush=True)
    except OSError:
        # Diagnostics must not prevent admission or change the wrapped command's status.
        pass


def acquire_slot(directory: Path, slots: int) -> tuple[int, int]:
    descriptors: list[tuple[int, int]] = []
    try:
        for slot in range(1, slots + 1):
            path = directory / f"slot-{slot:03d}.lock"
            descriptor = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
            descriptors.append((slot, descriptor))

        wait_started = time.monotonic()
        report_interval = wait_report_interval()
        waiting = False
        managed_wait = None
        next_report = 0.0
        while True:
            for slot, descriptor in descriptors:
                try:
                    fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    continue

                for other_slot, other_descriptor in descriptors:
                    if other_slot != slot:
                        os.close(other_descriptor)
                if waiting:
                    elapsed = time.monotonic() - wait_started
                    if managed_wait is not None:
                        managed_wait.update(finished=True)
                    report_wait(f"acquired slot {slot} after {elapsed:.1f}s")
                return slot, descriptor

            now = time.monotonic()
            if not waiting:
                managed_wait = ManagedWait()
                report_wait(
                    f"waiting for admission (slots={slots}, budget_dir={directory})"
                )
                waiting = True
                next_report = now + report_interval

            elif now >= next_report:
                elapsed = now - wait_started
                report_wait(f"still waiting for admission (elapsed {elapsed:.1f}s)")
                next_report = now + report_interval

            if managed_wait is not None:
                managed_wait.update()

            time.sleep(0.05)
    except BaseException:
        for _, descriptor in descriptors:
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise


def lower_own_priority() -> None:
    """Raise this process's niceness to CHILD_NICENESS without ever lowering it."""
    try:
        if os.getpriority(os.PRIO_PROCESS, 0) < CHILD_NICENESS:
            os.setpriority(os.PRIO_PROCESS, 0, CHILD_NICENESS)
    except OSError:
        # Priority is advisory: a refused change leaves the command at its inherited level.
        pass


def prioritized_command(command: list[str], environment: dict[str, str]) -> list[str]:
    """Prefix a Linux command with ionice so it runs at the lowest best-effort IO level."""
    if not sys.platform.startswith("linux"):
        return command
    ionice = shutil.which("ionice")
    # A missing command or ionice keeps the command unchanged, so the caller reports it as before.
    if ionice is None or shutil.which(command[0], path=environment.get("PATH", os.defpath)) is None:
        return command
    return [ionice, *CHILD_IONICE_ARGUMENTS, "--", *command]


def main() -> None:
    if sys.argv[1:] == ["--cargo-jobs"]:
        # Read-only: callers size concurrency from the jobs Cargo would receive, without taking a slot.
        print(configured_budget()[1])
        return

    command = command_arguments(sys.argv[1:])
    slots, cargo_jobs, test_threads, enabled = configured_budget()
    low_priority = low_priority_enabled()

    environment = os.environ.copy()
    environment["CARGO_BUILD_JOBS"] = str(cargo_jobs)
    if test_threads is not None:
        for name in TEST_THREAD_VARIABLES:
            # A value the caller exported is more specific than the host setting.
            environment.setdefault(name, str(test_threads))

    already_admitted = environment.get("ORBIT_BUILD_BUDGET_HELD") == "1"
    launched = prioritized_command(command, environment) if low_priority else command
    if not enabled or already_admitted:
        if low_priority:
            # This process becomes the command, so the lowered priority carries over exec.
            lower_own_priority()
        os.execvpe(launched[0], launched, environment)

    slot, descriptor = acquire_slot(lock_directory(), slots)
    environment["ORBIT_BUILD_BUDGET_HELD"] = "1"
    environment["ORBIT_BUILD_BUDGET_SLOT"] = str(slot)

    try:
        status = run_admitted_command(launched, environment, low_priority)
    except FileNotFoundError:
        fail(f"command not found: {command[0]}", 127)
    except OSError as error:
        fail(f"could not execute {command[0]}: {error}", 126)
    finally:
        os.close(descriptor)

    if status < 0:
        # Preserve signal termination for callers that inspect waitpid status,
        # rather than only returning the shell's conventional 128 + signal.
        if -status != signal.SIGKILL:
            signal.signal(-status, signal.SIG_DFL)
        os.kill(os.getpid(), -status)
    raise SystemExit(status)


def run_admitted_command(command: list[str], environment: dict[str, str], low_priority: bool) -> int:
    """Hold admission in the parent until the direct command has exited."""
    child = None
    pending_signals: list[int] = []

    def forward_signal(signum: int, _frame: object) -> None:
        if child is None:
            # A cancellation can arrive while Popen is still starting the child.
            pending_signals.append(signum)
        else:
            child.send_signal(signum)

    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGQUIT, signal.SIGTERM):
        # Retain inherited ignored signals (for example background-shell SIGINT).
        if signal.getsignal(signum) != signal.SIG_IGN:
            signal.signal(signum, forward_signal)

    # In particular, never pass the flock descriptor to the command. Descendants
    # may outlive it, but only this supervising process owns the build slot.
    # Priority applies only to the child: the parent keeps its level while it holds admission.
    child = subprocess.Popen(
        command,
        env=environment,
        close_fds=True,
        preexec_fn=lower_own_priority if low_priority else None,
    )
    for signum in pending_signals:
        child.send_signal(signum)
    return child.wait()


if __name__ == "__main__":
    main()
