#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
workflow="${CI_MACOS_WORKFLOW:-$repo_root/.github/workflows/ci-macos.yml}"
evidence_output=""
# --workspace-build lists the filtered tests from one
# `cargo test --workspace --lib --bins --tests --no-run` build instead of a
# `cargo test -p <crate>` build per filter. Per-package invocations resolve
# features differently from the workspace build (resolver 2), so on a runner
# that also runs the workspace-wide nextest pass they recompile serde, tokio,
# hyper, reqwest and every workspace crate up to orbit-core a second time.
# The default keeps the per-package listing for callers whose `-p` artifacts
# are already warm (the macOS job, local runs). [DANI-10428]
workspace_build=""

usage() {
  echo "usage: $0 [--evidence-output PATH] [--workspace-build]" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --evidence-output)
      [[ -n "${2:-}" ]] || usage
      evidence_output="$2"
      shift 2
      ;;
    --workspace-build)
      workspace_build=1
      shift
      ;;
    *) usage ;;
  esac
done

if ! command -v python3 >/dev/null 2>&1; then
  echo "check-ci-macos: python3 is required; install it before running" >&2
  exit 1
fi

python3 - "$repo_root" "$workflow" "$evidence_output" "$workspace_build" <<'PY'
from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import shlex
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


repo_root = Path(sys.argv[1]).resolve()
workflow = Path(sys.argv[2]).resolve()
evidence_output = Path(sys.argv[3]).resolve() if sys.argv[3] else None
workspace_build = bool(sys.argv[4])

if not workflow.is_file():
    print(f"check-ci-macos: workflow does not exist: {workflow}", file=sys.stderr)
    raise SystemExit(1)

lines = workflow.read_text(encoding="utf-8").splitlines()


