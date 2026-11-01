#!/usr/bin/env python3
"""Exercise build-budget benchmark path admission with command shims."""

import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import tempfile
import time


TRANSPORT = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import subprocess
import sys

tool = Path(sys.argv[0]).name
args = sys.argv[1:]
event = {"tool": tool, "args": args}
events = Path(os.environ["BENCH_EVENTS"])
work = Path(os.environ["BENCH_WORK"]).resolve()


def record():
    with events.open("a", encoding="utf-8") as output:
        output.write(json.dumps(event) + "\n")


if tool == "git" and args == ["-C", os.environ["BENCH_REPO"], "rev-parse", "HEAD"]:
    record()
    print("fixture-head")
    raise SystemExit(0)
if tool == "git" and len(args) == 4 and args[2] == "archive":
    record()
    if os.environ.get("BENCH_ARCHIVE_FAIL") == "1":
        raise SystemExit(19)
    raise SystemExit(subprocess.call([
        os.environ["REAL_TAR"], "-cf", "-", "-C", os.environ["BENCH_SOURCE"], "."
    ]))
if tool == "rustc" and args == ["--version"]:
    record()
    print("rustc fixture")
    raise SystemExit(0)
if tool == "cargo" and args == ["--version"]:
    record()
    print("cargo fixture")
    raise SystemExit(0)
if tool == "cargo" and args[:2] == ["check", "-p"]:
    event.update(
        target=os.environ.get("CARGO_TARGET_DIR"),
        build_jobs=os.environ.get("CARGO_BUILD_JOBS"),
        orbit_jobs=os.environ.get("ORBIT_CARGO_JOBS"),
        lock_dir=os.environ.get("ORBIT_BUILD_BUDGET_DIR"),
        slots=os.environ.get("ORBIT_BUILD_SLOTS"),
    )
    record()
    block = os.environ.get("BENCH_BLOCK")
    if block:
        # Cancellation fixture: start a descendant, publish readiness, then
        # block on a FIFO that only the harness can release.
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
        ready = Path(block) / "ready.json"
        pending = ready.with_suffix(".tmp")
        pending.write_text(json.dumps({"pids": [os.getpid(), child.pid]}), encoding="utf-8")
        pending.rename(ready)
        with open(Path(block) / "release", "rb") as release:
            release.read()
        child.wait()
        raise SystemExit(0)
    target = Path(event["target"])
    if not target.is_dir() or target.is_symlink():
        raise SystemExit("target was not an owned directory")
    if target.resolve() == work or work not in target.resolve().parents:
        raise SystemExit("target escaped the benchmark workdir")
    if event["build_jobs"] != "1" or os.environ.get("CARGO_INCREMENTAL") != "0":
        raise SystemExit("current arm was not bounded")
    if event["orbit_jobs"] is not None and (event["orbit_jobs"] != "1" or event["slots"] != "1"):
        raise SystemExit("budgeted arm was not bounded")
    if event["lock_dir"] is not None and Path(event["lock_dir"]) != work / "locks":
        raise SystemExit("lock directory escaped the benchmark workdir")
    raise SystemExit(0)
if tool == "rm":
    event["owned_paths"] = []
    event["unsafe"] = False
    for arg in args:
        if arg.startswith("-"):
            continue
        path = Path(arg)
        try:
            resolved = path.resolve(strict=False)
        except OSError:
            resolved = path
        if resolved == work or work not in resolved.parents:
            event["unsafe"] = True
        else:
            event["owned_paths"].append(str(resolved))
    ready = os.environ.get("BENCH_BLOCK")
    if ready and (Path(ready) / "ready.json").exists():
        event["live_pids"] = [
            pid for pid in json.loads((Path(ready) / "ready.json").read_text(encoding="utf-8"))["pids"]
            if Path("/proc", str(pid), "stat").exists()
            and Path("/proc", str(pid), "stat").read_text().rsplit(")", 1)[1].split()[0] != "Z"
        ]
    record()
    if event["unsafe"]:
        raise SystemExit("cleanup escaped the benchmark workdir")
    if os.environ.get("BENCH_ALLOW_RM") == "1":
        raise SystemExit(subprocess.call([os.environ["REAL_RM"], *args]))
    raise SystemExit(0)
