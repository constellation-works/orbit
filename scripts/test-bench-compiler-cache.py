#!/usr/bin/env python3
"""Exercise benchmark path admission without compiling, daemons, or deletion."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def check_benchmark(script, root, scenario):
    fixture = root / scenario
    work = fixture / "work"
    outside = fixture / "operator-cache"
    tools = fixture / "bin"
    for directory in (work, outside, tools, fixture / "tree"):
        if directory == work and scenario == "private-nested":
            continue  # Exercise a workdir and cache tail that do not exist yet.
        directory.mkdir(parents=True)
    sentinel = outside / "sentinel"
    sentinel.write_text("operator data\n")
    (fixture / "tree" / "fixture.txt").write_text("tiny archive\n")
    events = fixture / "events.jsonl"
    transport = tools / "transport"
    transport.write_text(r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import subprocess
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "git" and args == ["rev-parse", "HEAD"]:
    print("fixture-head")
    sys.exit(0)
event = dict(tool=name, args=args)
if name == "rm":
    work = Path(os.environ["BENCH_WORK"]).resolve()
    paths = []
    for arg in args:
        if arg.startswith("-"):
            continue
        path = Path(arg)
        # rm unlinks a final symlink; it follows symlink ancestors when
        # resolving a child (including children expanded by the shell glob).
        if path.is_symlink() and not arg.endswith("/"):
            paths.append(path.parent.resolve() / path.name)
        else:
            paths.append(path.resolve())
    event["unsafe"] = any(path == work or work not in path.parents for path in paths)
if name == "sccache":
    event.update(cache=os.environ.get("SCCACHE_DIR"), uds=os.environ.get("SCCACHE_SERVER_UDS"))
with open(os.environ["BENCH_EVENTS"], "a") as log:
    log.write(json.dumps(event) + "\n")
if name == "rm":
    # Never delete anything, even for the unfixed benchmark.
    sys.exit(93 if event["unsafe"] else 0)
if name == "git":
    sys.exit(subprocess.call(["tar", "-cf", "-", "-C", os.environ["BENCH_TREE"], "."]))
if name in ("cargo", "rustc", "sccache"):
    print(name + " fixture")
    sys.exit(0)
if name == "bwrap":
    sys.exit(1)
raise SystemExit("unexpected transport: " + name)
''')
    transport.chmod(0o755)
    for tool in ("git", "cargo", "rustc", "sccache", "bwrap", "rm"):
        (tools / tool).symlink_to(transport)

    cache = work / "cache"
    if scenario == "parent-escape":
        cache = work / ".." / "operator-cache"
    elif scenario == "ancestor-symlink-escape":
        (work / "linked").symlink_to(fixture, target_is_directory=True)
        cache = work / "linked" / "operator-cache"
    elif scenario == "literal-external":
        cache = outside
    elif scenario == "workdir-equality":
        cache = work
    elif scenario == "dot-equality":
        cache = work / "cache" / ".."
        (work / "cache").mkdir()
    elif scenario == "target-symlink":
        (work / "a-target").symlink_to(outside, target_is_directory=True)
    elif scenario == "source-symlink":
        (work / "a").symlink_to(outside, target_is_directory=True)
    elif scenario == "cache-target-overlap":
        cache = work / "warm-target" / "cache"
    elif scenario == "cache-target-ancestor":
        cache = work / "warm-target"
    elif scenario == "result-symlink":
        (work / "result.txt").symlink_to(sentinel)
    elif scenario == "private-nested":
        cache = work / "private" / "missing" / "cache"
    elif scenario == "private-dot":
        (work / "existing").mkdir()
        (work / "cache").mkdir()
        cache = work / "existing" / ".." / "cache"
    elif scenario == "private-symlink":
        (work / "private").mkdir()
        (work / "linked").symlink_to(work / "private", target_is_directory=True)
        cache = work / "linked" / "cache"
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(("SCCACHE_", "ORBIT_COMPILER_CACHE_")):
            del env[key]
    env.update(PATH=str(tools) + os.pathsep + env["PATH"],
               SCCACHE_DIR=str(cache), ORBIT_COMPILER_CACHE_BENCH_DIR=str(work),
               ORBIT_COMPILER_CACHE_BENCH_RESULT=str(work / "result.txt"),
               ORBIT_COMPILER_CACHE_BIN=str(tools / "sccache"),
               BENCH_WORK=str(work), BENCH_TREE=str(fixture / "tree"),
               BENCH_EVENTS=str(events))
    process = subprocess.run(["bash", str(script)], env=env, text=True,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
    recorded = [json.loads(line) for line in events.read_text().splitlines()] if events.exists() else []
    accepted = scenario.startswith("private-")
    errors = []
    if accepted:
        if process.returncode != 0:
            errors.append("private benchmark failed")
        if not any(event["tool"] == "cargo" and "check" in event["args"] for event in recorded):
            errors.append("private build was not exercised")
        if not any(event["tool"] == "sccache" and "--show-stats" in event["args"] for event in recorded):
            errors.append("private cache was not exercised")
        for event in recorded:
            if event["tool"] == "sccache" and event["args"] != ["--version"]:
                if Path(event["cache"]) != cache.resolve():
                    errors.append("daemon did not receive canonical cache")
                if Path(event["uds"]) != work.resolve() / "sccache.sock":
                    errors.append("daemon did not receive private canonical socket")
    else:
        if process.returncode == 0:
            errors.append("unsafe path was accepted")
        if recorded:
            errors.append("refusal occurred after build/daemon/deletion side effects")
    if any(event.get("unsafe") for event in recorded):
        errors.append("external or workdir deletion reached recorder")
    if sentinel.read_text() != "operator data\n":
        errors.append("external sentinel changed")
    return dict(scenario=scenario, exit_code=process.returncode, errors=errors,
                events=recorded, stderr=process.stderr[-3000:])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--script", type=Path, default=Path(__file__).with_name("bench-compiler-cache.sh"))
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    scratch = Path(__file__).resolve().parent.parent / ".orbit" / "tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="bench-containment-", dir=scratch) as directory:
        results = [check_benchmark(args.script.resolve(), Path(directory), scenario) for scenario in (
            "parent-escape", "ancestor-symlink-escape", "literal-external", "workdir-equality",
            "dot-equality", "target-symlink", "source-symlink", "cache-target-overlap",
            "cache-target-ancestor", "result-symlink", "private-nested", "private-dot", "private-symlink")]
    if args.report:
        args.report.write_text(json.dumps(results, indent=2) + "\n")
    for result in results:
        print(result["scenario"] + ": " + ("; ".join(result["errors"]) or "passed"))
    return int(any(result["errors"] for result in results))


if __name__ == "__main__":
    raise SystemExit(main())
