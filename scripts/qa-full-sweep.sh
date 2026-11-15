#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
output="$repo_root/.orbit/tmp/qa-full-sweep/qa-full-sweep-report.json"
run_commands=0
playwright_module=""
playwright_browsers_path=""
browser_ld_library_path=""
browser_fontconfig_file=""
browser_evidence_dir=""
website_build=0
build_candidate=0
platform_evidence=()
orbit_bin="${ORBIT_QA_BINARY:-$(command -v orbit || true)}"

usage() { echo "usage: qa-full-sweep.sh [--output FILE] [--orbit-bin BIN] [--run-commands] [--playwright-module PATH] [--playwright-browsers-path PATH] [--browser-ld-library-path PATH] [--browser-fontconfig-file FILE] [--browser-evidence-dir DIR] [--website-build] [--build-candidate] [--platform-evidence FILE]..." >&2; }
require_value() {
  if [[ $# -lt 2 ]]; then
    echo "qa-full-sweep: $1 requires a value" >&2
    usage
    exit 2
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output) require_value "$@"; output="$2"; shift 2 ;;
    --orbit-bin) require_value "$@"; orbit_bin="$2"; shift 2 ;;
    --run-commands) run_commands=1; shift ;;
    --playwright-module) require_value "$@"; playwright_module="$2"; shift 2 ;;
    --playwright-browsers-path) require_value "$@"; playwright_browsers_path="$2"; shift 2 ;;
    --browser-ld-library-path) require_value "$@"; browser_ld_library_path="$2"; shift 2 ;;
    --browser-fontconfig-file) require_value "$@"; browser_fontconfig_file="$2"; shift 2 ;;
    --browser-evidence-dir) require_value "$@"; browser_evidence_dir="$2"; shift 2 ;;
    --website-build) website_build=1; shift ;;
    --build-candidate) build_candidate=1; shift ;;
    --platform-evidence) require_value "$@"; platform_evidence+=("$2"); shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ "$build_candidate" == 0 && ( -z "$orbit_bin" || ! -x "$orbit_bin" ) ]]; then
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
if [[ -n "$playwright_browsers_path" ]]; then
  extra_args+=(--playwright-browsers-path "$playwright_browsers_path")
fi
if [[ -n "$browser_ld_library_path" ]]; then
  extra_args+=(--browser-ld-library-path "$browser_ld_library_path")
fi
if [[ -n "$browser_fontconfig_file" ]]; then
  extra_args+=(--browser-fontconfig-file "$browser_fontconfig_file")
fi
if [[ -n "$browser_evidence_dir" ]]; then
  extra_args+=(--browser-evidence-dir "$browser_evidence_dir")
fi
if [[ "$website_build" == 1 ]]; then
  extra_args+=(--website-build)
fi
if [[ "$build_candidate" == 1 ]]; then
  extra_args+=(--build-candidate)
fi
for evidence in ${platform_evidence[@]+"${platform_evidence[@]}"}; do
  extra_args+=(--platform-evidence "$evidence")
done

python3 "$repo_root/scripts/test-qa-full-sweep.py" \
  --repo-root "$repo_root" --orbit-bin "$orbit_bin" --output "$output" \
  ${extra_args[@]+"${extra_args[@]}"}
