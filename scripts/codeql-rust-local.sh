#!/usr/bin/env bash
# Local Rust extraction must retain semantic analysis before results are usable.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/codeql-rust-local.sh [--ram MB] [--toolchain VERSION] QUERY_OR_SUITE

Run against this checkout using codeql and rustup already on PATH.
Default: --ram 16384 --toolchain 1.97.0 (CodeQL 2.27.1 extractor).
QUERY_OR_SUITE is a CodeQL pack selector, .ql file, or .qls suite.
All preparation, caches, logs, database and SARIF stay in a new directory
under $ORBIT_SCRATCH_DIR, or this checkout's .orbit/tmp when unset.
See docs/runbooks/codeql-local.md for confirmation and failure handling.
EOF
}

fail() {
  echo "codeql-rust-local: $*" >&2
  exit 1
}

ram=16384
toolchain=1.97.0
query=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ram|--toolchain)
      [[ $# -ge 2 && -n "$2" ]] || fail "$1 requires a value"
      if [[ "$1" == --ram ]]; then ram="$2"; else toolchain="$2"; fi
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    -*) fail "unknown option: $1" ;;
    *)
      [[ -z "$query" ]] || fail "provide exactly one query or suite"
      query="$1"
      shift
      ;;
  esac
done
[[ -n "$query" ]] || { usage >&2; exit 2; }
[[ "$ram" =~ ^[1-9][0-9]*$ ]] || fail "--ram must be a positive number of MiB"
[[ "$toolchain" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[[:alnum:]-]+)?$ ]] || fail "--toolchain must name a pinned Rust version"
command -v codeql >/dev/null || fail "codeql is required on PATH (use an existing CodeQL bundle)"
command -v rustup >/dev/null || fail "rustup is required on PATH to prepare Rust $toolchain"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"
scratch_root="${ORBIT_SCRATCH_DIR:-$repo_root/.orbit/tmp}"
mkdir -p "$scratch_root" || fail "cannot prepare scratch $scratch_root for Rust $toolchain"
scratch_root="$(cd "$scratch_root" && pwd -P)"
config="$repo_root/.github/codeql/codeql-config.yml"
[[ -f "$config" ]] || fail "missing CodeQL configuration $config"

# CodeQL enumerates every .rs file under the source root, so scratch inside the
# checkout (this run's rust-src and builds, earlier runs) must be excluded. The
# whole scratch tree is excluded, which is only safe if it holds no tracked source.
scratch_ignore=
if [[ "$scratch_root" == "$repo_root" ]]; then
  fail "scratch $scratch_root is the checkout itself; choose a scratch directory that holds no tracked source"
