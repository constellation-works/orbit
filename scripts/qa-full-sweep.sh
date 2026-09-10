#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
output="$repo_root/qa-full-sweep-report.json"
run_commands=0
playwright_module=""
website_build=0
orbit_bin="${ORBIT_QA_BINARY:-$(command -v orbit || true)}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    --orbit-bin) orbit_bin="$2"; shift 2 ;;
    --run-commands) run_commands=1; shift ;;
    --playwright-module) playwright_module="$2"; shift 2 ;;
    --website-build) website_build=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$orbit_bin" || ! -x "$orbit_bin" ]]; then
  echo "qa-full-sweep: --orbit-bin must name an executable Orbit binary" >&2
  exit 2
fi

extra_args=()
if [[ "$run_commands" == 1 ]]; then
  extra_args+=(--run-commands)
fi
if [[ -n "$playwright_module" ]]; then
  extra_args+=(--playwright-module "$playwright_module")
fi
if [[ "$website_build" == 1 ]]; then
  extra_args+=(--website-build)
fi

python3 "$repo_root/scripts/test-qa-full-sweep.py" \
  --repo-root "$repo_root" --orbit-bin "$orbit_bin" --output "$output" \
  "${extra_args[@]}"
