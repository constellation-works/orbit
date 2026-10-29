"""Deterministic identity-lifetime checks for the two embedded process owners.

Run the actual embedded programs with intercepted process APIs. No process is
created or signaled; a reap immediately makes the numeric group a foreign owner.
The public compiler-cache test entrypoint invokes these regression fixtures.
"""

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
import os
import signal
import subprocess
import sys
import tempfile


def group_lifetime_regression(root, namespace_source=None, watchdog_source=None):
    namespace = (namespace_source or root / "scripts/test-compiler-cache-namespaces.sh").read_text()
    watchdog = (watchdog_source or root / "scripts/test-compiler-cache.sh").read_text()
    programs = [
        ("supervisor", namespace.split("<<'PYSUPERVISOR'\n", 1)[1].split("\nPYSUPERVISOR", 1)[0]),
        ("watchdog", watchdog.split("<<'PYTIMEOUT'\n", 1)[1].split("\nPYTIMEOUT", 1)[0]),
    ]
    for name, program in programs:
        for delayed in (False, True):
            with tempfile.TemporaryDirectory(prefix="group-lifetime-", dir=root / ".orbit/tmp") as tmp:
                state = dict(reserved=True, child=True, killed=False, clock=0, signals=0, reaped=0)
                processes = []
                real_path = Path

                read_text = Path.read_text

                def model_read_text(path, *args, **kwargs):
                    if str(path).startswith("/proc/"):
                        if str(path) != "/proc/100/task/100/children":
                            return ""
                        return "101" + (" 103" if name == "supervisor" and len(processes) == 2 else "") + (" 102" if state["child"] else "")
                    return read_text(path, *args, **kwargs)

                class Process:
                    pid = 101
                    returncode = None

                    def __init__(self, argv, **kwargs):
                        self.anchor = "stdin" in kwargs
                        if self.anchor:
                            self.pid = 103
                        else:
                            assert not processes or not state["reserved"], "previous leader was not reaped"
                            state.update(reserved=True, child=True, killed=False)
                        self.code = 0
                        if name == "watchdog":
                            env = kwargs["env"]
                            scenario = env["NS_SCENARIO"]
                            self.code = 0 if scenario == "success" else 1
                            fixture = real_path(env["TMPDIR"]).parent
                            (fixture / "out").write_text("test-compiler-cache-namespaces: ok\n")
                            stages = {
                                "owner-timeout": ("server-side-shared-tcp", "C"),
                                "peer-timeout": ("server-side-shared-tcp", "D"),
                                "failure-cleanup": ("server-side-shared-tcp", "C"),
                                "warm-identical-source": ("warm-identical-source", "E"),
                            }
                            stage, participants = stages.get(scenario, (scenario, "CD"))
                            status = 143 if scenario == "failure-cleanup" else 124
                            (fixture / "err").write_text("stubborn transport ready\n" + "".join(
                                f"stage={stage} participant={p} exit_status={status}\n" for p in participants))
                            (fixture / "events").write_text("")
                        processes.append(self)

                    def poll(self):
                        if delayed and not state["killed"]:
                            return None
                        if self.returncode is None:
                            return self.wait()
                        return self.returncode

                    def wait(self, **kwargs):
                        if self.anchor or len(processes) == 1 or name == "watchdog":
                            state["reserved"] = False
                        if self.returncode is None:
                            state["reaped"] += 1
                        self.returncode = self.code
                        return self.code

                def monotonic():
                    state["clock"] += 0.05
                    return state["clock"]

                def waitid(kind, pid, options):
                    assert kind == os.P_PID and pid == 101
                    assert options & os.WNOWAIT, "exit observation must preserve the leader"
                    assert state["reserved"], "leader observed after losing its reservation"
                    if delayed and not state["killed"]:
                        return None
                    return SimpleNamespace(si_code=os.CLD_EXITED, si_status=processes[-1].code)

                def killpg(pid, sig):
                    assert pid == 101
                    assert state["reserved"], f"{name}: killpg reached a reused foreign process group"
                    state["signals"] += 1
                    if sig == signal.SIGKILL:
                        state["killed"] = True
                        state["child"] = False

                def kill(pid, sig):
                    assert pid in (101, 102)
                    if sig == signal.SIGKILL:
                        state["child"] = False

                def waitpid(pid, options):
                    assert pid != -1, "generic reap can release the leader before the last signal"
                    assert pid == 102, "leader must be reaped only after group cleanup"
                    return (102, 0) if not state["child"] else (0, 0)

                argv = ([name, "1", "transport"] if name == "supervisor" else
                        [name, "namespace-script", tmp, "home", "bin", "sccache"])
                with patch.multiple(os, create=True, **{
                        key: getattr(os, key, fallback) for key, fallback in
                        [("P_PID", 1), ("WEXITED", 4), ("WNOWAIT", 0x1000000), ("CLD_EXITED", 1)]}), \
                        patch("subprocess.Popen", Process), patch("os.waitid", waitid, create=True), \
                        patch("os.waitpid", waitpid), patch("os.killpg", killpg), patch("os.kill", kill), \
                        patch("os.getpid", return_value=100), patch("pathlib.Path.read_text", model_read_text), \
                        patch("ctypes.CDLL", return_value=SimpleNamespace(prctl=lambda *_: 0)), \
                        patch("time.monotonic", monotonic), patch("time.sleep"), \
                        patch.dict(os.environ, {"NS_REAL_CARGO": "cargo", "NS_REAL_HOME": "home"}), \
                        patch.object(sys, "platform", "linux"), patch.object(sys, "argv", argv):
                    try:
                        exec(compile(program, name, "exec"), {})
                    except SystemExit as error:
                        assert name == "supervisor" and error.code == (124 if delayed else 0), error
                    except subprocess.TimeoutExpired:
                        assert name == "watchdog" and delayed
                assert state["signals"] > 0 and state["reaped"] == len(processes)
                assert not state["reserved"] and not state["child"], state
                print(f"group-lifetime: {name} delayed={delayed} passed")
