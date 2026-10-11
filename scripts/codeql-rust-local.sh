#!/usr/bin/env bash
# Local Rust extraction must retain semantic analysis before results are usable.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/codeql-rust-local.sh [--ram MB] [--toolchain VERSION] QUERY_OR_SUITE

Run against this checkout using codeql and rustup already on PATH.
Linux hosts only: elsewhere it exits 3 before preparing anything.
Default: --ram 16384 --toolchain 1.97.0 (CodeQL 2.27.1 extractor).
QUERY_OR_SUITE is a CodeQL pack selector, .ql file, or .qls suite.
Logs, database and SARIF stay in a new directory under $ORBIT_SCRATCH_DIR,
or this checkout's .orbit/tmp when unset; a passing run then removes the
database dataset and build scratch. The pinned toolchain is installed once
per host into ORBIT_CODEQL_CACHE_DIR (default ~/.orbit/cache/codeql-rust-local),
or into the run directory when that cache is unusable. Extraction and analysis
wait for one of ORBIT_CODEQL_SLOTS host slots (else <cache>/slots, default 2).
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

# Production modules gated on target_os = "linux" are outside the active cfg on
# any other host, so its extractor always skips their semantic analysis and no
# run there can be complete. Refuse with a distinct status before preparing
# anything; the check is owed by a Linux run of the same command.
host_os="$(uname -s)" || fail "cannot determine the host platform"
if [[ "$host_os" != Linux ]]; then
  echo "codeql-rust-local: host platform $host_os cannot extract the Linux-only Rust modules with semantic analysis, so a local run here is never complete; nothing was prepared or queried. Record this exact command as a required not_run check and name a Linux run of it as kind codeql external evidence (docs/runbooks/codeql-local.md, Non-Linux hosts)" >&2
  exit 3
fi

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
scratch_rel=
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

# Earlier runs may have used another scratch inside the checkout. Their run
# directories, recognized by name and the configuration each run writes first,
# are excluded wherever they are; .orbit/ and target/ are already excluded.
# Check the marker before pruning: an unrelated scratch ancestor can have a
# matching name and still contain a marked run directory that must be found.
found="$(find . \( -path ./.git -o -path ./.orbit -o -path ./target ${scratch_rel:+-o -path "./$scratch_rel"} \) -prune \
  -o \( -type d -name 'codeql-rust-local.??????' -exec test -f '{}/codeql-config.yml' \; -print -prune \) | LC_ALL=C sort)" \
  || fail "cannot search $repo_root for earlier run directories to exclude from extraction"
while IFS= read -r prior; do
  prior_rel="${prior#./}"
  [[ "${prior_rel##*/}" =~ ^codeql-rust-local\.[[:alnum:]]{6}$ && -f "$prior/codeql-config.yml" ]] || continue
  [[ "$prior_rel" =~ ^[[:alnum:]\ ._/@+-]+$ ]] \
    || fail "earlier run directory $repo_root/$prior_rel contains characters that cannot be excluded from extraction exactly"
  tracked="$(git -C "$repo_root" ls-files -- ":(literal)$prior_rel")" \
    || fail "cannot verify that earlier run directory $repo_root/$prior_rel holds no tracked source"
  [[ -z "$tracked" ]] || fail "$repo_root/$prior_rel holds tracked checkout files, so it cannot be excluded as an earlier run directory"
  scratch_ignore+="${scratch_ignore:+$'\n'}$prior_rel/**"
done <<<"$found"

# The run-scoped configuration is the repository one plus the scratch and
# earlier-run exclusions as the first paths-ignore entries; any other
# paths-ignore layout is refused.
effective_config="$(IGNORES="$scratch_ignore" awk '
  function add(indent,  n, i, list) {
    n = split(ENVIRON["IGNORES"], list, "\n")
    for (i = 1; i <= n; i++) print indent "- \"" list[i] "\""
  }
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
    if (!seen && ENVIRON["IGNORES"] != "") { print "paths-ignore:"; add("  ") }
  }' "$config")" \
  || fail "cannot add the scratch exclusions to $config; paths-ignore must be one top-level block list"

run_dir="$(mktemp -d "$scratch_root/codeql-rust-local.XXXXXX")" || fail "cannot create run scratch for Rust $toolchain"
echo "codeql-rust-local: run directory: $run_dir" >&2

printf '%s\n' "$effective_config" >"$run_dir/codeql-config.yml"

export CARGO_HOME="$run_dir/cargo"
export RUSTUP_TOOLCHAIN="$toolchain"
export CARGO_TARGET_DIR="$run_dir/target"
export TMPDIR="$run_dir/tmp"
export XDG_CACHE_HOME="$run_dir/cache"
# Keep Cargo from invoking a host compiler cache through repository config.
export RUSTC_WRAPPER=
export RUSTC_WORKSPACE_WRAPPER=
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR" "$TMPDIR" "$XDG_CACHE_HOME" "$run_dir/codeql-logs"

note() {
  echo "codeql-rust-local: $*" >&2
}

