#!/usr/bin/env python3
"""Exercise benchmark path admission and arm environments without compiling,
daemons, or deletion."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

TRANSPORT = r'''#!/usr/bin/env python3
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
if name == "cargo":
    # The effective environment of each arm, as the rustc wrapper would see it.
    event.update(cache_switch=os.environ.get("ORBIT_COMPILER_CACHE"),
                 cache_bin=os.environ.get("ORBIT_COMPILER_CACHE_BIN"),
                 incremental=os.environ.get("CARGO_INCREMENTAL"))
with open(os.environ["BENCH_EVENTS"], "a") as log:
    log.write(json.dumps(event) + "\n")
if name == "rm":
    # Never delete anything, even for the unfixed benchmark.
    sys.exit(93 if event["unsafe"] else 0)
if name == "git":
    sys.exit(subprocess.call(["tar", "-cf", "-", "-C", os.environ["BENCH_TREE"], "."]))
if name == "cargo" and args[:1] == ["generate-lockfile"]:
    Path("Cargo.lock").write_text("# fixture lockfile\n")
if name in ("cargo", "rustc", "sccache"):
    print(name + " fixture")
    sys.exit(0)
if name == "bwrap":
    sys.exit(0 if os.environ.get("BENCH_BWRAP") == "ok" else 1)
if name == "curl":
    sys.exit(22)  # No network: the isolated sccache install fails.
raise SystemExit("unexpected transport: " + name)
'''

# Stands in for scripts/test-compiler-cache-namespaces.sh. Like that script it
# exits 0 without running when the cache is disabled, and prints its `ok` line
# only for a real run.
NAMESPACES = r'''#!/usr/bin/env python3
import json
import os

with open(os.environ["BENCH_EVENTS"], "a") as log:
    log.write(json.dumps(dict(
        tool="namespaces", cache_switch=os.environ.get("ORBIT_COMPILER_CACHE"),
        cache_bin=os.environ.get("ORBIT_COMPILER_CACHE_BIN"),
        require_ns=os.environ.get("ORBIT_COMPILER_CACHE_REQUIRE_NS"))) + "\n")
prefix = "test-compiler-cache-namespaces: "
if os.environ.get("ORBIT_COMPILER_CACHE") == "0":
    print(prefix + "not applicable: host compiler cache is disabled (ORBIT_COMPILER_CACHE=0)")
elif os.environ.get("BENCH_NAMESPACES") == "unwritable":
    print(prefix + "not applicable: host compiler cache is disabled "
          "(cache directory unset or not writable)")
else:
    print(prefix + "ok")
'''


def write_transport(tools, names):
    transport = tools / "transport"
    transport.write_text(TRANSPORT)
    transport.chmod(0o755)
    for tool in names:
        (tools / tool).symlink_to(transport)


def path_without_sccache(shadows):
    """The host PATH with any `sccache` hidden, so its absence is the fixture's."""
    entries = []
    for index, entry in enumerate(os.environ["PATH"].split(os.pathsep)):
        directory = Path(entry)
        if not (directory / "sccache").exists():
            entries.append(entry)
            continue
        shadow = shadows / str(index)
        shadow.mkdir(parents=True)
        for item in directory.iterdir():
            if item.name != "sccache":
                (shadow / item.name).symlink_to(item)
        entries.append(str(shadow))
    return os.pathsep.join(entries)


