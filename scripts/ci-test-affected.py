#!/usr/bin/env python3
"""Run complete test targets for changed crates and their workspace dependents.

Orbit's validation runner sets two environment variables [ORB-15131]. When
ORBIT_VALIDATION_SUMMARY names a file, the run writes a JSON summary there:
the selection it ran and how many tests nextest executed. When
ORBIT_VALIDATION_SELECTION holds such a selection (from a candidate run), the
run executes exactly those packages instead of selecting from a diff, so a
rerun on the base commit tests what the candidate tested.
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import xml.etree.ElementTree


ROOT = Path(__file__).resolve().parent.parent
SUMMARY_ENV = "ORBIT_VALIDATION_SUMMARY"
SELECTION_ENV = "ORBIT_VALIDATION_SELECTION"


def git(*arguments):
    return subprocess.check_output(
        ["git", "-C", str(ROOT), *arguments],
        env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
    )


def comparison_base(explicit):
    if explicit:
        return git("rev-parse", "--verify", f"{explicit}^{{commit}}").decode().strip()
    for branch in ("refs/remotes/origin/agent-main", "refs/heads/agent-main"):
        exists = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "--verify", "--quiet", branch],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if exists.returncode == 0:
            return git("merge-base", "HEAD", branch).decode().strip()
    raise ValueError("cannot resolve agent-main; set CI_TEST_BASE to the delivery base commit")


def changed_paths(base):
    # A commit-to-worktree diff includes committed, staged and unstaged changes.
    # Disabling rename detection includes both the old and new crate of a move.
    tracked = git("diff", "--name-only", "--no-renames", "-z", base, "--")
    untracked = git("ls-files", "--others", "--exclude-standard", "-z")
    return {os.fsdecode(path) for path in (tracked + untracked).split(b"\0") if path}


# Files that a crate's tests read at run time from outside its own directory:
# another crate's assets, or a repository-root file. Cargo metadata cannot see
# these edges, so each is declared here as (path prefix, reading crates). A
# changed path under a prefix selects the readers and their reverse dependents.
FILE_READERS = (
    (Path("scripts/build-budget.py"), ("orbit-engine",)),
    (Path("crates/orbit-core/assets/jobs"), ("orbit-engine", "orbit-cli")),
    (Path("crates/orbit-core/assets/activities"), ("orbit-engine",)),
    (Path("crates/orbit-core/assets/auto_tasks"), ("orbit-cli",)),
    (Path("plugin/hooks"), ("orbit-cli",)),
    (Path("server.json"), ("orbit-cli",)),
)


def affected_packages(metadata, paths):
    members = set(metadata["workspace_members"])
    packages = {package["name"]: package for package in metadata["packages"]
                if package["id"] in members}
    if not packages:
        raise ValueError("cargo metadata reported no workspace packages")
    roots = {name: Path(package["manifest_path"]).resolve().parent.relative_to(ROOT)
             for name, package in packages.items()}
    by_root = {ROOT / path: name for name, path in roots.items()}
    selected = set()
    shared_files = {"Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"}
    for raw_path in paths:
        path = Path(raw_path)
        if raw_path in shared_files or path.parts[0] in {".cargo", ".config"}:
            return sorted(packages)
        owners = {name for name, root in roots.items() if path.is_relative_to(root)}
        if not owners and path.parts[0] == "crates":
            # A removed member no longer appears in current Cargo metadata.
            return sorted(packages)
        selected.update(owners)
        for prefix, readers in FILE_READERS:
            if path.is_relative_to(prefix):
                missing = [name for name in readers if name not in packages]
                if missing:
                    raise ValueError(f"FILE_READERS names unknown workspace crates: {', '.join(missing)}")
                selected.update(readers)

    reverse = {name: set() for name in packages}
    for name, package in packages.items():
        for dependency in package["dependencies"]:
            # --no-deps metadata retains dev/build/optional/target-specific edges,
            # including renamed dependencies. Match workspace paths, not registry
            # names, so a registry package with the same name is not an edge.
            dependency_path = dependency.get("path")
            owner = by_root.get(Path(dependency_path).resolve()) if dependency_path else None
            if owner is not None:
                reverse[owner].add(name)
    pending = list(selected)
    while pending:
        for dependent in reverse[pending.pop()] - selected:
            selected.add(dependent)
            pending.append(dependent)
    return sorted(selected)


def given_packages(metadata, text):
    """The packages a candidate's summary selection names, all of them current members."""
    selection = json.loads(text)
    packages = selection.get("packages") if isinstance(selection, dict) else None
    if not isinstance(packages, list) or not all(isinstance(name, str) for name in packages):
        raise ValueError(f"{SELECTION_ENV} must be a JSON object with a list of package names")
    members = set(metadata["workspace_members"])
    known = {package["name"] for package in metadata["packages"] if package["id"] in members}
    missing = sorted(set(packages) - known)
    if missing:
        raise ValueError(f"{SELECTION_ENV} names packages this checkout lacks: {', '.join(missing)}")
    return sorted(set(packages))


def executed_tests(junit):
    """How many tests nextest's JUnit report says ran, or None without a report."""
    try:
        tests = xml.etree.ElementTree.parse(junit).getroot().get("tests")
    except (OSError, xml.etree.ElementTree.ParseError):
        return None
    return int(tests) if tests is not None and tests.isdigit() else None


