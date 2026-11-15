"""Deterministic identity-lifetime checks for embedded and shell process owners.

Run the actual embedded programs, and the port allocator's shell release and
cleanup, with intercepted signals. A reap makes the numeric PID a foreign
owner. No foreign process is signaled. The public compiler-cache test
entrypoint invokes these regression fixtures.
"""

from contextlib import redirect_stderr
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
import errno
import io
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import time


def group_lifetime_regression(root, namespace_source=None, watchdog_source=None,
                              anchor_failures=("eagain", "preexec")):
    namespace = (namespace_source or root / "scripts/test-compiler-cache-namespaces.sh").read_text()
    watchdog = (watchdog_source or root / "scripts/test-compiler-cache.sh").read_text()
    programs = [
        ("supervisor", namespace.split("<<'PYSUPERVISOR'\n", 1)[1].split("\nPYSUPERVISOR", 1)[0]),
        ("watchdog", watchdog.split("<<'PYTIMEOUT'\n", 1)[1].split("\nPYTIMEOUT", 1)[0]),
    ]
    for name, program in programs:
        cases = [(delayed, None) for delayed in (False, True)]
        if name == "supervisor":
            cases += [(delayed, failure) for failure in anchor_failures
                      for delayed in (False, True)]
        for delayed, anchor_failure in cases:
            with tempfile.TemporaryDirectory(prefix="group-lifetime-", dir=root / ".orbit/tmp") as tmp:
                state = dict(reserved=True, child=True, killed=False, clock=0, signals=0,
                             reaped=0, child_reaped=False)
                processes = []
                errors = io.StringIO()
                exited = False
                real_path = Path

                read_text = Path.read_text

                def model_read_text(path, *args, **kwargs):
                    if str(path).startswith("/proc/"):
                        if str(path) != "/proc/100/task/100/children":
                            return ""
                        return "101" + (" 103" if name == "supervisor" and len(processes) == 2 else "") + (" 102" if state["child"] and state["listed"] else "")
                    return read_text(path, *args, **kwargs)

                class Process:
                    pid = 101
                    returncode = None

                    def __init__(self, argv, **kwargs):
                        self.anchor = "stdin" in kwargs
                        if self.anchor:
                            if anchor_failure == "eagain":
                                raise OSError(errno.EAGAIN, "fixture anchor fork unavailable")
                            if anchor_failure == "preexec":
                                raise subprocess.SubprocessError("Exception occurred in preexec_fn.")
                            self.pid = 103
                        else:
                            assert not processes or not state["reserved"], "previous leader was not reaped"
                            # A delayed watchdog's adopted descendants are listed only after its leader exits.
                            state.update(reserved=True, child=True, killed=False, pending=False,
                                         listed=name != "watchdog" or not delayed, exited=not delayed,
                                         child_reaped=False)
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
                            server = ""
                            if stage in ("client-side-shared-tcp", "server-side-shared-tcp"):
                                server = (f"stage={stage} sccache_server_log=/fixture/server.log\n"
                                          f"sccache-server-log bytes=1 limit=4096\n"
                                          f"sccache-server-{stage}-end\n")
                            (fixture / "err").write_text("stubborn transport ready\n" + "".join(
                                f"stage={stage} participant={p} exit_status={status}\n" for p in participants) + server)
                            (fixture / "events").write_text("")
                        processes.append(self)

                    def poll(self):
                        if delayed and not state["killed"]:
                            return None
                        if self.returncode is None:
                            return self.wait()
                        return self.returncode

                    def wait(self, **kwargs):
                        if anchor_failure:
                            assert state["killed"], "unanchored leader reaped before the final group signal"
                        if self.anchor or len(processes) == 1 or name == "watchdog":
                            state["reserved"] = False
                        if self.returncode is None:
                            state["reaped"] += 1
                        self.returncode = self.code
                        if name == "watchdog":
                            # Reaping the leader ends its exit, so any adopted descendant must already be dead.
                            state.update(exited=True, listed=True, pending=False)
                            assert not state["child"], "watchdog reaped its leader while an adopted descendant was alive"
                        return self.code

                def monotonic():
                    state["clock"] += 0.05
                    return state["clock"]

                def waitid(kind, pid, options):
                    assert kind == os.P_PID and pid == 101
                    assert options & os.WNOWAIT, "exit observation must preserve the leader"
                    assert state["reserved"], "leader observed after losing its reservation"
                    if not state["exited"]:
                        return None
                    return SimpleNamespace(si_code=os.CLD_EXITED, si_status=processes[-1].code)

                def killpg(pid, sig):
                    assert pid == 101
                    assert state["reserved"], f"{name}: killpg reached a reused foreign process group"
                    state["signals"] += 1
                    if sig == signal.SIGKILL:
                        # The group signal does not reach adopted descendants, which live in their own groups.
                        state.update(killed=True, pending=True)

                def model_sleep(_seconds):
                    # A signaled leader exits at the next scheduling point; its descendants reparent then.
                    if state["pending"]:
                        state.update(pending=False, exited=True, listed=True)

                def kill(pid, sig):
                    assert pid in (101, 102)
                    if pid == 102 and sig == signal.SIGKILL:
                        state["child"] = False

                def waitpid(pid, options):
                    assert pid != -1, "generic reap can release the leader before the last signal"
                    assert pid == 102, "leader must be reaped only after group cleanup"
                    if not state["child"]:
                        state["child_reaped"] = True
                    return (102, 0) if not state["child"] else (0, 0)

                argv = ([name, "1", str(Path(tmp) / "cancel"), "transport"] if name == "supervisor" else
                        [name, "namespace-script", tmp, "home", "bin", "sccache"])
                with patch.multiple(os, create=True, **{
                        key: getattr(os, key, fallback) for key, fallback in
                        [("P_PID", 1), ("WEXITED", 4), ("WNOWAIT", 0x1000000), ("CLD_EXITED", 1)]}), \
                        patch("subprocess.Popen", Process), patch("os.waitid", waitid, create=True), \
                        patch("os.waitpid", waitpid), patch("os.killpg", killpg), patch("os.kill", kill), \
                        patch("os.getpid", return_value=100), patch("pathlib.Path.read_text", model_read_text), \
                        patch("ctypes.CDLL", return_value=SimpleNamespace(prctl=lambda *_: 0)), \
                        patch("time.monotonic", monotonic), patch("time.sleep", model_sleep), \
                        patch.dict(os.environ, {"NS_REAL_CARGO": "cargo", "NS_REAL_HOME": "home"}), \
                        patch.object(sys, "platform", "linux"), patch.object(sys, "argv", argv), \
                        redirect_stderr(errors):
                    try:
                        exec(compile(program, name, "exec"), {})
                    except SystemExit as error:
                        exited = True
                        expected = 125 if anchor_failure else (124 if delayed else 0)
                        assert name == "supervisor" and error.code == expected, error
                    except subprocess.TimeoutExpired:
                        assert name == "watchdog" and delayed
                    except (OSError, subprocess.SubprocessError) as error:
                        raise AssertionError(f"{anchor_failure}: startup failure escaped cleanup: {state}") from error
                if anchor_failure:
                    assert exited, "anchor startup failure did not exit nonzero"
                    diagnostic = "fixture anchor fork unavailable" if anchor_failure == "eagain" else "preexec_fn"
                    assert diagnostic in errors.getvalue(), "anchor startup failure was not reported"
                assert state["signals"] > 0 and state["reaped"] == len(processes)
                assert not state["reserved"] and not state["child"] and state["child_reaped"], state
                print(f"group-lifetime: {name} delayed={delayed} anchor_failure={anchor_failure} passed")