raise SystemExit("unexpected command shim: " + tool + " " + repr(args))
'''


def read_events(path):
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def make_fixture(root, script, name, *, archive_fail=False, allow_rm=False):
    case = root / name
    case.mkdir()
    work = case / "work"
    events = case / "events.jsonl"
    tools = case / "bin"
    source = case / "source"
    tools.mkdir()
    source.mkdir()
    (source / "fixture.txt").write_text("archived fixture\n", encoding="utf-8")
    transport = tools / "transport"
    transport.write_text(TRANSPORT, encoding="utf-8")
    transport.chmod(0o755)
    for name in ("git", "cargo", "rustc", "rm"):
        (tools / name).symlink_to(transport)
    env = dict(os.environ)
    env.update(
        PATH=str(tools) + os.pathsep + env["PATH"],
        BENCH_EVENTS=str(events),
        BENCH_WORK=str(work),
        BENCH_REPO=str(script.parents[1]),
        BENCH_SOURCE=str(source),
        BENCH_ARCHIVE_FAIL="1" if archive_fail else "0",
        BENCH_ALLOW_RM="1" if allow_rm else "0",
        REAL_TAR=shutil.which("tar") or "/bin/tar",
        REAL_RM=shutil.which("rm") or "/bin/rm",
        ORBIT_BUILD_BUDGET_BENCH_DIR=str(work),
        ORBIT_BUILD_BUDGET_BENCH_PROCESSES="1",
        ORBIT_BUILD_BUDGET_BENCH_SLOTS="1",
        ORBIT_BUILD_BUDGET_BENCH_JOBS="1",
    )
    env.pop("ORBIT_BUILD_BUDGET_BENCH_RESULT", None)
    env.pop("ORBIT_BUILD_BUDGET_BENCH_OWNER", None)
    env.pop("ORBIT_BUILD_BUDGET_DIR", None)
    env.pop("ORBIT_CARGO_JOBS", None)
    env.pop("ORBIT_BUILD_SLOTS", None)
    env.pop("ORBIT_BUILD_BUDGET_HELD", None)
    return case, work, events, env


def run(script, env, *args):
    return subprocess.run(
        ["bash", str(script), *args], env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
    )


def scenario_existing_root(root, script):
    case, work, events, env = make_fixture(root, script, "existing-root", archive_fail=True)
    for relative in ("trees/1", "targets", "locks"):
        path = work / relative
        path.mkdir(parents=True, exist_ok=True)
        (path / "sentinel").write_text(relative + "\n", encoding="utf-8")
    result_sentinel = work / "results.tsv"
    result_sentinel.write_text("operator result\n", encoding="utf-8")
    result = run(script, env)
    errors = []
    if result.returncode == 0:
        errors.append("existing workdir was accepted")
    if read_events(events):
        errors.append("a command or cleanup ran before rejecting the existing workdir")
    for relative in ("trees/1", "targets", "locks"):
        if (work / relative / "sentinel").read_text(encoding="utf-8") != relative + "\n":
            errors.append("pre-existing sentinel changed: " + relative)
    if result_sentinel.read_text(encoding="utf-8") != "operator result\n":
        errors.append("pre-existing result changed")
    # An existing empty directory is refused too. The result sentinel above is
    # refused by its own check, so only this case isolates the workdir refusal.
    case, work, events, env = make_fixture(root, script, "existing-empty-root")
    work.mkdir()
    result = run(script, env)
    if result.returncode == 0:
        errors.append("existing empty workdir was accepted")
    if read_events(events):
        errors.append("a command or cleanup ran before rejecting the empty workdir")
    return errors


def scenario_root_symlink(root, script):
    case, work, events, env = make_fixture(root, script, "root-symlink")
    target = case / "operator-data"
    target.mkdir()
    sentinel = target / "sentinel"
    sentinel.write_text("keep\n", encoding="utf-8")
    work.symlink_to(target, target_is_directory=True)
    result = run(script, env)
    errors = []
    if result.returncode == 0:
        errors.append("symlinked workdir was accepted")
    if read_events(events):
        errors.append("a command or cleanup ran before rejecting the workdir symlink")
    if sentinel.read_text(encoding="utf-8") != "keep\n":
        errors.append("symlink target sentinel changed")
    # A dangling symlink is refused too. Without the symlink check, the workdir
    # resolves to the missing target and the claim creates it.
    case, work, events, env = make_fixture(root, script, "root-dangling-symlink")
    missing = case / "missing-target"
    work.symlink_to(missing, target_is_directory=True)
    result = run(script, env)
    if result.returncode == 0:
        errors.append("dangling workdir symlink was accepted")
    if read_events(events):
        errors.append("a command or cleanup ran before rejecting the dangling workdir symlink")
    if missing.exists():
        errors.append("dangling symlink target was created")
    return errors


def scenario_internal_symlink(root, script, destination):
    case, work, events, env = make_fixture(root, script, "internal-" + destination)
    work.mkdir()
    token = "fixture-owner-token"
    (work / ".orbit-build-budget-owner").write_text(token + "\n", encoding="utf-8")
    for name in ("trees", "targets", "locks"):
        (work / name).mkdir()
    outside = case / "operator-data"
    outside.mkdir()
    sentinel = outside / "sentinel"
    sentinel.write_text("keep\n", encoding="utf-8")
    processes = 2 if destination == "target" else 1
    if destination == "tree":
        (work / "trees" / "1").symlink_to(outside, target_is_directory=True)
    else:
        for index in range(1, processes + 1):
            (work / "trees" / str(index)).mkdir()
        (work / "targets" / "current-2").symlink_to(outside, target_is_directory=True)
    result_path = work / "results.tsv"
    result_path.write_text("header\n", encoding="utf-8")
    env.update(
        ORBIT_BUILD_BUDGET_BENCH_DIR=str(work),
        ORBIT_BUILD_BUDGET_BENCH_RESULT=str(result_path),
        ORBIT_BUILD_BUDGET_BENCH_OWNER=token,
        ORBIT_BUILD_BUDGET_BENCH_PROCESSES=str(processes),
    )
    result = run(script, env, "__arm", "current")
    errors = []
    if result.returncode == 0:
        errors.append("internal arm accepted a symlinked " + destination + " destination")
    if any(event["tool"] == "cargo" for event in read_events(events)):
        errors.append("Cargo ran before rejecting the symlinked " + destination)
    if read_events(events):
        errors.append("an external command ran before rejecting the symlinked " + destination)
    if destination == "target" and (work / "targets" / "current-1").exists():
        errors.append("a target directory was created before all destinations were checked")
    if sentinel.read_text(encoding="utf-8") != "keep\n":
        errors.append("internal-arm sentinel changed")
    return errors


def scenario_result_escape(root, script):
    case, work, events, env = make_fixture(root, script, "result-escape")
    outside = case / "operator-result.tsv"
    outside.write_text("keep\n", encoding="utf-8")
    env["ORBIT_BUILD_BUDGET_BENCH_RESULT"] = str(outside)
    result = run(script, env)
    errors = []
    if result.returncode == 0:
        errors.append("result override outside the workdir was accepted")
    if work.exists():
        errors.append("invalid result override acquired a workdir")
    if read_events(events):
        errors.append("a command ran before rejecting the result override")
    if outside.read_text(encoding="utf-8") != "keep\n":
        errors.append("external result sentinel changed")
    return errors


def scenario_archive_failure(root, script):
    case, work, events, env = make_fixture(root, script, "archive-failure", archive_fail=True, allow_rm=True)
    outside = case / "operator-data"
    outside.mkdir()
    sentinel = outside / "sentinel"
    sentinel.write_text("keep\n", encoding="utf-8")
    result = run(script, env)
    recorded = read_events(events)
    cleanup = [event for event in recorded if event["tool"] == "rm"]
    errors = []
    if result.returncode == 0:
        errors.append("injected archive failure succeeded")
    if not any(event["tool"] == "git" and len(event["args"]) == 4 for event in recorded):
        errors.append("archive failure was not reached")
    if any(event["tool"] == "cargo" for event in recorded):
        errors.append("Cargo ran after archive failure")
    if not cleanup:
        errors.append("owned setup directories were not cleaned after archive failure")
    if any(event.get("unsafe") for event in cleanup):
        errors.append("archive-failure cleanup escaped the owned workdir")
    expected = {str((work / name).resolve()) for name in ("trees", "targets", "locks")}
    removed = {path for event in cleanup for path in event["owned_paths"]}
    if removed != expected:
        errors.append("cleanup did not target only the three owned benchmark directories")
    if any((work / name).exists() for name in ("trees", "targets", "locks")):
        errors.append("owned setup directories survived archive-failure cleanup")
    if (work / "results.tsv").exists():
        errors.append("result output was created before archive setup succeeded")
    if sentinel.read_text(encoding="utf-8") != "keep\n":
        errors.append("external sentinel changed during archive-failure cleanup")
    return errors


def scenario_fresh_run(root, script):
    case, work, events, env = make_fixture(root, script, "fresh-run", allow_rm=True)
    env["ORBIT_BUILD_BUDGET_BENCH_RESULT"] = "comparison.tsv"
    result = run(script, env)
    recorded = read_events(events)
    cargo = [event for event in recorded if event["tool"] == "cargo" and event["args"][:2] == ["check", "-p"]]
    errors = []
    if result.returncode != 0:
        logs = "".join(
            "\n" + name + ":\n" + (work / name).read_text(encoding="utf-8")[-1500:]
            for name in ("current.log", "budgeted.log") if (work / name).exists()
        )
        errors.append(
            f"fresh benchmark failed (exit {result.returncode}): "
            + result.stderr[-1000:] + result.stdout[-1000:] + logs
        )
    if len(cargo) != 2:
        errors.append("fresh benchmark did not execute both bounded fake arms")
    if len({event.get("target") for event in cargo}) != 2:
        errors.append("benchmark arms did not use separate private targets")
    if any(event.get("unsafe") for event in recorded if event["tool"] == "rm"):
        errors.append("fresh cleanup escaped the owned workdir")
    if not (work / "comparison.tsv").is_file():
        errors.append("result override was not written inside the owned workdir")
    if any((work / name).exists() for name in ("trees", "targets", "locks")):
        errors.append("owned benchmark directories were not cleaned")
    if not (work / ".orbit-build-budget-owner").is_file():
        errors.append("workdir ownership marker was not retained")
    return errors


def pid_alive(pid):
    try:
        state = Path("/proc", str(pid), "stat").read_text(encoding="utf-8").rsplit(")", 1)[1].split()[0]
    except OSError:
        return False
    return state != "Z"


def read_arm_proc_identity(pid):
    """Return (process group, start time) for one live or zombie pid."""
    text = Path("/proc", str(pid), "stat").read_text(encoding="utf-8")
    fields = text.rsplit(")", 1)[1].split()
    return int(fields[2]), int(fields[19])


def arm_group_members(pgid):
    members = []
    try:
        entries = list(Path("/proc").iterdir())
    except OSError:
        return members
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            pgrp, _start = read_arm_proc_identity(int(entry.name))
        except (OSError, ValueError, IndexError):
            continue
        if pgrp == pgid:
            members.append(int(entry.name))
    return members


def pidfd_is_alive(fd):
    """A pidfd becomes readable when that incarnation has exited."""
    readable, _, _ = select.select([fd], [], [], 0)
    return not readable


def pin_arm_pid(pid, pgid):
    """Open a pidfd only when `pid` is stably the arm group member it was."""
    try:
        pgrp, start = read_arm_proc_identity(pid)
    except (OSError, ValueError, IndexError):
        return None
    if pgrp != pgid:
        return None
    try:
        fd = os.pidfd_open(pid)
    except OSError:
        return None
    try:
        try:
            again = read_arm_proc_identity(pid)
        except (OSError, ValueError, IndexError):
            os.close(fd)
            return None
        if again != (pgrp, start) or not pidfd_is_alive(fd):
            os.close(fd)
            return None
        return fd
    except BaseException:
        os.close(fd)
        raise


class OwnedArmIdentity:
    """Reservations for an arm group this fixture cannot join.

    `setsid` puts the arm in another session, so an unreaped anchor cannot
    enter that group. Each pidfd pins one incarnation observed while the arm
    was blocked. Later signals go through those fds and only while the
    incarnation is still alive; the fds are then closed. A saved numeric
    PID or PGID is never signaled.
    """

    def __init__(self):
        self.fds = []
        self.retired = False

    def pin(self, pgid, extra_pids):
        seen = []
        for pid in (pgid, *extra_pids, *arm_group_members(pgid)):
            if pid in seen or not isinstance(pid, int) or pid <= 0:
                continue
            seen.append(pid)
            fd = pin_arm_pid(pid, pgid)
            if fd is not None:
                self.fds.append(fd)

    def signal_and_retire(self):
        if self.retired:
            return
        self.retired = True
        try:
            for fd in self.fds:
                try:
                    alive = pidfd_is_alive(fd)
                except OSError:
                    continue
                if not alive:
                    continue
                try:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                except OSError:
                    pass
        finally:
            for fd in self.fds:
                try:
                    os.close(fd)
                except OSError:
                    pass
            self.fds = []


def retire_cancellation_processes(reservation, process):
    """Signal only identities still reserved, then the unreaped benchmark."""
    try:
        reservation.signal_and_retire()
    finally:
        if process.poll() is None:
            process.kill()
            try:
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                pass


def scenario_cancellation(root, script):
    case, work, events, env = make_fixture(root, script, "cancellation", allow_rm=True)
    block = case / "block"
    block.mkdir()
    os.mkfifo(block / "release")
    env["BENCH_BLOCK"] = str(block)
    process = subprocess.Popen(
        ["bash", str(script)], env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    errors = []
    pids = []
    try:
        deadline = time.monotonic() + 20
        while not (block / "ready.json").exists():
            if process.poll() is not None or time.monotonic() > deadline:
                errors.append("benchmark did not reach the blocked fake Cargo arm")
                return errors
            time.sleep(0.02)
        pids = json.loads((block / "ready.json").read_text(encoding="utf-8"))["pids"]
        process.send_signal(signal.SIGTERM)
        try:
            process.communicate(timeout=20)
        except subprocess.TimeoutExpired:
            errors.append("benchmark did not exit after SIGTERM")
            return errors
        if process.returncode != 143:
            errors.append(f"cancellation did not report SIGTERM failure (exit {process.returncode})")
        survivors = [pid for pid in pids if pid_alive(pid)]
        if survivors:
            errors.append(f"arm processes survived the benchmark command: {survivors}")
        cleanup = [event for event in read_events(events) if event["tool"] == "rm"]
        if not cleanup:
            errors.append("owned directories were not cleaned after cancellation")
        if any(event.get("live_pids") for event in cleanup):
            errors.append("cleanup removed owned directories while the arm was still running")
        if any((work / name).exists() for name in ("trees", "targets", "locks")):
            errors.append("owned directories survived cancellation cleanup")
    finally:
        for pid in pids:
            if pid_alive(pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
        if process.poll() is None:
            process.kill()
            process.communicate()
    return errors


def scenario_cancellation_spawn_window(root, script):
    case, work, events, env = make_fixture(root, script, "cancellation-spawn-window", allow_rm=True)
    block = case / "block"
    block.mkdir()
    os.mkfifo(block / "release")
    os.mkfifo(block / "parent-release")
    env["BENCH_BLOCK"] = str(block)
    env["RACE_ROOT"] = str(block)
    hook = case / "bash-env"
    hook.write_text(
        "set -T\n"
        "race_hook() {\n"
        "  if [[ \"$BASH_COMMAND\" == 'ARM_PGID=\"$group_pid\"' && \"$BASHPID\" == \"$$\" ]]; then\n"
        "    trap - DEBUG\n"
        "    printf '%s\\n' \"$group_pid\" > \"$RACE_ROOT/arm-pgid\"\n"
        "    exec 9<> \"$RACE_ROOT/parent-release\"\n"
        "    IFS= read -r -t 5 unused <&9 || true\n"
        "  fi\n"
        "}\n"
        "trap race_hook DEBUG\n",
        encoding="utf-8",
    )
    env["BASH_ENV"] = str(hook)
    process = subprocess.Popen(
        ["bash", str(script)], env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    errors = []
    pids = []
    reservation = OwnedArmIdentity()
    try:
        deadline = time.monotonic() + 20
        while not ((block / "ready.json").exists() and (block / "arm-pgid").exists()):
            if process.poll() is not None or time.monotonic() > deadline:
                errors.append("benchmark did not reach the spawn-window (ready + arm identity)")
                return errors
            time.sleep(0.02)
        try:
            pgid = int((block / "arm-pgid").read_text(encoding="utf-8").strip())
            pids = json.loads((block / "ready.json").read_text(encoding="utf-8"))["pids"]
        except (OSError, ValueError, KeyError):
            errors.append("spawn-window arm identity was not recorded")
            return errors
        # The DEBUG hook is still blocked, so stop_arm has not reaped this group.
        if process.poll() is None:
            reservation.pin(pgid, pids)
            if process.poll() is None and not reservation.fds:
                errors.append("spawn-window arm identity was not reserved")
        process.send_signal(signal.SIGTERM)
        try:
            process.communicate(timeout=20)
        except subprocess.TimeoutExpired:
            errors.append("benchmark did not exit after SIGTERM in the spawn window")
            return errors
        if process.returncode != 143:
            errors.append(f"spawn-window cancellation did not report SIGTERM failure (exit {process.returncode})")
        survivors = [pid for pid in pids if pid_alive(pid)]
        if survivors:
            errors.append(f"arm processes survived the spawn-window cancellation: {survivors}")
        cleanup = [event for event in read_events(events) if event["tool"] == "rm"]
        if not cleanup:
            errors.append("owned directories were not cleaned after spawn-window cancellation")
        if any(event.get("live_pids") for event in cleanup):
            errors.append("cleanup removed owned directories while the arm was still running")
        if any((work / name).exists() for name in ("trees", "targets", "locks")):
            errors.append("owned directories survived spawn-window cancellation cleanup")
    finally:
        retire_cancellation_processes(reservation, process)
    return errors


def scenario_cancellation_flag_clear(root, script):
    case, work, events, env = make_fixture(root, script, "cancellation-flag-clear", allow_rm=True)
    block = case / "block"
    block.mkdir()
    os.mkfifo(block / "release")
    os.mkfifo(block / "parent-release")
    env["BENCH_BLOCK"] = str(block)
    env["RACE_ROOT"] = str(block)
    hook = case / "bash-env"
    hook.write_text(
        "set -T\n"
        "flag_hold() {\n"
        "  if [[ \"${ARM_SPAWNING:-0}\" == 1 && \"$BASHPID\" == \"$$\" ]]; then\n"
        "    trap - DEBUG\n"
        "    printf 'held\\n' > \"$RACE_ROOT/flag-held\"\n"
        "    exec 9<> \"$RACE_ROOT/parent-release\"\n"
        "    IFS= read -r -t 5 unused <&9 || true\n"
        "    trap record_arm DEBUG\n"
        "  fi\n"
        "}\n"
        "record_arm() {\n"
        "  if [[ \"$BASH_COMMAND\" == 'ARM_PGID=\"$group_pid\"' && \"$BASHPID\" == \"$$\" ]]; then\n"
        "    trap - DEBUG\n"
        "    printf '%s\\n' \"$group_pid\" > \"$RACE_ROOT/arm-pgid\"\n"
        "    local waited\n"
        "    for waited in $(seq 1 250); do\n"
        "      if [[ -s \"$RACE_ROOT/ready.json\" ]]; then\n"
        "        break\n"
        "      fi\n"
        "      sleep 0.02\n"
        "    done\n"
        "    for waited in $(seq 1 250); do\n"
        "      if [[ -f \"$RACE_ROOT/identity-pinned\" ]]; then\n"
        "        break\n"
        "      fi\n"
        "      sleep 0.02\n"
        "    done\n"
        "  fi\n"
        "}\n"
        "trap flag_hold DEBUG\n",
        encoding="utf-8",
    )
    env["BASH_ENV"] = str(hook)
    process = subprocess.Popen(
        ["bash", str(script)], env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    errors = []
    pids = []
    reservation = OwnedArmIdentity()
    try:
        deadline = time.monotonic() + 20
        while not (block / "flag-held").exists():
            if process.poll() is not None or time.monotonic() > deadline:
                errors.append("benchmark did not reach the flag-clear boundary (spawn flag raised)")
                return errors
            time.sleep(0.02)
        process.send_signal(signal.SIGTERM)
        # stop_arm runs as soon as record_arm returns. Hold that hook until
        # the live group has been pinned, then let the benchmark cancel.
        while process.poll() is None and time.monotonic() < deadline:
            if (block / "arm-pgid").exists() and (block / "ready.json").exists():
                try:
                    pgid = int((block / "arm-pgid").read_text(encoding="utf-8").strip())
                    pids = json.loads((block / "ready.json").read_text(encoding="utf-8"))["pids"]
                except (OSError, ValueError, KeyError):
                    errors.append("flag-clear arm identity was not recorded")
                else:
                    if process.poll() is None:
                        reservation.pin(pgid, pids)
                        if process.poll() is None and not reservation.fds:
                            errors.append("flag-clear arm identity was not reserved")
                (block / "identity-pinned").write_text("pinned\n", encoding="utf-8")
                break
            time.sleep(0.02)
        remaining = max(0.1, deadline - time.monotonic())
        try:
            process.communicate(timeout=remaining)
        except subprocess.TimeoutExpired:
            errors.append("benchmark did not exit after SIGTERM at the flag-clear boundary")
            return errors
        if process.returncode != 143:
            errors.append(f"flag-clear cancellation did not report SIGTERM failure (exit {process.returncode})")
        if (block / "ready.json").exists():
            try:
                pids = json.loads((block / "ready.json").read_text(encoding="utf-8"))["pids"]
            except (OSError, ValueError, KeyError):
                errors.append("flag-clear arm identity was not recorded")
                pids = []
        survivors = [pid for pid in pids if pid_alive(pid)]
        if survivors:
            errors.append(f"arm processes survived the flag-clear cancellation: {survivors}")
        cleanup = [event for event in read_events(events) if event["tool"] == "rm"]
        if not cleanup:
            errors.append("owned directories were not cleaned after flag-clear cancellation")
        if any(event.get("live_pids") for event in cleanup):
            errors.append("cleanup removed owned directories while the arm was still running")
        if any((work / name).exists() for name in ("trees", "targets", "locks")):
            errors.append("owned directories survived flag-clear cancellation cleanup")
    finally:
        retire_cancellation_processes(reservation, process)
    return errors


def scenario_cancellation_identity(root, script):
    """Run both real cancellation teardowns against intercepted identities.

    Reaping retires the arm group and permits a replacement in this model.
    A signal is unsafe when it targets a numeric PID/PGID or a pidfd whose
    incarnation is already gone or was opened after that reuse.
    """
    import errno
    from unittest import mock

    leader = 424242
    shim = 424243
    child = 424244
    members = (leader, shim, child)
    cases = (
        (scenario_cancellation_spawn_window, "spawn-window"),
        (scenario_cancellation_flag_clear, "flag-clear"),
    )
    modes = ("success", "timeout", "assertion", "early-exit")
    failures = []
    for scenario, label in cases:
        for mode in modes:
            case_root = root / f"identity-{label}-{mode}"
            case_root.mkdir()
            model = {
                "next_fd": 10_000_000,
                "fds": {},
                "events": [],
                "unsafe": [],
                "retired": mode == "early-exit",
                "procs": {},
            }
            for pid in members:
                model["procs"][pid] = {
                    "pgrp": leader,
                    "start": 1000 + pid,
                    "generation": 2 if mode == "early-exit" else 1,
                    "alive": True,
                    "replacement": mode == "early-exit",
                }

            class FakeBenchmark:
                def __init__(self, argv, env=None, **kwargs):
                    self.returncode = None
                    self.env = env or {}
                    block = Path(self.env["BENCH_BLOCK"])
                    if mode == "early-exit":
                        self.returncode = 1
                        (block / "arm-pgid").write_text(f"{leader}\n", encoding="utf-8")
                        return
                    (block / "flag-held").write_text("held\n", encoding="utf-8")
                    (block / "arm-pgid").write_text(f"{leader}\n", encoding="utf-8")
                    (block / "ready.json").write_text(
                        json.dumps({"pids": [shim, child]}), encoding="utf-8")

                def poll(self):
                    return self.returncode

                def send_signal(self, sig):
                    if self.returncode is not None:
                        return
                    model["events"].append(("benchmark_signal", int(sig)))

                def kill(self):
                    if self.returncode is not None:
                        return
                    model["events"].append(("owned_benchmark_kill",))
                    self.returncode = -9

                def communicate(self, timeout=None):
                    if mode == "timeout" and self.returncode is None:
                        raise subprocess.TimeoutExpired("benchmark", timeout)
                    if self.returncode is None:
                        self.returncode = 143
                        self._finish(retire=(mode == "success"))
                    return "", ""

                def _finish(self, retire):
                    events_path = Path(self.env["BENCH_EVENTS"])
                    with events_path.open("a", encoding="utf-8") as output:
                        output.write(json.dumps({
                            "tool": "rm", "owned_paths": [], "live_pids": [],
                        }) + "\n")
                    if not retire:
                        model["events"].append(("communicate",))
                        return
                    model["events"].append(("retire",))
                    model["retired"] = True
                    for info in model["fds"].values():
                        info["original_alive"] = False
                    for pid in members:
                        proc = model["procs"][pid]
                        proc["generation"] += 1
                        proc["start"] += 1
                        proc["replacement"] = True
                        proc["alive"] = True

            real_close = os.close

            def fake_read(pid):
                proc = model["procs"].get(pid)
                if proc is None:
                    raise OSError(errno.ENOENT, "no such process")
                return proc["pgrp"], proc["start"]

            def fake_pidfd_open(pid, flags=0):
                proc = model["procs"].get(pid)
                if proc is None or not proc["alive"]:
                    raise OSError(errno.ESRCH, "gone")
                if model["retired"] or proc["replacement"]:
                    model["unsafe"].append(("pidfd_open", pid))
                fd = model["next_fd"]
                model["next_fd"] += 1
                model["fds"][fd] = {
                    "pid": pid,
                    "generation": proc["generation"],
                    "original_alive": proc["alive"] and not proc["replacement"],
                }
                model["events"].append(("pidfd_open", pid, proc["generation"]))
                return fd

            def fake_select(rlist, wlist, xlist, timeout=None):
                info = model["fds"][rlist[0]]
                if info["original_alive"]:
                    return [], [], []
                return [rlist[0]], [], []

            def fake_send(fd, sig, siginfo=None, flags=0):
                info = model["fds"][fd]
                proc = model["procs"][info["pid"]]
                replacement = (
                    proc["generation"] != info["generation"] or proc["replacement"]
                )
                stale = not info["original_alive"] or replacement
                model["events"].append(("pidfd_send_signal", info["pid"], int(sig)))
                if stale:
                    model["unsafe"].append(("pidfd_send_signal", info["pid"]))
                    raise OSError(errno.ESRCH, "reservation gone")
                info["original_alive"] = False
                proc["alive"] = False
                return None

            def fake_kill(pid, sig):
                model["events"].append(("kill", pid, int(sig)))
                model["unsafe"].append(("kill", pid))

            def fake_killpg(pgid, sig):
                model["events"].append(("killpg", pgid, int(sig)))
                model["unsafe"].append(("killpg", pgid))

            def fake_close(fd):
                if fd in model["fds"]:
                    model["events"].append(("close", fd))
                    return None
                return real_close(fd)

            def fake_pid_alive(pid):
                return mode == "assertion" and pid in (shim, child)

            with mock.patch("os.pidfd_open", fake_pidfd_open), \
                    mock.patch("os.kill", fake_kill), \
                    mock.patch("os.killpg", fake_killpg), \
                    mock.patch("os.close", fake_close), \
                    mock.patch("signal.pidfd_send_signal", fake_send), \
                    mock.patch("select.select", fake_select), \
                    mock.patch("subprocess.Popen", FakeBenchmark), \
                    mock.patch(f"{__name__}.read_arm_proc_identity", fake_read), \
                    mock.patch(f"{__name__}.pid_alive", fake_pid_alive):
                try:
                    scenario_errors = scenario(case_root, script)
                except Exception as exc:
                    scenario_errors = [f"raised {exc!r}"]
            opened = [event[1] for event in model["events"] if event[0] == "pidfd_open"]
            signaled = [event[1] for event in model["events"] if event[0] == "pidfd_send_signal"]
            numeric = [event for event in model["events"] if event[0] in ("kill", "killpg")]
            owned_kills = [event for event in model["events"] if event[0] == "owned_benchmark_kill"]
            problems = list(model["unsafe"])
            if numeric:
                problems.append(("numeric", numeric))
            if mode == "early-exit":
                if not any("did not reach" in item for item in scenario_errors):
                    problems.append(("scenario", scenario_errors))
                if opened or signaled or owned_kills:
                    problems.append(("signaled_without_reservation", opened, signaled, owned_kills))
            elif mode == "success":
                if scenario_errors:
                    problems.append(("scenario", scenario_errors))
                if opened != [leader, shim, child]:
                    problems.append(("pinned", opened))
                if signaled or owned_kills:
                    problems.append(("signaled_after_retire", signaled, owned_kills))
                retire_at = [
                    index for index, event in enumerate(model["events"]) if event[0] == "retire"
                ]
                if not retire_at:
                    problems.append(("missing_retire", model["events"]))
                elif any(
                    event[0] == "pidfd_open" and index > retire_at[0]
                    for index, event in enumerate(model["events"])
                ):
                    problems.append(("opened_after_retire", model["events"]))
            elif mode == "timeout":
                if scenario_errors != [
                    "benchmark did not exit after SIGTERM in the spawn window"
                    if label == "spawn-window"
                    else "benchmark did not exit after SIGTERM at the flag-clear boundary"
                ]:
                    problems.append(("scenario", scenario_errors))
                if opened != [leader, shim, child] or signaled != [leader, shim, child]:
                    problems.append(("signals", opened, signaled))
                if len(owned_kills) != 1:
                    problems.append(("benchmark_kill", owned_kills))
            else:
                if not any("survived" in item for item in scenario_errors):
                    problems.append(("scenario", scenario_errors))
                if any("not cleaned" in item or "while the arm" in item or "did not report" in item
                       for item in scenario_errors):
                    problems.append(("lost_checks", scenario_errors))
                if opened != [leader, shim, child] or signaled != [leader, shim, child]:
                    problems.append(("signals", opened, signaled))
                if owned_kills:
                    problems.append(("benchmark_kill", owned_kills))
            if problems:
                failures.append(f"{label} {mode}: {problems}")
    return failures


def main():
    script = Path(__file__).with_name("bench-build-budget.sh").resolve()
    scratch = Path(__file__).resolve().parent.parent / ".orbit" / "tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    scenarios = (
        ("existing-root", scenario_existing_root),
        ("root-symlink", scenario_root_symlink),
        ("internal-tree-symlink", lambda root, path: scenario_internal_symlink(root, path, "tree")),
        ("internal-target-symlink", lambda root, path: scenario_internal_symlink(root, path, "target")),
        ("result-escape", scenario_result_escape),
        ("archive-failure", scenario_archive_failure),
        ("fresh-run", scenario_fresh_run),
        ("cancellation", scenario_cancellation),
        ("cancellation-spawn-window", scenario_cancellation_spawn_window),
        ("cancellation-flag-clear", scenario_cancellation_flag_clear),
        ("cancellation-identity", scenario_cancellation_identity),
    )
    failures = 0
    with tempfile.TemporaryDirectory(prefix="bench-build-budget-", dir=scratch) as directory:
        root = Path(directory)
        for name, scenario in scenarios:
            errors = scenario(root, script)
            print(name + ": " + ("; ".join(errors) if errors else "passed"))
            failures += bool(errors)
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
