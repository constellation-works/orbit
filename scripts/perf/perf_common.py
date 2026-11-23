"""Shared guards for the paired perf timing protocol (scripts/perf/).

Also the command-line owner of these rules for shell entry points
(`build-pair.sh`): `disk-gate`, `require-absent-or-empty` and `lane-exec`
(see `main`), so a shell script never re-spells them.

Holds the three host-safety rules every perf entry point applies:

- the disk gate (refuse to create anything when the parent filesystem is at
  or above 80% full, per RB-11 in the ws_constellation runbooks);
- the host-wide serial lane (one `flock` lock outside the live Orbit state);
- the protected-root check (never restore into, or clean, the live Orbit
  root, a registered workspace's `.orbit` directory, or a deployed service's
  state).

See docs/runbooks/perf-timing-protocol.md.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import pwd
import shutil
import signal
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

DISK_GATE_PERCENT = 80
# Test hook for the disk gate refusal: report this percentage instead of
# reading `df`. Never set it for a real measurement.
SIMULATE_DISK_ENV = "ORBIT_PERF_SIMULATE_DISK_PCENT"
LANE_LOCK_ENV = "ORBIT_PERF_LANE_LOCK"
LANE_WAIT_ENV = "ORBIT_PERF_LANE_WAIT_SECONDS"
# Set by `lane-exec` in the child it runs, so a re-entered shell script knows
# the lane is already held and does not take it again.
LANE_HELD_ENV = "ORBIT_PERF_LANE_HELD"
DEFAULT_LANE_WAIT_SECONDS = 900


class PerfRefusal(Exception):
    """A protocol refusal: printed as one line, exit status 2."""


def now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def real_home() -> Path:
    """The invoking user's passwd home, independent of an overridden HOME."""
    return Path(pwd.getpwuid(os.getuid()).pw_dir)


def host_identity() -> dict:
    machine_id = ""
    for candidate in ("/etc/machine-id", "/var/lib/dbus/machine-id"):
        try:
            machine_id = Path(candidate).read_text().strip()
            break
        except OSError:
            continue
    return {"host": socket.gethostname(), "machine_id": machine_id}


# ── disk gate ────────────────────────────────────────────────────────────────


