#!/usr/bin/env bash
# Preflight for gates that import `tomllib` (check-dependency-direction.sh,
# check-unused-dependencies.py, check_npm_package.py): stop up front with one
# actionable message instead of a `ModuleNotFoundError` traceback. Stock macOS
# ships /usr/bin/python3 3.9, which has no `tomllib`. Never skips: a missing or
# too-old `python3` is a non-zero exit. It does not install or select an
# interpreter; put a newer `python3` first on PATH. --yaml also checks PyYAML
# when the guard runner selects mandatory YAML-dependent fixture self-tests.
#
# Kept to macOS Bash 3.2 syntax like the other guardrail scripts.
set -euo pipefail

case "${1:-}" in
  "" | --yaml) ;;
  *) echo "usage: require-python.sh [--yaml]" >&2; exit 2 ;;
esac

min_major=3
min_minor=11

if ! python_path="$(command -v python3 2>/dev/null)"; then
  echo "python preflight: Python >= ${min_major}.${min_minor} is required (tomllib), but no python3 is on PATH; put one first on PATH" >&2
  exit 1
fi

version="$("$python_path" -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])' 2>/dev/null || true)"
major=""
minor=""
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) IFS=. read -r major minor _ <<<"$version" ;;
esac

if [[ -z "$major" || -z "$minor" ]] ||
  [[ "$major" -lt "$min_major" ]] ||
  { [[ "$major" -eq "$min_major" ]] && [[ "$minor" -lt "$min_minor" ]]; }; then
  echo "python preflight: Python >= ${min_major}.${min_minor} is required (tomllib), found python3 ${version:-of unknown version} at ${python_path}; put a newer python3 first on PATH" >&2
  exit 1
fi

if [[ "${1:-}" == --yaml ]] && ! "$python_path" -c 'import yaml' >/dev/null 2>&1; then
  echo "python preflight: PyYAML (import yaml) is required by the selected fixture self-tests, but python3 ${version} at ${python_path} cannot import it; create a virtual environment with \"${python_path}\" -m venv .venv, activate it (source .venv/bin/activate), then run python3 -m pip install PyYAML. See docs/DEVELOPMENT.md for setup; no packages were installed" >&2
  exit 1
fi
