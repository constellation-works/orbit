#!/usr/bin/env python3
"""Run one command and record the orbit processes that belong to it.

The Claude MCP smokes accept an audit row only when two things are true: the
command exits 0, and the row's pid was an ``orbit`` process observed under
that command. A pid from another client, a null pid, or the command's own pid
is not evidence unless that process's command name is ``orbit``.

Descendants are read from the OS (Linux ``/proc``, otherwise ``ps``). Each
matching pid is appended to ``$ORBIT_SMOKE_OBSERVED`` as soon as it is seen,
one complete line at a time, so a caller can wait for its own server to be
recorded. Exit status is the command's status. Exit 2 when the process tree
cannot be observed or the invocation is not usable.
"""

from __future__ import annotations

import os
import subprocess
import sys
import threading
import time


def _basename(token: str) -> str:
    return os.path.basename(token)


def _cmdline_basename(pid: int) -> str | None:
    try:
        raw = open(f"/proc/{pid}/cmdline", "rb").read()
    except OSError:
        return None
    if not raw:
        return None
    first = raw.split(b"\0", 1)[0]
    if not first:
        return None
    return _basename(first.decode("utf-8", "surrogateescape"))


def _linux_children(pid: int) -> list[int] | None:
    """Direct children, or None when this kernel has no children file.

    An empty file is a real answer (the process has no children). A missing
    file is the signal to fall back to a parent-pid scan. ``FileNotFoundError``
    is raised for both a missing file and an exited process; the caller treats
    that as "use the parent-pid scan" only when the file is absent on a live
    process, and an empty result when the process is already gone.
    """
    path = f"/proc/{pid}/task/{pid}/children"
    if not os.path.isdir(f"/proc/{pid}"):
        return []
    try:
        text = open(path, encoding="ascii").read()
    except FileNotFoundError:
        return None
    except OSError:
        return []
    return [int(part) for part in text.split() if part.isdigit()]


def _stat_ppid(pid: int) -> int | None:
    try:
        text = open(f"/proc/{pid}/stat", encoding="utf-8", errors="surrogateescape").read()
    except OSError:
        return None
    # comm is inside parentheses and may itself contain ")".
    end = text.rfind(")")
    if end < 0 or end + 2 >= len(text):
        return None
    fields = text[end + 2:].split()
    if len(fields) < 2 or not fields[1].isdigit():
        return None
    return int(fields[1])


def _descendants_by_ppid(root: int) -> list[int]:
    children: dict[int, list[int]] = {}
    names: dict[int, str | None] = {}
    try:
        entries = os.listdir("/proc")
    except OSError:
        return []
    for name in entries:
        if not name.isdigit():
            continue
        pid = int(name)
        ppid = _stat_ppid(pid)
        if ppid is None:
            continue
        names[pid] = _cmdline_basename(pid)
        children.setdefault(ppid, []).append(pid)
    found: list[int] = []
    if names.get(root) == "orbit":
        found.append(root)
    stack = list(children.get(root, []))
    seen: set[int] = set()
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        if names.get(pid) == "orbit":
            found.append(pid)
        stack.extend(children.get(pid, []))
    return found


def _sample_linux(root: int) -> list[int]:
    direct = _linux_children(root)
    if direct is None:
        return _descendants_by_ppid(root)
    found: list[int] = []
    if _cmdline_basename(root) == "orbit":
        found.append(root)
    stack = list(direct)
    seen: set[int] = set()
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        if _cmdline_basename(pid) == "orbit":
            found.append(pid)
        kids = _linux_children(pid)
        if kids is None:
            return _descendants_by_ppid(root)
        stack.extend(kids)
    return found


def _sample_ps(root: int) -> list[int] | None:
    result = subprocess.run(
        ["ps", "-ax", "-o", "pid=,ppid=,args="],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    children: dict[int, list[int]] = {}
    args_by_pid: dict[int, str] = {}
    for line in result.stdout.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) < 2 or not parts[0].isdigit() or not parts[1].isdigit():
            continue
        pid = int(parts[0])
        ppid = int(parts[1])
        args_by_pid[pid] = parts[2] if len(parts) == 3 else ""
        children.setdefault(ppid, []).append(pid)
    found: list[int] = []
    stack = [root]
    seen: set[int] = set()
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        args = args_by_pid.get(pid, "")
        token = args.split(None, 1)[0] if args else ""
        if token and _basename(token) == "orbit":
            found.append(pid)
        stack.extend(children.get(pid, []))
    return found


def sample(root: int) -> list[int] | None:
    """Pids whose command name is ``orbit`` at or under ``root``.

    None means the process tree could not be read. An empty list means it was
    read and no orbit process was there.
    """
    if os.path.isdir("/proc/self"):
        return _sample_linux(root)
    return _sample_ps(root)


def _record(observed: str, seen: set[int], pids: list[int]) -> None:
    fresh = [pid for pid in pids if pid not in seen]
    if not fresh:
        return
    with open(observed, "a", encoding="utf-8") as handle:
        for pid in fresh:
            seen.add(pid)
            handle.write(f"{pid}\n")
        handle.flush()
        os.fsync(handle.fileno())


def main(argv: list[str]) -> int:
    merge_stderr = False
    args = list(argv)
    if args and args[0] == "--merge-stderr":
        merge_stderr = True
        args = args[1:]
    if args and args[0] == "--":
        args = args[1:]
    if not args:
        print("FAIL: smoke invoke: missing command", file=sys.stderr)
        return 2
    observed = os.environ.get("ORBIT_SMOKE_OBSERVED")
    if not observed:
        print("FAIL: smoke invoke: ORBIT_SMOKE_OBSERVED is unset", file=sys.stderr)
        return 2
    try:
        proc = subprocess.Popen(
            args,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT if merge_stderr else None,
        )
    except OSError as error:
        print(f"FAIL: smoke invoke: cannot start {args[0]}: {error}", file=sys.stderr)
        return 2
    chunks: list[bytes] = []

    def drain() -> None:
        stream = proc.stdout
        if stream is None:
            return
        while True:
            data = stream.read(4096)
            if not data:
                break
            chunks.append(data)

    thread = threading.Thread(target=drain, daemon=True)
    thread.start()
    seen: set[int] = set()
    observable = True
    while True:
        found = sample(proc.pid)
        if found is None:
            observable = False
            break
        _record(observed, seen, found)
        if proc.poll() is not None:
            break
        time.sleep(0.02)
    if observable:
        again = sample(proc.pid)
        if again is None:
            observable = False
        else:
            _record(observed, seen, again)
    rc = proc.wait()
    thread.join()
    sys.stdout.buffer.write(b"".join(chunks))
    if not observable:
        print("FAIL: smoke invoke: cannot observe the process tree", file=sys.stderr)
        return 2
    if rc < 0:
        return 1
    return rc


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