def disk_percent(parent: Path) -> int:
    simulated = os.environ.get(SIMULATE_DISK_ENV, "").strip()
    if simulated:
        return int(simulated)
    output = subprocess.run(
        ["df", "--output=pcent", str(parent)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    digits = "".join(ch for ch in output.splitlines()[-1] if ch.isdigit())
    return int(digits)


def disk_gate(target: Path) -> None:
    """Refuse before creating `target` when its parent filesystem is >= 80%."""
    parent = target.parent
    while not parent.exists() and parent != parent.parent:
        parent = parent.parent
    percent = disk_percent(parent)
    if percent >= DISK_GATE_PERCENT:
        raise PerfRefusal(
            f"disk gate: {parent} is at {percent}% (df --output=pcent), at or above "
            f"{DISK_GATE_PERCENT}%; created nothing for {target}. Free space first "
            "(RB-11, ws_constellation runbooks)."
        )


# ── protected roots ──────────────────────────────────────────────────────────


def _resolve(path: Path) -> Path:
    return Path(os.path.realpath(path))


def _registry_orbit_dirs(registry: Path) -> list[str]:
    """The `orbit_dir` of every checkout the registry names, or refuse.

    Only a missing registry names no checkouts. A registry that exists but cannot
    be read or parsed, or is not an object whose `checkouts` is a list of objects
    each naming an `orbit_dir`, refuses: guarding without it would let a live
    workspace's `.orbit` through the check.
    """
    try:
        raw = registry.read_bytes()
    except FileNotFoundError:
        return []
    except OSError as error:
        raise PerfRefusal(f"cannot read {registry}: {error.strerror or error}") from error
    try:
        data = json.loads(raw)
    except ValueError as error:
        raise PerfRefusal(f"{registry} is not valid JSON: {error}") from error
    if not isinstance(data, dict):
        raise PerfRefusal(f"{registry} is not a registry object")
    checkouts = data.get("checkouts", [])
    if not isinstance(checkouts, list):
        raise PerfRefusal(f"{registry}: checkouts is not a list")
    orbit_dirs = []
    for index, checkout in enumerate(checkouts):
        orbit_dir = checkout.get("orbit_dir") if isinstance(checkout, dict) else None
        if not isinstance(orbit_dir, str) or not orbit_dir:
            raise PerfRefusal(f"{registry}: checkouts[{index}] has no orbit_dir string")
        orbit_dirs.append(orbit_dir)
    return orbit_dirs


def protected_roots() -> list[Path]:
    """Directories a perf run must never restore into or remove.

    The live global root (`~/.orbit` of the passwd home, plus `$HOME/.orbit`
    and `ORBIT_ROOT` when set) and every checkout `.orbit` directory the live
    registry names. The deployed `orbit-web.service` serves exactly these.
    A registry that exists but cannot be read or parsed raises `PerfRefusal`.
    """
    roots = {_resolve(real_home() / ".orbit")}
    env_home = os.environ.get("HOME")
    if env_home:
        roots.add(_resolve(Path(env_home) / ".orbit"))
    env_root = os.environ.get("ORBIT_ROOT", "").strip()
    if env_root:
        roots.add(_resolve(Path(os.path.expanduser(env_root))))
    registry = real_home() / ".orbit" / "workspaces.json"
    for orbit_dir in _registry_orbit_dirs(registry):
        roots.add(_resolve(Path(orbit_dir)))
    return sorted(roots)


def _is_run_scratch(path: Path, root: Path) -> bool:
    """`<orbit_dir>/state/worktrees/<run>/.orbit/tmp/**` is a run's own scratch."""
    try:
        parts = path.relative_to(root).parts
    except ValueError:
        return False
    return (
        len(parts) >= 5
        and parts[0] == "state"
        and parts[1] == "worktrees"
        and parts[3] == ".orbit"
        and parts[4] == "tmp"
    )


def refuse_protected(target: Path, action: str) -> None:
    resolved = _resolve(target)
    for root in protected_roots():
        inside = resolved == root or root in resolved.parents
        if inside and not _is_run_scratch(resolved, root):
            raise PerfRefusal(
                f"refusing to {action} {target}: it is or resolves under the live Orbit "
                f"root {root}"
            )
        if resolved in root.parents:
            raise PerfRefusal(
                f"refusing to {action} {target}: it contains the live Orbit root {root}"
            )


# ── serial lane ──────────────────────────────────────────────────────────────


def lane_lock_path() -> Path:
    """Host-wide lane lock.

    Defaults to `$HOME/.orbit/cache/perf-lane/lane.lock` (passwd home): host
    shared, writable from Orbit's Linux activity sandbox (like the build
    budget's `~/.orbit/cache/build-budget`), and outside every store and
    workspace state directory. `ORBIT_PERF_LANE_LOCK` overrides it; every
    perf run on a host must use the same path.
    """
    override = os.environ.get(LANE_LOCK_ENV, "").strip()
    if override:
        return Path(override)
    return real_home() / ".orbit" / "cache" / "perf-lane" / "lane.lock"


class Lane:
    """Holds the host-wide perf lane for the life of the process."""

    def __init__(self, entry: str, wait_seconds: float):
        self.path = lane_lock_path()
        self.entry = entry
        self.wait_seconds = wait_seconds
        self.fd: int | None = None

    def __enter__(self) -> "Lane":
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(self.path, os.O_RDWR | os.O_CREAT, 0o644)
        deadline = time.monotonic() + self.wait_seconds
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    holder = os.pread(fd, 4096, 0).decode("utf-8", "replace").strip()
                    os.close(fd)
                    raise PerfRefusal(f"perf lane busy: {holder or '<unknown holder>'}")
                time.sleep(0.5)
        holder = {
            "task_id": os.environ.get("ORBIT_TASK_ID", ""),
            "run_id": os.environ.get("ORBIT_RUN_ID", ""),
            "entry": self.entry,
            "pid": os.getpid(),
            "host": socket.gethostname(),
            "started_at": now_iso(),
        }
        os.ftruncate(fd, 0)
        os.pwrite(fd, (json.dumps(holder, sort_keys=True) + "\n").encode(), 0)
        self.fd = fd
        self.holder = holder
        return self

    def __exit__(self, *exc) -> None:
        if self.fd is not None:
            os.ftruncate(self.fd, 0)
            fcntl.flock(self.fd, fcntl.LOCK_UN)
            os.close(self.fd)
            self.fd = None


# ── created-directory cleanup ────────────────────────────────────────────────


class CreatedDirs:
    """Removes the directories a script created when it exits, unless --keep."""

    def __init__(self, keep: bool):
        self.keep = keep
        self.paths: list[Path] = []
        self.keep_on_success: set[Path] = set()

    def create(self, path: Path, *, output: bool = False) -> Path:
        disk_gate(path)
        path.mkdir(parents=True, exist_ok=True)
        self.paths.append(path)
        if output:
            self.keep_on_success.add(path)
        return path

    def remove(self, path: Path) -> None:
        if self.keep:
            return
        if path in self.paths:
            shutil.rmtree(path, ignore_errors=True)
            self.paths.remove(path)

    def finish(self, success: bool) -> None:
        if self.keep:
            for path in self.paths:
                if path.exists():
                    print(f"kept: {path}", file=sys.stderr)
            return
        for path in reversed(self.paths):
            if success and path in self.keep_on_success:
                continue
            shutil.rmtree(path, ignore_errors=True)


def require_absent_or_empty(path: Path) -> None:
    if path.exists() and (not path.is_dir() or any(path.iterdir())):
        raise PerfRefusal(f"{path} already exists and is not empty")


def json_text(value) -> str:
    """The on-disk text of a JSON evidence file, so callers can size it before writing."""
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


def write_json(path: Path, value) -> None:
    path.write_text(json_text(value))


# ── command line (for shell entry points) ────────────────────────────────────


def _run_in_lane(entry: str, wait_seconds: float, command: list[str]) -> int:
    """Run `command` while this process holds the lane; forward termination signals."""
    with Lane(entry, wait_seconds):
        child = subprocess.Popen(command, env={**os.environ, LANE_HELD_ENV: "1"})
        forwarded = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        previous = {
            number: signal.signal(number, lambda signum, _frame: child.send_signal(signum))
            for number in forwarded
        }
        try:
            status = child.wait()
        finally:
            for number, handler in previous.items():
                signal.signal(number, handler)
    return 128 - status if status < 0 else status


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Perf host guards for shell entry points.")
    commands = parser.add_subparsers(dest="command", required=True)
    for name, helptext in (
        ("disk-gate", "refuse when the filesystem that will hold TARGET is at or above 80%%"),
        ("require-absent-or-empty", "refuse a non-empty PATH; remove an empty directory"),
    ):
        sub = commands.add_parser(name, help=helptext)
        sub.add_argument("--entry", required=True, help="script name, prefixed to a refusal")
        sub.add_argument("path", type=Path)
    lane = commands.add_parser("lane-exec", help="run COMMAND while holding the host-wide lane")
    lane.add_argument("--entry", required=True)
    lane.add_argument("--wait", type=float, help=f"seconds to wait (default ${LANE_WAIT_ENV} or {DEFAULT_LANE_WAIT_SECONDS})")
    lane.add_argument("lane_command", nargs=argparse.REMAINDER, metavar="-- COMMAND")
    args = parser.parse_args(argv)
    try:
        if args.command == "disk-gate":
            disk_gate(args.path)
        elif args.command == "require-absent-or-empty":
            require_absent_or_empty(args.path)
        else:
            command = args.lane_command[1:] if args.lane_command[:1] == ["--"] else args.lane_command
            if not command:
                parser.error("lane-exec needs a command after --")
            wait = args.wait
            if wait is None:
                try:
                    wait = float(os.environ.get(LANE_WAIT_ENV) or DEFAULT_LANE_WAIT_SECONDS)
                except ValueError:
                    raise PerfRefusal(f"{LANE_WAIT_ENV} is not a number") from None
            return _run_in_lane(args.entry, wait, command)
    except PerfRefusal as refusal:
        print(f"{args.entry}: {refusal}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