def write_summary(path, selection, tests_run):
    if path:
        Path(path).write_text(json.dumps(
            dict(schema_version=1, selection=selection, tests_run=tests_run)))


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default=os.environ.get("CI_TEST_BASE"),
                        help="exact delivery base revision (default: merge base with origin/agent-main or agent-main)")
    parser.add_argument("--list", action="store_true",
                        help="list selected package names without compiling or running tests")
    arguments = parser.parse_args()
    cargo = shlex.split(os.environ.get("CARGO", "cargo"))
    given = os.environ.get(SELECTION_ENV)
    summary = None if arguments.list else os.environ.get(SUMMARY_ENV)
    base = None if given else comparison_base(arguments.base)
    metadata = json.loads(subprocess.check_output(
        [*cargo, "metadata", "--format-version", "1", "--no-deps", "--manifest-path", str(ROOT / "Cargo.toml")],
        cwd=ROOT,
    ))
    if given:
        packages = given_packages(metadata, given)
        print(f"ci-test-affected: running the selection {SELECTION_ENV} names", file=sys.stderr, flush=True)
    else:
        packages = affected_packages(metadata, changed_paths(base))
        print(f"ci-test-affected: base={base}", file=sys.stderr, flush=True)
    # Cargo rejects --lib for a bin-only selection, even with --bins/--tests.
    library_kinds = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}
    has_library = any(library_kinds.intersection(target["kind"])
                      for package in metadata["packages"] if package["name"] in packages
                      for target in package["targets"])
    target_flags = (["--lib"] if has_library else []) + ["--bins", "--tests"] if packages else []
    # nextest does not execute doctests; they run through Cargo for the same set.
    # A bin-only selection has no doctest targets and Cargo rejects --doc for it.
    doc_packages = sorted(package["name"] for package in metadata["packages"]
                          if package["name"] in packages
                          and any(target["doctest"] for target in package["targets"]))
    selection = dict(packages=packages, target_flags=target_flags, doctest_packages=doc_packages)
    if not packages:
        print("ci-test-affected: no affected workspace crates; no Rust tests to run", file=sys.stderr)
        write_summary(summary, selection, 0)
        return 0
    if arguments.list:
        print("\n".join(packages))
        return 0

    print(f"ci-test-affected: running full test targets for {', '.join(packages)}", file=sys.stderr, flush=True)
    package_flags = [argument for name in packages for argument in ("-p", name)]
    test_environment = dict(os.environ)
    # A test that runs this script must not report into the outer run's summary.
    test_environment.pop(SUMMARY_ENV, None)
    test_environment.pop(SELECTION_ENV, None)
    # A claimed executor exports the worker-binding marker so the Orbit commands
    # its agent runs refuse to start without the binding recorded for them. A
    # test process has no binding: inherited, the marker makes every in-process
    # fixture that opens a runtime fail closed. Tests of the refusal set it.
    test_environment.pop("ORBIT_WORKER_CONTEXT_REQUIRED", None)
    temporary_root = Path(tempfile.gettempdir()).resolve()
    if temporary_root.is_relative_to(ROOT) and temporary_root != ROOT:
        # Non-Git fixtures must not discover the enclosing managed checkout.
        # Keep any caller-supplied boundaries and the sandbox permissions.
        test_environment["GIT_CEILING_DIRECTORIES"] = os.pathsep.join(filter(None, (
            test_environment.get("GIT_CEILING_DIRECTORIES"), str(temporary_root))))
    budget = shlex.split(os.environ.get("BUILD_BUDGET", str(ROOT / "scripts/build-budget.py")))
    nextest = subprocess.run([*cargo, "nextest", "--version"], cwd=ROOT,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    with tempfile.TemporaryDirectory(prefix="ci-test-affected-") as report:
        junit = Path(report) / "junit.xml"
        if nextest.returncode == 0:
            report_flags = []
            if summary:
                # The executed-test count comes from nextest's JUnit report. A
                # tool config file adds it under the repository's own profile.
                tool_config = Path(report) / "nextest.toml"
                tool_config.write_text(f"[profile.default.junit]\npath = {json.dumps(str(junit))}\n")
                report_flags = ["--tool-config-file", f"ci-test-affected:{tool_config}"]
            command = [*cargo, "nextest", "run", "--no-fail-fast", *report_flags, *package_flags, *target_flags]
        else:
            # cargo test reports no executed-test count, so the summary records none.
            print("ci-test-affected: cargo-nextest unavailable; falling back to cargo test", file=sys.stderr)
            command = [*cargo, "test", "--no-fail-fast", *package_flags, *target_flags]
        result = subprocess.run([*budget, "--", *command], cwd=ROOT, env=test_environment)
        write_summary(summary, selection, executed_tests(junit))
    if result.returncode:
        return result.returncode
    if not doc_packages:
        return 0
    doc_flags = [argument for name in doc_packages for argument in ("-p", name)]
    return subprocess.run([*budget, "--", *cargo, "test", "--no-fail-fast", *doc_flags, "--doc"],
                          cwd=ROOT, env=test_environment).returncode


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"ci-test-affected: {error}", file=sys.stderr)
        sys.exit(1)
