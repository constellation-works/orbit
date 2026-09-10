#!/usr/bin/env bash
# Canonical cargo-deny runner with support for writable isolated advisory database [ORB-11983].
#
# Supports running cargo-deny in managed/sandboxed environments where ~/.cargo or
# the ambient advisory database path is read-only.
#
# Configuration:
#   CARGO_DENY_DB_PATH / ORBIT_CARGO_DENY_DB_PATH:
#     Path to a writable advisory database root (directory where cargo-deny stores
#     or locks advisory databases) or directly to an advisory-db repository.
#   CARGO_DENY_DISABLE_FETCH / ORBIT_CARGO_DENY_DISABLE_FETCH /
#   CARGO_DENY_OFFLINE / ORBIT_CARGO_DENY_OFFLINE:
#     When set to "1" or "true", passes --disable-fetch to cargo-deny check to validate
#     against a local snapshot without network access.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
deny_toml="$repo_root/deny.toml"

CARGO="${CARGO:-cargo}"
DENY_CMD=()
if command -v cargo-deny >/dev/null 2>&1; then
  DENY_CMD=(cargo-deny)
elif "$CARGO" deny --version >/dev/null 2>&1; then
  DENY_CMD=("$CARGO" deny)
else
  echo "cargo-deny not found; install via: cargo install cargo-deny --locked" >&2
  exit 1
fi

db_path="${CARGO_DENY_DB_PATH:-${ORBIT_CARGO_DENY_DB_PATH:-}}"

# Resolve offline / disable-fetch intent
disable_fetch=false
if [[ "${CARGO_DENY_DISABLE_FETCH:-${ORBIT_CARGO_DENY_DISABLE_FETCH:-0}}" == "1" ]] || \
   [[ "${CARGO_DENY_DISABLE_FETCH:-${ORBIT_CARGO_DENY_DISABLE_FETCH:-false}}" == "true" ]] || \
   [[ "${CARGO_DENY_OFFLINE:-${ORBIT_CARGO_DENY_OFFLINE:-0}}" == "1" ]] || \
   [[ "${CARGO_DENY_OFFLINE:-${ORBIT_CARGO_DENY_OFFLINE:-false}}" == "true" ]]; then
  disable_fetch=true
fi

args=("$@")
if [[ ${#args[@]} -eq 0 ]]; then
  args=("check")
fi

is_check=false
has_disable_fetch=false
has_config=false
for ((i=0; i<${#args[@]}; i++)); do
  arg="${args[i]}"
  if [[ "$arg" == "check" ]]; then
    is_check=true
  fi
  if [[ "$arg" == "--disable-fetch" || "$arg" == "-d" || "$arg" == "--offline" ]]; then
    has_disable_fetch=true
  fi
  if [[ "$arg" == "--config" || "$arg" == "-c" ]]; then
    has_config=true
  fi
done

extra_args=()
if [[ "$is_check" == true && "$disable_fetch" == true && "$has_disable_fetch" == false ]]; then
  extra_args+=("--disable-fetch")
fi

temp_cfg=""
temp_link_dir=""
cleanup() {
  if [[ -n "$temp_cfg" && -f "$temp_cfg" ]]; then
    rm -f "$temp_cfg"
  fi
  if [[ -n "$temp_link_dir" && -d "$temp_link_dir" ]]; then
    rm -rf "$temp_link_dir"
  fi
}
trap cleanup EXIT INT TERM

if [[ -n "$db_path" && "$has_config" == false ]]; then
  if [[ "$db_path" != /* ]]; then
    db_path="$PWD/$db_path"
  fi

  target_db_dir="$db_path"
  # If db_path points directly to the advisory-db git repository rather than its parent directory:
  if [[ -d "$db_path/crates" && ! -d "$db_path/advisory-db-3157b0e258782691" ]]; then
    if [[ "$(basename "$db_path")" == advisory-db-* ]]; then
      target_db_dir="$(dirname "$db_path")"
    else
      temp_link_dir="$(mktemp -d "${TMPDIR:-/tmp}/orbit-advisory-link-XXXXXX")"
      ln -s "$db_path" "$temp_link_dir/advisory-db-3157b0e258782691"
      target_db_dir="$temp_link_dir"
    fi
  fi

  temp_cfg="$(mktemp "${TMPDIR:-/tmp}/orbit-deny-XXXXXX.toml")"
  awk -v db="$target_db_dir" '
    /^\[advisories\]/ {
      print;
      print "db-path = \"" db "\"";
      in_advisories = 1;
      next;
    }
    /^\[/ { in_advisories = 0 }
    in_advisories && /^[[:space:]]*db-path[[:space:]]*=/ { next }
    { print }
  ' "$deny_toml" > "$temp_cfg"
fi

exec_args=()
if [[ -n "$temp_cfg" ]]; then
  found_sub=false
  for arg in "${args[@]}"; do
    exec_args+=("$arg")
    if [[ "$arg" == "check" || "$arg" == "fetch" || "$arg" == "list" ]]; then
      exec_args+=(--config "$temp_cfg")
      found_sub=true
    fi
  done
  if [[ "$found_sub" == false ]]; then
    exec_args+=(--config "$temp_cfg")
  fi
else
  exec_args=("${args[@]}")
fi

if [[ ${#extra_args[@]} -gt 0 ]]; then
  exec_args+=("${extra_args[@]}")
fi

status=0
"${DENY_CMD[@]}" "${exec_args[@]}" || status=$?
if [[ $status -ne 0 && -z "$db_path" ]]; then
  advisory_default="${CARGO_HOME:-$HOME/.cargo}/advisory-dbs"
  if [[ -d "$advisory_default" ]] && ! [ -w "$advisory_default" ]; then
    echo "cargo-deny: advisory database '$advisory_default' is read-only." >&2
    echo "cargo-deny: provide a writable database path via CARGO_DENY_DB_PATH or ORBIT_CARGO_DENY_DB_PATH." >&2
  fi
fi

exit $status