# Host state shared by every run: the pinned toolchain and the slot locks.
# Lock files are opened read-only when the cache cannot be written (a reviewer
# or Bubblewrap owner run), which is enough to hold a flock.
cache_dir="${ORBIT_CODEQL_CACHE_DIR:-${HOME:+$HOME/.orbit/cache/codeql-rust-local}}"
if [[ -n "$cache_dir" && "$cache_dir" != /* ]]; then
  fail "ORBIT_CODEQL_CACHE_DIR must be an absolute path"
fi

open_lock() {
  # Sets lock_fd to a descriptor on $1, creating the file when it can.
  { exec {lock_fd}<>"$1"; } 2>/dev/null || { exec {lock_fd}<"$1"; } 2>/dev/null
}

# Content digests of the installed toolchain; any difference is corruption.
toolchain_manifest() {
  (cd "$1" && find toolchains \( -type f -o -type l \) -print0 | LC_ALL=C sort -z | xargs -0r sha256sum --)
}

# The one installed toolchain must be the pinned version with rust-src.
toolchain_is_pinned() {
  local toolchains=("$1"/toolchains/*/) version
  [[ ${#toolchains[@]} -eq 1 && -d "${toolchains[0]}" ]] || return 1
  [[ -f "${toolchains[0]}lib/rustlib/src/rust/library/core/src/lib.rs" ]] || return 1
  version="$("${toolchains[0]}bin/rustc" --version 2>/dev/null)" || return 1
  [[ "$version" == "rustc ${toolchain%%-*} "* || "$version" == "rustc ${toolchain%%-*}-"* ]]
}

cached_toolchain_is_valid() {
  [[ -f "$1/manifest.sha256" ]] && toolchain_is_pinned "$1" \
    && toolchain_manifest "$1" | cmp -s - "$1/manifest.sha256"
}

rustup_install() {
  rustup toolchain install "$toolchain" --profile minimal --component rust-src --no-self-update >>"$run_dir/toolchain.log" 2>&1
}

remove_tree() {
  chmod -R u+w -- "$@" && rm -rf -- "$@"
}

# Installs the pinned toolchain into a fresh staging home and swaps it in whole,
# replacing a missing or corrupt one, so a reader sees all of it or none.
# Called with the exclusive lock held. Returns 1 when the cache is unwritable.
install_cached_toolchain() {
  local version_dir="$1" home="$1/rustup" staging leftover stale=
  staging="$(mktemp -d "$version_dir/.install.XXXXXX" 2>/dev/null)" || return 1
  # Leftovers of interrupted installs; only the lock holder writes here.
  for leftover in "$version_dir"/.install.* "$version_dir"/.stale.*; do
    [[ "$leftover" == "$staging" || ! -e "$leftover" ]] || remove_tree "$leftover" || true
  done
  note "installing Rust $toolchain with rust-src into the host cache $home"
  if ! (export RUSTUP_HOME="$staging" && rustup_install); then
    remove_tree "$staging" || true
    cat "$run_dir/toolchain.log" >&2
    fail "cannot prepare required Rust $toolchain with rust-src in $home; no query was run (see $run_dir/toolchain.log)"
  fi
  if ! toolchain_is_pinned "$staging"; then
    remove_tree "$staging" || true
    fail "rustup did not install Rust $toolchain with rust-src in $home; no query was run (see $run_dir/toolchain.log)"
  fi
  toolchain_manifest "$staging" >"$staging/manifest.sha256" && chmod -R a-w "$staging" \
    || fail "cannot seal the Rust $toolchain install in $staging"
  if [[ -e "$home" ]]; then
    # A same-parent rename; the exclusive lock makes the unused name safe.
    stale="$(mktemp -u "$version_dir/.stale.XXXXXX")" && mv -T -- "$home" "$stale" \
      || fail "cannot set aside the invalid Rust $toolchain install $home"
  fi
  mv -- "$staging" "$home" || fail "cannot move the Rust $toolchain install into $home"
  if [[ -n "$stale" ]]; then remove_tree "$stale" || true; fi
}

# Uses $cache_dir/rust-$toolchain/rustup, installing it first when it is
# missing or fails verification, and keeps a shared lock on it for the rest of
# the run so no install replaces it underneath. Returns 1 when the cache is
# unusable, and this run installs its own toolchain instead.
use_cached_toolchain() {
  local version_dir="$cache_dir/rust-$toolchain" installed=
  { (umask 077 && mkdir -p "$version_dir") 2>/dev/null || [[ -d "$version_dir" ]]; } \
    && open_lock "$version_dir/lock" || return 1
  toolchain_lock_fd="$lock_fd"
  flock -s "$toolchain_lock_fd" || fail "cannot lock the host toolchain cache $version_dir"
  if ! cached_toolchain_is_valid "$version_dir/rustup"; then
    # Conversion to exclusive is not atomic: another run may install first.
    flock -x "$toolchain_lock_fd" || fail "cannot lock the host toolchain cache $version_dir"
    if ! cached_toolchain_is_valid "$version_dir/rustup"; then
      if ! install_cached_toolchain "$version_dir"; then
        exec {toolchain_lock_fd}>&-
        return 1
      fi
      installed=1
    fi
    flock -s "$toolchain_lock_fd" || fail "cannot lock the host toolchain cache $version_dir"
  fi
  export RUSTUP_HOME="$version_dir/rustup"
  if [[ -z "$installed" ]]; then
    echo "codeql-rust-local: using cached Rust $toolchain with rust-src from $RUSTUP_HOME, verified against its manifest; no rustup install ran" \
      >>"$run_dir/toolchain.log"
    note "using cached Rust $toolchain with rust-src from $RUSTUP_HOME"
  fi
}

: >"$run_dir/toolchain.log"
if [[ -z "$cache_dir" ]] || ! use_cached_toolchain; then
  note "host toolchain cache ${cache_dir:-(no HOME)} is unusable; installing Rust $toolchain for this run only"
  export RUSTUP_HOME="$run_dir/rustup"
  mkdir -p "$RUSTUP_HOME"
  if ! rustup_install; then
    cat "$run_dir/toolchain.log" >&2
    fail "cannot prepare required Rust $toolchain with rust-src in $RUSTUP_HOME; no query was run (see $run_dir/toolchain.log)"
  fi
fi

# A counted host lock bounds concurrent extraction and analysis, which saturate
# the disk well before the CPUs. ORBIT_CODEQL_SLOTS overrides <cache>/slots.
slots="${ORBIT_CODEQL_SLOTS:-}"
if [[ -z "$slots" && -n "$cache_dir" && -f "$cache_dir/slots" ]]; then
  slots="$(tr -d '[:space:]' <"$cache_dir/slots")" || fail "cannot read the CodeQL slot count from $cache_dir/slots"
fi
slots="${slots:-2}"
[[ "$slots" =~ ^[1-9][0-9]*$ && "$slots" -le 128 ]] || fail "the CodeQL slot count must be from 1 through 128; got '$slots'"

now_us() {
  local now="$EPOCHREALTIME"
  echo "${now//[!0-9]/}"
}

seconds_since() {
  local elapsed=$(($(now_us) - $1))
  printf '%d.%01d' $((elapsed / 1000000)) $((elapsed % 1000000 / 100000))
}

# Sets slot_fd and reports the queued time, or warns and runs unbounded when
# the slot locks cannot be opened.
acquire_slot() {
  local slot_dir="$cache_dir/slots" fds=() fd slot started next_report waiting= report_every=45
  # The behavior test shortens the progress interval.
  if [[ "${_ORBIT_CODEQL_WAIT_REPORT_SECONDS:-}" =~ ^[1-9][0-9]{0,3}$ ]]; then
    report_every="$_ORBIT_CODEQL_WAIT_REPORT_SECONDS"
  fi
  slot_fd=
  if [[ -n "$cache_dir" ]] && { (umask 077 && mkdir -p "$slot_dir") 2>/dev/null || [[ -d "$slot_dir" ]]; }; then
    for ((slot = 1; slot <= slots; slot++)); do
      open_lock "$slot_dir/slot-$(printf '%03d' "$slot").lock" || break
      fds+=("$lock_fd")
    done
  fi
  if [[ ${#fds[@]} -ne "$slots" ]]; then
    for fd in "${fds[@]}"; do exec {fd}>&-; done
    note "cannot open the CodeQL slot locks under ${cache_dir:-(no HOME)}; running without a host slot"
    return 0
  fi
  started="$(now_us)"
  while true; do
    for slot in "${!fds[@]}"; do
      flock -n "${fds[$slot]}" || continue
      slot_fd="${fds[$slot]}"
      for fd in "${fds[@]}"; do [[ "$fd" == "$slot_fd" ]] || exec {fd}>&-; done
      note "acquired CodeQL slot $((slot + 1)) of $slots after queueing $(seconds_since "$started")s"
      return 0
    done
    if [[ -z "$waiting" ]]; then
      note "waiting for a CodeQL slot (slots=$slots, slot_dir=$slot_dir)"
      waiting=1
      next_report=$(($(now_us) + report_every * 1000000))
    elif (($(now_us) >= next_report)); then
      note "still waiting for a CodeQL slot (elapsed $(seconds_since "$started")s)"
      next_report=$(($(now_us) + report_every * 1000000))
    fi
    sleep 0.5
  done
}
acquire_slot

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
if [[ -n "$slot_fd" ]]; then exec {slot_fd}>&-; fi

# The evidence of a passing run is its logs, configuration, database/log,
# database/src.zip and SARIF. The dataset and build scratch only cost disk; a
# failed run exits above and keeps everything for diagnosis.
shopt -s nullglob
pruned=("$database"/db-* "$CARGO_TARGET_DIR" "$CARGO_HOME" "$TMPDIR" "$XDG_CACHE_HOME" \
  "$run_dir/codeql-cache" "$run_dir/query-cache")
if [[ "$RUSTUP_HOME" == "$run_dir/rustup" ]]; then pruned+=("$RUSTUP_HOME"); fi
rm -rf -- "${pruned[@]}" || note "could not remove all of the database dataset and build scratch in $run_dir"
echo "codeql-rust-local: analysis completed; inspect rule and affected locations in $run_dir/results.sarif"
