#!/usr/bin/env python3
"""Admit repository build commands to a host-wide, cross-worktree budget."""

from __future__ import annotations

import fcntl
import math
import os
from pathlib import Path
import sys
import time
from typing import NoReturn


DEFAULT_BUILD_SLOTS = 2
DEFAULT_CARGO_JOBS = 4
MAX_BUILD_SLOTS = 128
MAX_CARGO_JOBS = 1024
DEFAULT_WAIT_REPORT_INTERVAL_SECONDS = 45.0
TEST_WAIT_REPORT_INTERVAL_ENV = "_ORBIT_BUILD_BUDGET_TEST_WAIT_INTERVAL_SECONDS"


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
            "\n"
            "Configuration: ORBIT_BUILD_SLOTS overrides <budget-dir>/slots "
            "(default 2); ORBIT_CARGO_JOBS, then CARGO_BUILD_JOBS, override "
            "<budget-dir>/cargo-jobs (default 4). The budget directory is "
            "ORBIT_BUILD_BUDGET_DIR or ~/.orbit/cache/build-budget. "
            "ORBIT_BUILD_BUDGET=0 bypasses admission."
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


def host_setting(
    directory: Path,
    filename: str,
    name: str,
    default: int,
    maximum: int,
) -> int:
    path = directory / filename
    try:
        value = path.read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        return default
    except (OSError, UnicodeError) as error:
        fail(f"cannot read {name} from {path}: {error}", 73)

    return positive_integer(name, value, maximum)


def configured_budget() -> tuple[int, int, bool]:
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

    enabled = os.environ.get("ORBIT_BUILD_BUDGET", "1")
    if enabled not in {"0", "1"}:
        fail(f"ORBIT_BUILD_BUDGET must be 0 or 1; got {enabled!r}")

    return slots, jobs, enabled == "1"


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
                    report_wait(f"acquired slot {slot} after {elapsed:.1f}s")
                return slot, descriptor

            now = time.monotonic()
            if not waiting:
                report_wait(
                    f"waiting for admission (slots={slots}, budget_dir={directory})"
                )
                waiting = True
                next_report = now + report_interval
            elif now >= next_report:
                elapsed = now - wait_started
                report_wait(f"still waiting for admission (elapsed {elapsed:.1f}s)")
                next_report = now + report_interval

            time.sleep(0.05)
    except BaseException:
        for _, descriptor in descriptors:
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise


def main() -> None:
    command = command_arguments(sys.argv[1:])
    slots, cargo_jobs, enabled = configured_budget()

    environment = os.environ.copy()
    environment["CARGO_BUILD_JOBS"] = str(cargo_jobs)

    already_admitted = environment.get("ORBIT_BUILD_BUDGET_HELD") == "1"
    if not enabled or already_admitted:
        os.execvpe(command[0], command, environment)

    slot, descriptor = acquire_slot(lock_directory(), slots)
    os.set_inheritable(descriptor, True)
    environment["ORBIT_BUILD_BUDGET_HELD"] = "1"
    environment["ORBIT_BUILD_BUDGET_SLOT"] = str(slot)

    try:
        os.execvpe(command[0], command, environment)
    except FileNotFoundError:
        fail(f"command not found: {command[0]}", 127)
    except OSError as error:
        fail(f"could not execute {command[0]}: {error}", 126)


if __name__ == "__main__":
    main()
