"""Hang ceiling for pooled self-test subprocesses.

Idle scheduling (`chrt --idle`, or `nice -n 19` where the idle class is
unavailable) stretches wall time on a busy host. A fixed 10s or 20s cap then
fails a fixture step that would have exited. These caps are hang protection
only.

`HANG_CEILING_SECONDS` matches the fixture-step deadline (120s): no correct
local fixture step approaches it. A step that exits is returned however long
it took. A step still running at the ceiling is killed, and the error names
the step or command. Leader exit does not skip that kill: surviving group
members that still hold the output pipes are signaled too. An owned unreaped
anchor reserves the group until its final signal, independently of polling
or communicating with the command. Both owned children are then reaped.

`timeout` overrides the ceiling for the hang demonstration. Production
callers omit it. Timeouts that are themselves the assertion do not belong
here.
"""

import os
import signal
import subprocess
import sys
import time

# Past the removed 10s and 20s workflow-step caps, far under the ceiling.
SLOW_FIXTURE_SECONDS = 25

HANG_CEILING_SECONDS = 120

# The hang demonstration uses this instead of waiting out the ceiling.
HANG_PROBE_SECONDS = 2

# Joins that wait for a ceiling-bounded subprocess need slack so the child
# can be reaped and the thread can record the result.
SUBPROCESS_JOIN_SECONDS = HANG_CEILING_SECONDS + 15


def _kill_group(owner):
    # The anchor remains unreaped until the last group signal, even when
    # poll()/communicate() already reaped the command. Never signal again
    # after a wait or a cleanup error could release the numeric identity.
    def reap(proc):
        if proc is None:
            return
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)

    try:
        if not owner.signaled:
            owner.signaled = True
            try:
                os.killpg(owner.anchor.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except PermissionError:
                for proc in (owner.process, owner.anchor):
                    if proc is not None and proc.returncode is None:
                        proc.kill()
    finally:
        try:
            reap(owner.process)
        finally:
            reap(owner.anchor)


class OwnedProcessGroup:
    """Own a command group independently of the command's exit/reaping.

    The anchor blocks on our stdin pipe, inherits no command output pipes,
    and is never polled. Python 3.11's process_group joins the command to
    that reserved group without preexec_fn (callers may be multithreaded).
    """

    def __init__(self, argv, **popen_kwargs):
        self.process = None
        self.signaled = False
        self.anchor = subprocess.Popen(
            [sys.executable, "-c", "import sys; sys.stdin.buffer.read()"],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, process_group=0)
        try:
            popen_kwargs["start_new_session"] = False
            popen_kwargs["process_group"] = self.anchor.pid
            self.process = subprocess.Popen(argv, **popen_kwargs)
        except BaseException:
            self.close()
            raise

    def kill(self):
        """Send the final group signal, then reap both owned children."""
        _kill_group(self)

    def close(self):
        """Stop the owned group and close every owned pipe on all outcomes."""
        try:
            self.kill()
        finally:
            for proc in (self.process, self.anchor):
                if proc is not None:
                    for stream in (proc.stdin, proc.stdout, proc.stderr):
                        if stream is not None:
                            stream.close()


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


def run(argv, *, step, timeout=None, check=False, input=None, capture_output=False,
        ready_file=None, startup_timeout=None, **popen_kwargs):
    """Run argv until it exits, or fail when it is still running at the ceiling.

    `step` is the workflow step or command name in the hang diagnostic.
    `timeout` defaults to `HANG_CEILING_SECONDS`.
    When `ready_file` is set, wait for a non-empty file before starting the
    hang timeout. `startup_timeout` bounds that fixture setup phase.
    """
    if not isinstance(step, str) or not step.strip():
        raise ValueError("hang guard requires a step or command name")
    limit = HANG_CEILING_SECONDS if timeout is None else timeout
    if limit <= 0:
        raise ValueError("hang ceiling must be positive")
    ready_limit = HANG_CEILING_SECONDS if startup_timeout is None else startup_timeout
    if ready_file is not None and ready_limit <= 0:
        raise ValueError("startup timeout must be positive")
    if capture_output:
        popen_kwargs["stdout"] = subprocess.PIPE
        popen_kwargs["stderr"] = subprocess.PIPE
    owned = OwnedProcessGroup(argv, **popen_kwargs)
    proc = owned.process
    try:
        if ready_file is not None:
            deadline = time.monotonic() + ready_limit
            while True:
                try:
                    ready = os.path.getsize(ready_file) > 0
                except FileNotFoundError:
                    ready = False
                if ready or proc.poll() is not None:
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    owned.kill()
                    try:
                        _, stderr = proc.communicate(timeout=5)
                    except subprocess.TimeoutExpired:
                        stderr = b""
                    message = f"fixture for {step} did not become ready within {ready_limit:g}s"
                    detail = _tail(stderr)
                    if detail:
                        message = f"{message}\n{detail}"
                    raise AssertionError(message) from None
                time.sleep(min(0.01, remaining))
        try:
            stdout, stderr = proc.communicate(input=input, timeout=limit)
        except subprocess.TimeoutExpired:
            owned.kill()
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
        owned.close()
