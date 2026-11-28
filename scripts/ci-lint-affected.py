#!/usr/bin/env python3
"""Run production, all-targets, and rustdoc lint-wrapper checks on changed crates.

Resolves the delivery base from CI_TEST_BASE (or `--base`), falling back to
the merge base with origin/agent-main or agent-main. Maps committed, staged,
unstaged, deleted, and non-ignored untracked paths to workspace crates, then
runs the three `scripts/with-test-build-lint.sh --check` passes sequentially
under `scripts/build-budget.py` for each changed crate. Exits 0 without
compiling when no workspace crate changed.
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
BASE_BRANCHES = ("refs/remotes/origin/agent-main", "refs/heads/agent-main")


class GateError(Exception):
    """A condition that stops the gate before checks can run."""


def git(*arguments):
    try:
        return subprocess.check_output(
            ["git", "-C", str(ROOT), *arguments],
            env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
            stderr=subprocess.PIPE,
        )
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode(errors="replace").strip() if error.stderr else ""
        raise GateError(f"git {arguments[0]} failed: {detail}" if detail else f"git {arguments[0]} failed") from error
    except OSError as error:
        raise GateError(f"git {arguments[0]} could not run: {error}") from error


def has_ref(ref):
    return subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "--verify", "--quiet", ref],
        env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    ).returncode == 0


def resolve_base(explicit):
    """The merge base of HEAD and CI_TEST_BASE (else origin/agent-main or agent-main)."""
    revision = (explicit or "").strip()
    if not revision:
        revision = next((ref for ref in BASE_BRANCHES if has_ref(ref)), "")
    if not revision:
        raise GateError("no base revision: set CI_TEST_BASE or fetch origin/agent-main")
    commit = git("rev-parse", "--verify", f"{revision}^{{commit}}").decode().strip()
    fork = git("merge-base", "HEAD", commit).decode().strip()
    if not fork:
        raise GateError(f"no merge base between HEAD and {revision}")
    return fork


def changed_paths(base):
    """Committed, staged, unstaged, deleted, and untracked paths since `base`."""
    # Disabling rename detection includes both the old and new paths of a move.
    tracked = git("diff", "--name-only", "--no-renames", "-z", base, "--")
    untracked = git("ls-files", "--others", "--exclude-standard", "-z")
    return {os.fsdecode(path) for path in (tracked + untracked).split(b"\0") if path}


def workspace_packages(metadata):
    members = set(metadata["workspace_members"])
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in members
    }
    if not packages:
        raise GateError("cargo metadata reported no workspace packages")
    return packages


def changed_crates(packages, paths):
    """Sorted workspace crate names owning any path in `paths`."""
    roots = {
        name: Path(package["manifest_path"]).resolve().parent.relative_to(ROOT)
        for name, package in packages.items()
    }
    selected = set()
    for raw_path in paths:
        path = Path(raw_path)
        for name, root in roots.items():
            if path.is_relative_to(root):
                selected.add(name)
    return sorted(selected)


def production_target_flags(package):
    """Target flags for the production Clippy pass (`--bins` for bin-only crates)."""
    kinds = {kind for target in package.get("targets", []) for kind in target.get("kind", [])}
    if "lib" in kinds:
        return ["--lib", "--bins"]
    if "bin" in kinds:
        return ["--bins"]
    return []


def crate_checks(crate, package, cargo):
    """The three wrapper checks from docs/development/validation-and-ci.md for one crate."""
    checks = []
    prod_flags = production_target_flags(package)
    if prod_flags:
        checks.append((
            "production",
            [*cargo, "check", "-p", crate, *prod_flags, "--message-format=json"],
            {},
        ))
    checks.append((
        "all-targets",
        [*cargo, "check", "-p", crate, "--all-targets", "--message-format=json"],
        {},
    ))
    checks.append((
        "rustdoc",
        [*cargo, "doc", "--no-deps", "-p", crate, "--message-format=json"],
        {"RUSTDOCFLAGS": "-D warnings"},
    ))
    return checks


def command_from_env(name, default):
    raw = os.environ.get(name, "").strip()
    return shlex.split(raw) if raw else list(default)


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--base",
        default=os.environ.get("CI_TEST_BASE"),
        help="base revision (default: CI_TEST_BASE, else merge base with origin/agent-main or agent-main)",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="list changed workspace crate names without running checks",
    )
    arguments = parser.parse_args()

    cargo = command_from_env("CARGO", ["cargo"])
    budget = command_from_env("BUILD_BUDGET", [str(ROOT / "scripts/build-budget.py")])
    lint_wrapper = ROOT / "scripts/with-test-build-lint.sh"

    base = resolve_base(arguments.base)
    metadata = json.loads(
        subprocess.check_output(
            [*cargo, "metadata", "--format-version", "1", "--no-deps", "--manifest-path", str(ROOT / "Cargo.toml")],
            cwd=ROOT,
        )
    )
    packages = workspace_packages(metadata)
    crates = changed_crates(packages, changed_paths(base))

    if arguments.list:
        if crates:
            print("\n".join(crates))
        return 0

    if not crates:
        print(f"ci-lint-affected: no changed workspace crates against {base[:12]}; nothing to lint", file=sys.stderr)
        return 0

    print(f"ci-lint-affected: base={base[:12]} crates={', '.join(crates)}", file=sys.stderr, flush=True)
    for crate in crates:
        for label, cargo_command, extra_env in crate_checks(crate, packages[crate], cargo):
            print(f"ci-lint-affected: {crate} ({label})", file=sys.stderr, flush=True)
            env = dict(os.environ, **extra_env)
            status = subprocess.run(
                [*budget, "--", str(lint_wrapper), "--check", *cargo_command],
                cwd=ROOT,
                env=env,
            ).returncode
            if status != 0:
                print(
                    f"ci-lint-affected: FAILED {crate} ({label}, exit {status})",
                    file=sys.stderr,
                    flush=True,
                )
                return status
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (GateError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"ci-lint-affected: {error}", file=sys.stderr)
        sys.exit(1)
