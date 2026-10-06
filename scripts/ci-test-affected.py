#!/usr/bin/env python3
"""Run complete test targets for changed crates and their workspace dependents."""

import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default=os.environ.get("CI_TEST_BASE"),
                        help="exact delivery base revision (default: merge base with origin/agent-main or agent-main)")
    parser.add_argument("--list", action="store_true",
                        help="list selected package names without compiling or running tests")
    arguments = parser.parse_args()
    cargo = shlex.split(os.environ.get("CARGO", "cargo"))
    base = comparison_base(arguments.base)
    metadata = json.loads(subprocess.check_output(
        [*cargo, "metadata", "--format-version", "1", "--no-deps", "--manifest-path", str(ROOT / "Cargo.toml")],
        cwd=ROOT,
    ))
    packages = affected_packages(metadata, changed_paths(base))
    print(f"ci-test-affected: base={base}", file=sys.stderr, flush=True)
    if not packages:
        print("ci-test-affected: no affected workspace crates; no Rust tests to run", file=sys.stderr)
        return 0
    if arguments.list:
        print("\n".join(packages))
        return 0

    print(f"ci-test-affected: running full test targets for {', '.join(packages)}", file=sys.stderr, flush=True)
    package_flags = [argument for name in packages for argument in ("-p", name)]
    test_environment = dict(os.environ)
    temporary_root = Path(tempfile.gettempdir()).resolve()
    if temporary_root.is_relative_to(ROOT) and temporary_root != ROOT:
        # Non-Git fixtures must not discover the enclosing managed checkout.
        # Keep any caller-supplied boundaries and the sandbox permissions.
        test_environment["GIT_CEILING_DIRECTORIES"] = os.pathsep.join(filter(None, (
            test_environment.get("GIT_CEILING_DIRECTORIES"), str(temporary_root))))
    budget = shlex.split(os.environ.get("BUILD_BUDGET", str(ROOT / "scripts/build-budget.py")))
    nextest = subprocess.run([*cargo, "nextest", "--version"], cwd=ROOT,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if nextest.returncode == 0:
        command = [*cargo, "nextest", "run", "--no-fail-fast", *package_flags, "--lib", "--bins", "--tests"]
    else:
        print("ci-test-affected: cargo-nextest unavailable; falling back to cargo test", file=sys.stderr)
        command = [*cargo, "test", "--no-fail-fast", *package_flags, "--lib", "--bins", "--tests"]
    result = subprocess.run([*budget, "--", *command], cwd=ROOT, env=test_environment)
    if result.returncode:
        return result.returncode
    # nextest does not execute doctests; run those through Cargo for the same set.
    # A bin-only selection has no doctest targets and Cargo rejects --doc for it.
    doc_packages = sorted(package["name"] for package in metadata["packages"]
                          if package["name"] in packages
                          and any(target["doctest"] for target in package["targets"]))
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
