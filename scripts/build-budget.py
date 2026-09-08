#!/usr/bin/env python3
"""Admit repository build commands to a host-wide, cross-worktree budget."""

from __future__ import annotations

import fcntl
import os
from pathlib import Path
import sys
import time
from typing import NoReturn


DEFAULT_BUILD_SLOTS = 2
DEFAULT_CARGO_JOBS = 4
MAX_BUILD_SLOTS = 128
MAX_CARGO_JOBS = 1024


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
            "Environment: ORBIT_BUILD_SLOTS (default 2), ORBIT_CARGO_JOBS "
            "(default CARGO_BUILD_JOBS or 4), ORBIT_BUILD_BUDGET_DIR, and "
            "ORBIT_BUILD_BUDGET=0 to bypass admission."
        )
        raise SystemExit(0)

    if not arguments or arguments[0] != "--" or len(arguments) == 1:
        fail("expected -- followed by a command")

    return arguments[1:]


def configured_budget() -> tuple[int, int, bool]:
    slots = positive_integer(
        "ORBIT_BUILD_SLOTS",
        os.environ.get("ORBIT_BUILD_SLOTS", str(DEFAULT_BUILD_SLOTS)),
        MAX_BUILD_SLOTS,
    )
    jobs_source = (
        os.environ["ORBIT_CARGO_JOBS"]
        if "ORBIT_CARGO_JOBS" in os.environ
        else os.environ.get("CARGO_BUILD_JOBS", str(DEFAULT_CARGO_JOBS))
    )
    jobs = positive_integer("ORBIT_CARGO_JOBS/CARGO_BUILD_JOBS", jobs_source, MAX_CARGO_JOBS)

    enabled = os.environ.get("ORBIT_BUILD_BUDGET", "1")
    if enabled not in {"0", "1"}:
        fail(f"ORBIT_BUILD_BUDGET must be 0 or 1; got {enabled!r}")

    return slots, jobs, enabled == "1"


def lock_directory() -> Path:
    configured = os.environ.get("ORBIT_BUILD_BUDGET_DIR")
    if configured:
        directory = Path(configured).expanduser()
    else:
        directory = Path.home() / ".orbit" / "cache" / "build-budget"

    if directory.is_symlink():
        fail(f"lock directory must not be a symbolic link: {directory}")

    try:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    except OSError as error:
        fail(f"cannot create lock directory {directory}: {error}", 73)

    return directory


def acquire_slot(directory: Path, slots: int) -> tuple[int, int]:
    descriptors: list[tuple[int, int]] = []
    try:
        for slot in range(1, slots + 1):
            path = directory / f"slot-{slot:03d}.lock"
            descriptor = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
            descriptors.append((slot, descriptor))

        while True:
            for slot, descriptor in descriptors:
                try:
                    fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    continue

                for other_slot, other_descriptor in descriptors:
                    if other_slot != slot:
                        os.close(other_descriptor)
                return slot, descriptor

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