def indentation(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def pull_request_paths() -> list[str]:
    entries: list[str] = []

    for index, line in enumerate(lines):
        if line.strip() != "pull_request:":
            continue

        event_indent = indentation(line)
        paths_index = None
        paths_indent = None
        cursor = index + 1
        while cursor < len(lines):
            candidate = lines[cursor]
            stripped = candidate.strip()
            if stripped and indentation(candidate) <= event_indent:
                break
            if stripped == "paths:":
                paths_index = cursor
                paths_indent = indentation(candidate)
                break
            cursor += 1

        if paths_index is None or paths_indent is None:
            continue

        cursor = paths_index + 1
        while cursor < len(lines):
            candidate = lines[cursor]
            stripped = candidate.strip()
            if stripped and indentation(candidate) <= paths_indent:
                break
            if stripped.startswith("-") and indentation(candidate) > paths_indent:
                value = stripped[1:].strip()
                value = re.sub(r"\s+#.*$", "", value).strip()
                if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                    value = value[1:-1]
                entries.append(value)
            cursor += 1

    return entries


def filtered_cargo_tests() -> list[tuple[str, str]]:
    commands: list[tuple[str, str]] = []

    for line in lines:
        if "cargo test" not in line or line.lstrip().startswith("#"):
            continue

        try:
            tokens = shlex.split(line.strip())
        except ValueError as error:
            print(f"check-ci-macos: cannot parse workflow command {line!r}: {error}", file=sys.stderr)
            raise SystemExit(1) from error

        try:
            cargo_index = tokens.index("cargo")
        except ValueError:
            continue

        if cargo_index + 1 >= len(tokens) or tokens[cargo_index + 1] != "test":
            continue

        try:
            separator = tokens.index("--", cargo_index + 2)
        except ValueError:
            separator = len(tokens)

        cargo_args = tokens[cargo_index + 2:separator]
        package = None
        for option in ("-p", "--package"):
            if option in cargo_args:
                package_index = cargo_args.index(option)
                if package_index + 1 < len(cargo_args):
                    package = cargo_args[package_index + 1]
                break

        if package is None:
            continue

        package_index = cargo_args.index(package)
        positional = [token for token in cargo_args[package_index + 1:] if not token.startswith("-")]
        if positional:
            commands.append((package, positional[0]))

    return commands


errors: list[str] = []

for path in pull_request_paths():
    if path.startswith("!") or any(character in path for character in "*?["):
        continue
    if not (repo_root / path).exists():
        errors.append(f"pull_request.paths entry does not exist: {path}")

filtered_tests = filtered_cargo_tests()
if not filtered_tests:
    errors.append("no filtered cargo test commands found in the macOS workflow")


def run_listing(command: list[str], label: str) -> str | None:
    result = subprocess.run(command, cwd=repo_root, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        errors.append(f"{label} (exit {result.returncode}): {result.stderr.strip()}")
        return None
    return result.stdout


def workspace_test_executables() -> dict[str, list[str]]:
    """Map each workspace package to the test binaries of one workspace build.

    These are the exact artifacts `cargo nextest run --workspace --lib --bins
    --tests` uses, so listing from them adds no compile work. Doctests are not
    listed (the per-package path includes them); the guard only needs >= 1 match.
    """
    metadata = run_listing(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        "cargo metadata failed",
    )
    build = run_listing(
        ["cargo", "test", "--workspace", "--lib", "--bins", "--tests", "--locked", "--no-run",
         "--message-format", "json"],
        "workspace test build failed",
    )
    if metadata is None or build is None:
        return {}
    names = {pkg["id"]: pkg["name"] for pkg in json.loads(metadata)["packages"]}
    executables: dict[str, list[str]] = {}
    for line in build.splitlines():
        if not line.startswith("{"):
            continue
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if not message.get("profile", {}).get("test"):
            continue
        name = names.get(message.get("package_id"))
        if name is not None:
            executables.setdefault(name, []).append(message["executable"])
    return executables


def list_filtered_tests(package: str, test_filter: str) -> str | None:
    if not workspace_build:
        return run_listing(
            ["cargo", "test", "-p", package, "--locked", test_filter, "--", "--list"],
            f"filtered cargo test failed for {package} {test_filter!r}",
        )
    binaries = executables.get(package)
    if not binaries:
        errors.append(f"workspace build produced no test binaries for package {package}")
        return None
    listing = ""
    for binary in binaries:
        stdout = run_listing([binary, "--list", test_filter], f"listing {binary} {test_filter!r} failed")
        if stdout is None:
            return None
        listing += stdout
    return listing


executables = workspace_test_executables() if workspace_build and filtered_tests else {}

for package, test_filter in filtered_tests:
    listing = list_filtered_tests(package, test_filter)
    if listing is None:
        continue

    test_count = sum(1 for line in listing.splitlines() if re.search(r": test\s*$", line))
    if test_count == 0:
        errors.append(f"filtered cargo test matched zero tests: {package} {test_filter}")
    else:
        print(f"check-ci-macos: {package} {test_filter} matched {test_count} tests")

if errors:
    for error in errors:
        print(f"check-ci-macos: {error}", file=sys.stderr)
    raise SystemExit(1)

print("check-ci-macos: workflow paths and filtered tests passed")

if evidence_output is not None:
    required_environment = {
        "GITHUB_ACTIONS": "true",
        "RUNNER_OS": "macOS",
    }
    for name, expected in required_environment.items():
        if os.environ.get(name) != expected:
            print(
                f"check-ci-macos: --evidence-output requires {name}={expected!r}",
                file=sys.stderr,
            )
            raise SystemExit(1)
    producer_fields = [
        "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT",
        "GITHUB_WORKFLOW_REF", "GITHUB_WORKFLOW_SHA",
    ]
    missing = [name for name in producer_fields if not os.environ.get(name)]
    if missing:
        print(
            "check-ci-macos: hosted evidence environment is incomplete: " + ", ".join(missing),
            file=sys.stderr,
        )
        raise SystemExit(1)
    if platform.system() != "Darwin":
        print("check-ci-macos: hosted evidence requires a Darwin runtime", file=sys.stderr)
        raise SystemExit(1)

    checked_out_commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=repo_root, text=True
    ).strip()
    source_paths = [
        Path("scripts/check-ci-macos.sh"),
        Path("scripts/qa-full-sweep-inventory.json"),
        Path("scripts/test-qa-full-sweep.py"),
        Path(".github/workflows/ci-macos.yml"),
    ]
    report = {
        "schema_version": 1,
        "evidence_type": "orbit-macos-platform",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "platform": "macos",
        "source_revision": checked_out_commit,
        "command": ["./scripts/check-ci-macos.sh"],
        "outcome": "PASS",
        "assertions": [
            "macos-required-workflow-is-current",
            "macos-check-ran-on-matching-revision",
        ],
        "producer": {
            "system": "github-actions",
            "repository": os.environ["GITHUB_REPOSITORY"],
            "run_id": os.environ["GITHUB_RUN_ID"],
            "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"],
            "workflow_ref": os.environ["GITHUB_WORKFLOW_REF"],
            "workflow_sha": os.environ["GITHUB_WORKFLOW_SHA"],
        },
        "source_identity": {
            str(path): hashlib.sha256((repo_root / path).read_bytes()).hexdigest()
            for path in source_paths
        },
    }
    evidence_output.parent.mkdir(parents=True, exist_ok=True)
    evidence_output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"check-ci-macos: wrote hosted evidence to {evidence_output}")
PY
