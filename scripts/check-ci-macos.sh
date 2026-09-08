#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
workflow="${CI_MACOS_WORKFLOW:-$repo_root/.github/workflows/ci-macos.yml}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "check-ci-macos: python3 is required; install it before running" >&2
  exit 1
fi

python3 - "$repo_root" "$workflow" <<'PY'
from __future__ import annotations

import re
import shlex
import subprocess
import sys
from pathlib import Path


repo_root = Path(sys.argv[1]).resolve()
workflow = Path(sys.argv[2]).resolve()

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

for package, test_filter in filtered_tests:
    command = [
        "cargo",
        "test",
        "-p",
        package,
        "--locked",
        test_filter,
        "--",
        "--list",
    ]
    result = subprocess.run(command, cwd=repo_root, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        errors.append(
            f"filtered cargo test failed for {package} {test_filter!r} "
            f"(exit {result.returncode}): {result.stderr.strip()}"
        )
        continue

    test_count = sum(1 for line in result.stdout.splitlines() if re.search(r": test\s*$", line))
    if test_count == 0:
        errors.append(f"filtered cargo test matched zero tests: {package} {test_filter}")
    else:
        print(f"check-ci-macos: {package} {test_filter} matched {test_count} tests")

if errors:
    for error in errors:
        print(f"check-ci-macos: {error}", file=sys.stderr)
    raise SystemExit(1)

print("check-ci-macos: workflow paths and filtered tests passed")
PY