def check_arms(script, root, scenario):
    """Run every arm with fake transports and check each effective environment."""
    fixture = root / scenario
    checkout = fixture / "checkout"
    tools = fixture / "bin"
    work = fixture / "work"
    for directory in (checkout / "scripts", checkout / ".cargo", tools,
                      fixture / "tree", fixture / "home"):
        directory.mkdir(parents=True)
    (fixture / "tree" / "fixture.txt").write_text("tiny archive\n")
    source = script.parent.parent
    bench = checkout / "scripts" / "bench-compiler-cache.sh"
    shutil.copy2(script, bench)
    shutil.copy2(source / "scripts" / "rustc-compiler-cache.sh", checkout / "scripts")
    shutil.copy2(source / ".cargo" / "config.toml", checkout / ".cargo")
    namespaces = checkout / "scripts" / "test-compiler-cache-namespaces.sh"
    namespaces.write_text(NAMESPACES)
    namespaces.chmod(0o755)
    without_sccache = "no-sccache" in scenario
    write_transport(tools, ("git", "cargo", "rustc", "bwrap", "rm", "curl")
                    + (() if without_sccache else ("sccache",)))
    required = scenario.startswith("required-")
    events = fixture / "events.jsonl"
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(("SCCACHE_", "ORBIT_COMPILER_CACHE")):
            del env[key]
    env.update(PATH=str(tools) + os.pathsep + path_without_sccache(fixture / "shadow"),
               HOME=str(fixture / "home"), ORBIT_COMPILER_CACHE_BENCH_DIR=str(work),
               ORBIT_COMPILER_CACHE_BENCH_RESULT=str(work / "result.txt"),
               BENCH_WORK=str(work), BENCH_TREE=str(fixture / "tree"),
               BENCH_EVENTS=str(events),
               BENCH_BWRAP="refused" if "no-namespace" in scenario else "ok")
    if not without_sccache:
        env["ORBIT_COMPILER_CACHE_BIN"] = str(tools / "sccache")
    if required:
        env["ORBIT_COMPILER_CACHE_REQUIRE_NS"] = "1"
    if "opt-out" in scenario:
        env["ORBIT_COMPILER_CACHE"] = "0"
    if "unwritable" in scenario:
        env["BENCH_NAMESPACES"] = "unwritable"
    process = subprocess.run(["bash", str(bench)], env=env, text=True,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    recorded = [json.loads(line) for line in events.read_text().splitlines()] if events.exists() else []
    result = (work / "result.txt").read_text() if (work / "result.txt").exists() else ""
    lines = result.splitlines()
    builds = [event for event in recorded if event["tool"] == "cargo"
              and event["args"][:1] in (["check"], ["clippy"])]
    namespace_runs = [event for event in recorded if event["tool"] == "namespaces"]
    errors = []
    if [event["cache_switch"] for event in builds[:2]] != ["0", "0"]:
        errors.append("baseline arms did not run with the cache disabled")
    if without_sccache:
        if len(builds) != 2:
            errors.append("cache arms ran without sccache")
    else:
        # Unnormalized pair, same-path pair, Clippy and the incremental probe.
        cached = builds[2:]
        if len(cached) != 6 or [event["args"][0] for event in cached] != ["check"] * 4 + ["clippy", "check"]:
            errors.append("cache arms did not all run")
        if any(event["cache_switch"] != "1" or event["cache_bin"] != str(tools / "sccache")
               for event in cached):
            errors.append("an arm labelled cached ran without an enabled cache")
        if [event["incremental"] for event in cached] != ["0"] * 5 + ["1"]:
            errors.append("cache arms changed their incremental setting")
    ran = not without_sccache and "no-namespace" not in scenario
    if len(namespace_runs) != int(ran):
        errors.append("concurrent namespace arm was not invoked exactly as expected")
    for event in namespace_runs:
        if event["cache_switch"] != "1" or event["cache_bin"] != str(tools / "sccache"):
            errors.append("namespace arm did not receive an enabled cache")
        if event["require_ns"] != ("1" if required else "0"):
            errors.append("namespace arm did not receive the required-coverage setting")
    measured = ran and "unwritable" not in scenario
    concurrent = [line.split("=", 1)[1] for line in lines if line.startswith("cache_concurrent=")]
    if measured:
        if process.returncode != 0:
            errors.append("measured benchmark failed")
        if len(concurrent) != 1 or not concurrent[0].replace(".", "", 1).isdigit():
            errors.append(f"concurrent arm has no wall time: {concurrent}")
    elif required:
        if process.returncode == 0:
            errors.append("required namespace arm did not run but the benchmark passed")
        if concurrent != ["failed"]:
            errors.append(f"required namespace failure was not recorded: {concurrent}")
        if "ORBIT_COMPILER_CACHE_REQUIRE_NS=1: the required concurrent arm did not run" not in lines:
            errors.append("required namespace failure was not named")
    else:
        if process.returncode != 0:
            errors.append("optional namespace skip failed the benchmark")
        if concurrent != ["skipped"]:
            errors.append(f"optional namespace skip was not explicit: {concurrent}")
    reasons = [line for line in lines if line.startswith("namespace_probe=")]
    expected_reason = ("sccache is not available" if without_sccache
                       else "cannot create mount namespace" if "no-namespace" in scenario
                       else "cache directory unset or not writable" if "unwritable" in scenario
                       else "bwrap can create mount namespaces")
    if len(reasons) != 1 or expected_reason not in reasons[0]:
        errors.append(f"namespace outcome was not explained: {reasons}")
    if any(event.get("unsafe") for event in recorded):
        errors.append("external or workdir deletion reached recorder")
    return dict(scenario=scenario, exit_code=process.returncode, errors=errors,
                events=recorded, stderr=process.stderr[-3000:])


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
    write_transport(tools, ("git", "cargo", "rustc", "sccache", "bwrap", "rm"))

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
    elif scenario == "destination-symlink":
        (work / "private").mkdir()
        (work / "a-target").symlink_to(work / "private", target_is_directory=True)
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
    elif scenario == "existing-unowned":
        (work / "a").mkdir()
        (work / "a" / "operator.txt").write_text("operator source\n")
        (work / "a-target").mkdir()
        (work / "a-target" / "sentinel.txt").write_text("operator target\n")
    elif scenario == "existing-unowned-cache":
        (work / "cache").mkdir()
        (work / "cache" / "operator.txt").write_text("operator cache\n")
    elif scenario == "existing-unowned-file":
        (work / "operator.txt").write_text("operator file\n")
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
    if scenario.startswith("existing-unowned"):
        expected = {
            "existing-unowned": ((work / "a" / "operator.txt", "operator source\n"),
                                 (work / "a-target" / "sentinel.txt", "operator target\n")),
            "existing-unowned-cache": ((work / "cache" / "operator.txt", "operator cache\n"),),
            "existing-unowned-file": ((work / "operator.txt", "operator file\n"),),
        }[scenario]
        for path, text in expected:
            if not path.exists() or path.read_text() != text:
                errors.append(f"pre-existing sentinel changed: {path}")
        if (work / "result.txt").exists():
            errors.append("refusal created or truncated the result file")
        if (work / ".orbit-compiler-cache-bench-owner").exists():
            errors.append("refusal claimed an unowned workdir")
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
            "dot-equality", "target-symlink", "destination-symlink", "source-symlink",
            "cache-target-overlap", "cache-target-ancestor", "result-symlink",
            "private-nested", "private-dot", "private-symlink",
            "private-empty", "existing-unowned", "existing-unowned-cache", "existing-unowned-file")]
        results += [check_arms(args.script.resolve(), Path(directory), scenario) for scenario in (
            "required-opt-out", "optional-opt-out", "required-no-sccache", "optional-no-sccache",
            "required-no-sccache-opt-out", "required-unwritable", "optional-unwritable",
            "required-no-namespace", "optional-no-namespace")]
    if args.report:
        args.report.write_text(json.dumps(results, indent=2) + "\n")
    for result in results:
        print(result["scenario"] + ": " + ("; ".join(result["errors"]) or "passed"))
    return int(any(result["errors"] for result in results))


if __name__ == "__main__":
    raise SystemExit(main())