elif [[ "$scratch_root" == "$repo_root"/* ]]; then
  scratch_rel="${scratch_root#"$repo_root"/}"
  [[ "$scratch_rel" =~ ^[[:alnum:]\ ._/@+-]+$ ]] \
    || fail "scratch $scratch_root contains characters that cannot be excluded from extraction exactly"
  tracked="$(git -C "$repo_root" ls-files -- ":(literal)$scratch_rel")" \
    || fail "cannot verify that scratch $scratch_root holds no tracked source"
  [[ -z "$tracked" ]] || fail "scratch $scratch_root holds tracked checkout files; choose a scratch directory that holds no tracked source"
  scratch_ignore="$scratch_rel/**"
fi

# The run-scoped configuration is the repository one plus the scratch exclusion
# as the first paths-ignore entry; any other paths-ignore layout is refused.
effective_config="$(awk -v ignore="$scratch_ignore" '
  function add(indent) { if (ignore != "") print indent "- \"" ignore "\"" }
  pending && /^[[:space:]]*(#.*)?$/ { print; next }
  pending {
    if ($0 !~ /^[[:space:]]*-([[:space:]]|$)/) { bad = 1; exit }
    match($0, /^[[:space:]]*/); add(substr($0, 1, RLENGTH)); pending = 0
  }
  /^paths-ignore:/ {
    if (seen || $0 !~ /^paths-ignore:[[:space:]]*(#.*)?$/) { bad = 1; exit }
    seen = 1; pending = 1
  }
  { print }
  END {
    if (bad || pending) exit 1
    if (!seen && ignore != "") { print "paths-ignore:"; add("  ") }
  }' "$config")" \
  || fail "cannot add the scratch exclusion to $config; paths-ignore must be one top-level block list"

run_dir="$(mktemp -d "$scratch_root/codeql-rust-local.XXXXXX")" || fail "cannot create run scratch for Rust $toolchain"
echo "codeql-rust-local: run directory: $run_dir" >&2

printf '%s\n' "$effective_config" >"$run_dir/codeql-config.yml"

export RUSTUP_HOME="$run_dir/rustup"
export CARGO_HOME="$run_dir/cargo"
export RUSTUP_TOOLCHAIN="$toolchain"
export CARGO_TARGET_DIR="$run_dir/target"
export TMPDIR="$run_dir/tmp"
export XDG_CACHE_HOME="$run_dir/cache"
# Keep Cargo from invoking a host compiler cache through repository config.
export RUSTC_WRAPPER=
export RUSTC_WORKSPACE_WRAPPER=
mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" "$CARGO_TARGET_DIR" "$TMPDIR" "$XDG_CACHE_HOME" "$run_dir/codeql-logs"

if ! rustup toolchain install "$toolchain" --profile minimal --component rust-src --no-self-update >"$run_dir/toolchain.log" 2>&1; then
  cat "$run_dir/toolchain.log" >&2
  fail "cannot prepare required Rust $toolchain with rust-src in $RUSTUP_HOME; no query was run (see $run_dir/toolchain.log)"
fi

common=("--common-caches=$run_dir/codeql-cache" "--logdir=$run_dir/codeql-logs")
database="$run_dir/database"
create_status=0
codeql database create "$database" --language=rust --build-mode=none \
  "--source-root=$repo_root" "--codescanning-config=$run_dir/codeql-config.yml" \
  "--ram=$ram" "${common[@]}" >"$run_dir/extraction.log" 2>&1 || create_status=$?

# Extractor warnings may only be in database/log, not the console stream.
# Reject all extraction warnings/errors as well as explicit fallback messages:
# an unknown new warning must not turn an incomplete database into acceptance.
bad_extraction='semantic (analyzer|analysis).*(unavailable|skip|disabl|fail)|macro expansion.*(skip|disabl|unavailable|fail)|(skip|disabl).*(semantic|macro expansion)|(^|[[:space:]]|\[)(WARN(ING)?|ERROR|FATAL)([[:space:]]|:|\]|$)'
logs=("$run_dir/extraction.log" "$run_dir/codeql-logs")
if [[ -d "$database/log" ]]; then logs+=("$database/log"); fi
scan_status=0
grep -rEin "$bad_extraction" "${logs[@]}" >"$run_dir/extraction-problems.log" || scan_status=$?
if [[ "$scan_status" -eq 0 ]]; then
  cat "$run_dir/extraction-problems.log" >&2
  fail "incomplete Rust extraction (prepared Rust $toolchain with rust-src); semantic analysis must not be skipped. No query was run; inspect $run_dir/extraction.log and extractor logs for the requested version/cause"
elif [[ "$scan_status" -ne 1 ]]; then
  fail "cannot inspect extraction logs for Rust $toolchain; no query was run"
fi
if [[ "$create_status" -ne 0 ]]; then
  cat "$run_dir/extraction.log" >&2
  fail "extraction failed with exit $create_status using Rust $toolchain; no query was run"
fi

if ! codeql database analyze "$database" "$query" "--ram=$ram" \
  --format=sarifv2.1.0 "--output=$run_dir/results.sarif" \
  --no-default-compilation-cache "--compilation-cache=$run_dir/query-cache" \
  "${common[@]}" >"$run_dir/analysis.log" 2>&1; then
  cat "$run_dir/analysis.log" >&2
  fail "query analysis failed; any partial SARIF is unusable (see $run_dir/analysis.log)"
fi
[[ -s "$run_dir/results.sarif" ]] || fail "query produced no SARIF; see $run_dir/analysis.log"
echo "codeql-rust-local: analysis completed; inspect rule and affected locations in $run_dir/results.sarif"