def _extract_shell_function(source, name):
    header = f"{name}() {{"
    start = source.index(header)
    end = source.index("\n}\n", start)
    return source[start:end + 2]


def _allocator_shell_functions(source):
    """Release and cleanup, plus the shared reaper once the fix adds one."""
    chunks = []
    if "\nreap_port_allocator() {" in source or source.startswith("reap_port_allocator() {"):
        chunks.append(_extract_shell_function(source, "reap_port_allocator"))
    chunks.append(_extract_shell_function(source, "release_port"))
    chunks.append(_extract_shell_function(source, "cleanup"))
    return "\n".join(chunks)


def _signal_lines(trace):
    return [line for line in trace.splitlines() if line.startswith("SIGNAL ")]


def _line_mentions_pid(line, pid):
    return pid in line.replace("=", " ").split()


def _run_allocator_shell(root, functions, body, timeout):
    """Run extracted allocator functions. Every kill is intercepted."""
    with tempfile.TemporaryDirectory(prefix="allocator-identity-", dir=root / ".orbit/tmp") as tmp:
        directory = Path(tmp)
        guard = directory / "bin"
        guard.mkdir()
        kill = guard / "kill"
        kill.write_text("#!/bin/sh\nprintf 'KILLCALL %s\\n' \"$*\" >>\"${TRACE:?}\"\nexit 0\n")
        kill.chmod(0o755)
        trace = directory / "trace"
        script = directory / "run.sh"
        # Shell braces must not go through an f-string. Only the exported paths
        # are interpolated, then concatenated with the extracted functions.
        header = (
            "set -euo pipefail\n"
            + f"export TRACE={shlex.quote(str(trace))}\n"
            + f"export CASE_DIR={shlex.quote(str(directory))}\n"
            + f"export PATH={shlex.quote(str(guard))}:$PATH\n"
            + "cd \"$CASE_DIR\"\n"
            + "enable -n kill\n"
            + "wait() {\n"
            + "  printf 'wait %s\\n' \"$*\" >>\"$TRACE\"\n"
            + "  builtin wait \"$@\"\n"
            + "}\n"
            + "kill() {\n"
            + "  printf 'KILLCALL %s\\n' \"$*\" >>\"$TRACE\"\n"
            + "  if [[ \"${1:-}\" == \"-0\" ]]; then\n"
            + "    return 1\n"
            + "  fi\n"
            + "  printf 'SIGNAL %s\\n' \"$*\" >>\"$TRACE\"\n"
            + "  return 0\n"
            + "}\n"
        )
        script.write_text(header + functions + "\n" + body)
        completed = subprocess.run(
            ["bash", str(script)],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        logged = trace.read_text() if trace.exists() else ""
        if completed.returncode != 0:
            raise AssertionError(
                f"allocator shell exited {completed.returncode}\n"
                f"{completed.stdout}\n{completed.stderr}\n{logged}"
            )
        return completed.stdout, logged


def _assert_no_delivered_signal(trace, message):
    delivered = _signal_lines(trace)
    assert not delivered, f"{message}\n" + "\n".join(delivered)


def participant_identity_regression(root, scratch, home, fixture_bin, sccache):
    """EXIT after a completed/reused participant must never signal its number.

    Drive the public namespace script with the same transport fixtures as its
    diagnostic controls. Bash's environment hook models reuse immediately
    after waiting for the first real participant. Every shell signal is
    intercepted; no PID churn or foreign process is involved.
    """
    fixture = scratch / "participant-identity"
    (fixture / "scratch").mkdir(parents=True)
    hook = fixture / "bash-env"
    hook.write_text(r'''
if [[ "$0" == */test-compiler-cache-namespaces.sh ]]; then
  kill() { printf 'SIGNAL %s\n' "$*" >>"$NS_IDENTITY_TRACE"; return 0; }
  wait() {
    if [[ "${1:-}" == 424242 ]]; then
      printf 'wait foreign-number marker=%s\n' "$(test -f "${LOGFILES[0]}.cancel" && echo present)" >>"$NS_IDENTITY_TRACE"
      return 127
    fi
    local status=0
    builtin wait "$@" || status=$?
    if [[ -z "${identity_reused:-}" ]]; then
      identity_reused=1
      PIDS[0]=424242
      printf 'completed participant reaped; number now foreign\n' >>"$NS_IDENTITY_TRACE"
      exit 1
    fi
    return "$status"
  }
fi
''')
    env = dict(HOME=str(home), PATH=f"{fixture_bin}:/usr/bin:/bin",
               TMPDIR=str(fixture / "scratch"), ORBIT_COMPILER_CACHE_BIN=str(sccache),
               ORBIT_COMPILER_CACHE_REQUIRE_NS="1", NS_REAL_CARGO=os.environ["NS_REAL_CARGO"],
               NS_REAL_HOME=os.environ["NS_REAL_HOME"], NS_DIAGNOSTIC_EVENTS=str(fixture / "events"),
               BASH_ENV=str(hook), NS_IDENTITY_TRACE=str(fixture / "trace"),
               ORBIT_COMPILER_CACHE_NS_WAIT_SECONDS="2")
    child = subprocess.run([str(root / "scripts/test-compiler-cache-namespaces.sh")],
                           env=env, capture_output=True, text=True, timeout=10)
    trace = (fixture / "trace").read_text()
    assert child.returncode == 1, (child.returncode, child.stdout, child.stderr)
    assert "completed participant reaped; number now foreign" in trace, trace
    _assert_no_delivered_signal(trace, "namespace EXIT cleanup signaled a reused participant number")
    assert "wait foreign-number marker=present" in trace, trace
    assert "stage=isolated-daemon participant=C exit_status=127" in child.stderr, child.stderr
    assert "participant=D exit_status=" in child.stderr, child.stderr
    assert not list((fixture / "scratch").iterdir()), "namespace scratch survived cleanup"
    print("participant-identity: public namespace EXIT after completed/reused participant passed")


def allocator_identity_regression(root):
    """Exited or reused allocator PIDs are never signaled.

    The intercepted kill admits numeric reuse. Release still publishes the
    marker and reaps the owned child. Participant cleanup uses markers too.
    A live allocator must close its socket and exit within a finite bound.
    """
    source_path = root / "scripts/test-compiler-cache-namespaces.sh"
    source = source_path.read_text()
    functions = _allocator_shell_functions(source)
    foreign = "424242"
    participant = "424243"

    # The liveness probe observes the owned allocator has exited and admits
    # reuse of its number. A later delivered signal would hit that foreign owner.
    exited = f"""
owner=exited
kill() {{
  if [[ "${{1:-}}" == "-0" ]]; then
    printf 'probe pid=%s owner=%s -> ESRCH; reuse admitted\\n' "$2" "$owner" >>"$TRACE"
    owner=foreign
    return 1
  fi
  printf 'SIGNAL args=%s owner=%s\\n' "$*" "$owner" >>"$TRACE"
  return 0
}}
HOLD_RELEASE="$CASE_DIR/release-marker"
HOLD_PID={foreign}
release_port
if [[ -f "$CASE_DIR/release-marker" ]]; then
  printf 'marker=present\\n'
else
  printf 'marker=missing\\n'
fi
printf 'cleared_pid=%s cleared_release=%s\\n' "$HOLD_PID" "$HOLD_RELEASE"
"""
    stdout, trace = _run_allocator_shell(root, functions, exited, timeout=10)
    assert "marker=present" in stdout, stdout
    assert "cleared_pid= cleared_release=" in stdout, stdout
    assert any(line == f"wait {foreign}" for line in trace.splitlines()), trace
    _assert_no_delivered_signal(trace, "release_port signaled a PID after reuse was admitted")
    print("allocator-identity: release exited-reused passed")

    # A successful liveness probe does not reserve the PID. Replacing the
    # unconditional kill with check-then-signal still delivers to a foreign owner.
    probed = f"""
owner=owned
sleep() {{ :; }}
kill() {{
  if [[ "${{1:-}}" == "-0" ]]; then
    printf 'probe pid=%s owner=%s\\n' "$2" "$owner" >>"$TRACE"
    owner=foreign
    return 0
  fi
  printf 'SIGNAL args=%s owner=%s\\n' "$*" "$owner" >>"$TRACE"
  return 0
}}
HOLD_RELEASE="$CASE_DIR/release-marker"
HOLD_PID={foreign}
release_port
if [[ -f "$CASE_DIR/release-marker" ]]; then
  printf 'marker=present\\n'
else
  printf 'marker=missing\\n'
fi
printf 'cleared_pid=%s cleared_release=%s\\n' "$HOLD_PID" "$HOLD_RELEASE"
"""
    stdout, trace = _run_allocator_shell(root, functions, probed, timeout=10)
    assert "marker=present" in stdout, stdout
    assert "cleared_pid= cleared_release=" in stdout, stdout
    _assert_no_delivered_signal(
        trace, "release_port signaled a PID after a liveness probe relinquished it")
    print("allocator-identity: release check-then-signal passed")

    # EXIT cleanup of an already reaped allocator must not signal its number.
    # Participant teardown and failure diagnostics stay in place.
    cleanup_model = f"""
report_participants() {{ printf 'report-participants\\n' >>"$TRACE"; }}
rm() {{ printf 'rm %s\\n' "$*" >>"$TRACE"; }}
kill() {{
  printf 'SIGNAL args=%s owner=foreign-after-reap\\n' "$*" >>"$TRACE"
  return 0
}}
HOLD_RELEASE="$CASE_DIR/release-marker"
HOLD_PID={foreign}
PIDS=({participant})
LOGFILES=("$CASE_DIR/participant")
STATUSES=()
WORKDIR="$CASE_DIR/owned-scratch"
mkdir -p "$WORKDIR"
set +e
false
cleanup
if [[ -f "$CASE_DIR/release-marker" ]]; then
  printf 'marker=present\\n'
else
  printf 'marker=missing\\n'
fi
"""
    stdout, trace = _run_allocator_shell(root, functions, cleanup_model, timeout=10)
    assert "marker=present" in stdout, stdout + trace
    _assert_no_delivered_signal(trace, "cleanup must cancel participants without numeric signals")
    assert not any(line.startswith("SIGNAL ") and _line_mentions_pid(line, foreign) for line in trace.splitlines()), trace
    assert "report-participants" in trace, trace
    assert any(line.startswith("rm ") and "owned-scratch" in line for line in trace.splitlines()), trace
    print("allocator-identity: cleanup foreign-allocator passed")

    live_functions = "\n".join((
        _extract_shell_function(source, "alloc_port"),
        functions,
    ))
    live = r"""
fail() { printf 'fail %s\n' "$*" >&2; exit 1; }
release_one() {
  local label="$1"
  alloc_port
  local port="$ALLOC_PORT" marker="$HOLD_RELEASE"
  release_port
  python3 -c 'import socket,sys
s=socket.socket()
s.bind(("127.0.0.1", int(sys.argv[1])))
print(s.getsockname()[1])' "$port" >/dev/null
  printf 'released %s port=%s marker=%s\n' "$label" "$port" "$marker"
}
WORKDIR="$CASE_DIR/work"
mkdir -p "$WORKDIR"
release_one client-side-shared-tcp
release_one server-side-shared-tcp
alloc_port
saved="$HOLD_PID"
port="$ALLOC_PORT"
PIDS=(424243)
LOGFILES=("$CASE_DIR/participant")
STATUSES=()
cleanup
python3 -c 'import socket,sys
s=socket.socket()
s.bind(("127.0.0.1", int(sys.argv[1])))
print(s.getsockname()[1])' "$port" >/dev/null
if [[ -d "$WORKDIR" ]]; then
  printf 'workdir-survived\n' >&2
  exit 1
fi
printf 'cleanup-reaped pid=%s port=%s\n' "$saved" "$port"
"""
    started = time.monotonic()
    stdout, trace = _run_allocator_shell(root, live_functions, live, timeout=15)
    elapsed = time.monotonic() - started
    assert elapsed < 8, f"allocator release exceeded the finite bound: {elapsed:.3f}s"
    assert "released client-side-shared-tcp " in stdout, stdout
    assert "released server-side-shared-tcp " in stdout, stdout
    assert "cleanup-reaped " in stdout, stdout
    assert "workdir-survived" not in stdout
    allocator_pids = []
    for line in stdout.splitlines():
        if line.startswith("cleanup-reaped "):
            allocator_pids.append(line.split("pid=", 1)[1].split()[0])
    assert allocator_pids and allocator_pids[0].isdigit(), stdout
    owned = allocator_pids[0]
    assert not any(line.startswith("SIGNAL ") and _line_mentions_pid(line, owned) for line in trace.splitlines()), trace
    _assert_no_delivered_signal(trace, "live allocator cleanup must not signal participant numbers")
    print(f"allocator-identity: shared-tcp release and cleanup passed elapsed={elapsed:.3f}s")
