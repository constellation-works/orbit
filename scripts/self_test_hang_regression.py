"""Intercept process APIs at the hang runners' public boundaries.

Reaping a group leader immediately permits a foreign replacement in this
model. Every signal checks that an owned child still reserves the group;
no kernel process, signal, synthetic load or PID churn is involved.
"""

import io
import signal
import subprocess
from unittest import mock


def assert_hang_group_identity(test, runner):
    """Exercise an exited leader whose inherited output pipes remain open."""
    for escaped_pipes in (False, True):
        with test.subTest(escaped_pipes=escaped_pipes):
            processes = []
            events = []
            state = {"killed": False, "clock": 0.0}

            class Stream(io.BytesIO):
                def fileno(self):
                    return 123

            class Process:
                def __init__(self, argv, **kwargs):
                    self.anchor = kwargs.get("process_group") == 0
                    self.pid = 100 + len(processes)
                    self.returncode = None
                    self.stdin = Stream() if self.anchor else None
                    self.stdout = Stream() if not self.anchor else None
                    self.stderr = Stream() if not self.anchor else None
                    processes.append(self)
                    events.append(("spawn", self.pid))

                def poll(self):
                    return self.wait()

                def wait(self, **kwargs):
                    if self.returncode is None:
                        events.append(("reap", self.pid))
                    self.returncode = 0
                    return 0

                def kill(self):
                    test.assertIsNone(self.returncode, "individual kill must retain child ownership")
                    events.append(("kill", self.pid))

                def communicate(self, **kwargs):
                    if not state["killed"] or escaped_pipes:
                        raise subprocess.TimeoutExpired("fixture", kwargs.get("timeout"))
                    self.wait()
                    return b"", b""

            def killpg(group, sig):
                test.assertTrue(
                    any(p.pid == group and p.returncode is None for p in processes),
                    "final signal must not reach an unreserved/replacement group")
                test.assertEqual(sig, signal.SIGKILL)
                events.append(("signal", group))
                state["killed"] = True

            def monotonic():
                state["clock"] += 0.25
                return state["clock"]

            def read(fd, size):
                if state["killed"] and not escaped_pipes:
                    return b""
                raise BlockingIOError()

            with mock.patch("subprocess.Popen", Process), \
                    mock.patch("os.killpg", killpg), \
                    mock.patch("os.path.getsize", return_value=0), \
                    mock.patch("os.set_blocking"), mock.patch("os.read", read), \
                    mock.patch("select.select", return_value=([], [], [])), \
                    mock.patch("time.monotonic", monotonic), mock.patch("time.sleep"):
                with test.assertRaisesRegex(AssertionError, "hung step: identity fixture"):
                    runner()
            test.assertTrue(state["killed"], events)
            test.assertTrue(all(p.returncode is not None for p in processes), events)
            test.assertEqual(sum(event == "signal" for event, _ in events), 1, events)
            test.assertTrue(all(p.stdin is None or p.stdin.closed for p in processes), events)
