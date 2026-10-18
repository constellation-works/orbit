"""Hang ceiling for pooled self-test subprocesses.

Idle scheduling (`chrt --idle`, or `nice -n 19` where the idle class is
unavailable) stretches wall time on a busy host. A fixed 10s or 20s cap then
fails a fixture step that would have exited. These caps are hang protection
only.

`HANG_CEILING_SECONDS` matches the fixture-step deadline (120s): no correct
local fixture step approaches it. A step that exits is returned however long
it took. A step still running at the ceiling is killed, and the error names
the step or command. Leader exit does not skip that kill: surviving group
members that still hold the output pipes are signaled too.

`timeout` overrides the ceiling for the hang demonstration. Production
callers omit it. Timeouts that are themselves the assertion do not belong
here.
"""

import os
import signal
import subprocess

# Past the removed 10s and 20s workflow-step caps, far under the ceiling.
SLOW_FIXTURE_SECONDS = 25

HANG_CEILING_SECONDS = 120

# The hang demonstration uses this instead of waiting out the ceiling.
HANG_PROBE_SECONDS = 2

# Joins that wait for a ceiling-bounded subprocess need slack so the child
# can be reaped and the thread can record the result.
SUBPROCESS_JOIN_SECONDS = HANG_CEILING_SECONDS + 15


def _kill_group(proc):
    # Signal the group before reaping the leader. A descendant can still hold
    # the inherited stdout and stderr pipes after the leader has exited, and
    # only the group signal terminates that descendant.
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        return
    except PermissionError:
        try:
            proc.kill()
        except ProcessLookupError:
            return
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            proc.kill()
        except ProcessLookupError:
            pass
        proc.wait(timeout=5)


def _tail(stream):
    if not stream:
        return ""
    if isinstance(stream, bytes):
        stream = stream.decode("utf-8", errors="replace")
    stream = stream.strip()
    if len(stream) > 500:
        return stream[-500:]
    return stream


def _hang_message(step, argv, limit, stderr):
    command = str(argv[0]) if argv else step
    message = f"hung step: {step} (command {command} exceeded {limit:g}s)"
    detail = _tail(stderr)
    if detail:
        return f"{message}\n{detail}"
    return message


def run(argv, *, step, timeout=None, check=False, input=None, capture_output=False, **popen_kwargs):
    """Run argv until it exits, or fail when it is still running at the ceiling.

    `step` is the workflow step or command name in the hang diagnostic.
    `timeout` defaults to `HANG_CEILING_SECONDS`.
    """
    if not isinstance(step, str) or not step.strip():
        raise ValueError("hang guard requires a step or command name")
    limit = HANG_CEILING_SECONDS if timeout is None else timeout
    if limit <= 0:
        raise ValueError("hang ceiling must be positive")
    if capture_output:
        popen_kwargs["stdout"] = subprocess.PIPE
        popen_kwargs["stderr"] = subprocess.PIPE
    # A new session makes the child its own process group, so a hang kill
    # reaps grandchildren (for example sleep under bash -c) with the child.
    popen_kwargs["start_new_session"] = True
    proc = subprocess.Popen(argv, **popen_kwargs)
    try:
        try:
            stdout, stderr = proc.communicate(input=input, timeout=limit)
        except subprocess.TimeoutExpired:
            _kill_group(proc)
            try:
                stdout, stderr = proc.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                stdout, stderr = b"", b""
            raise AssertionError(_hang_message(step, argv, limit, stderr)) from None
        result = subprocess.CompletedProcess(argv, proc.returncode, stdout, stderr)
        if check and result.returncode != 0:
            raise subprocess.CalledProcessError(result.returncode, argv, stdout, stderr)
        return result
    finally:
        if proc.poll() is None:
            _kill_group(proc)
