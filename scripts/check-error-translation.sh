#!/usr/bin/env bash
# Crate-boundary error-translation guardrail [ORB-10013].
#
# Enforces docs/design-patterns/error_translation.md: a typed error that
# crosses a crate boundary into `OrbitError` is translated by a single
# `*_error_to_orbit` function that lives in the crate that DEFINES the error,
# never by ad-hoc variant mapping at call sites and never by a translator
# defined in a caller crate. Runs in `make ci-fast` (local) and
# `scripts/ci-guardrails.sh` (CI).
#
# Mechanical approximation (three checks, tuned against false positives):
#
#   1. Registry completeness — every registered boundary error type has a
#      `pub fn <name>_error_to_orbit` translator defined in its owning crate.
#   2. Translator locality — every `fn *_error_to_orbit` definition in the
#      workspace is either a registered translator in its owning crate or an
#      entry in the explicit allowlist below. This blocks the historical
#      failure mode of a caller crate (e.g. orbit-mcp) growing a private
#      translator for another crate's error.
#   3. No ad-hoc boundary mapping — outside the owning crate, a registered
#      error type's name must not appear on the same source line as an
#      `OrbitError::` constructor (the `FooError::X(..) => OrbitError::Y(..)`
#      match-arm shape). Multi-line closures can evade this; the same-line
#      rule is the widest net that stays false-positive-free on this tree.
#
# What this deliberately does NOT flag: one-line stringify conversions
# (`.map_err(|e| OrbitError::Execution(e.to_string()))`) of errors without a
# registered translator — the pattern doc explicitly allows those when no
# kind→variant mapping exists ("When NOT to", bullet 3).
set -euo pipefail

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"
crates_dir="$repo_root/crates"

if ! command -v rg >/dev/null 2>&1; then
  echo "check-error-translation: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

fail=0

# --- Registry: boundary error type -> owning crate -> translator name. ---
# Add a line when a typed error starts crossing into OrbitError; the owning
# crate is where the type (and therefore the translator) must be defined.
registry=(
  "AutomationError:orbit-automation:automation_error_to_orbit"
  "SelectorParseError:orbit-common:selector_error_to_orbit"
  "CatalogError:orbit-engine:catalog_error_to_orbit"
  "RpcError:orbit-search:rpc_error_to_orbit"
  "DispatchError:orbit-engine:dispatch_error_to_orbit"
)

# --- Allowlist: fn names matching *_error_to_orbit that are NOT crate-boundary
# translators. Each entry needs a justifying comment.
#
#   spawn_error_to_orbit — orbit-search/src/subprocess.rs. Private helper that
#   renders a std::io::Error spawn failure with companion-path context. Its
#   input type belongs to std, not to a workspace crate, so there is no owning
#   crate to host a boundary translator (error_translation.md "you don't have
#   a typed error yet" case).
allowlist=(
  "spawn_error_to_orbit"
)

is_allowlisted() {
  local name="$1"
  for entry in "${allowlist[@]}"; do
    if [[ "$entry" == "$name" ]]; then
      return 0
    fi
  done
  return 1
}

registered_translator_crate() {
  local name="$1"
  for entry in "${registry[@]}"; do
    IFS=':' read -r _type crate translator <<<"$entry"
    if [[ "$translator" == "$name" ]]; then
      echo "$crate"
      return 0
    fi
  done
  return 1
}

# Rust sources to scan: crate src/ trees, excluding test code (sibling
# `tests/` modules, crate-root `tests/`, examples, benches).
rust_sources() {
  local crate_filter="$1" # crate name, or "" for all crates
  find "$crates_dir" -name '*.rs' -path '*/src/*' \
    ! -path '*/tests/*' ! -path '*/examples/*' ! -path '*/benches/*' |
    while IFS= read -r file; do
      local rel="${file#"$crates_dir"/}"
      local crate="${rel%%/*}"
      if [[ -z "$crate_filter" || "$crate" == "$crate_filter" ]]; then
        printf '%s\n' "$file"
      fi
    done
}

crate_of() {
  local rel="${1#"$crates_dir"/}"
  echo "${rel%%/*}"
}

# --- Check 1: every registered error type has its translator in its crate. ---
for entry in "${registry[@]}"; do
  IFS=':' read -r err_type crate translator <<<"$entry"
  if [[ ! -d "$crates_dir/$crate/src" ]]; then
    echo "error-translation: registry names missing crate '$crate' (entry: $entry)"
    fail=1
    continue
  fi
  if ! rg -q "pub fn ${translator}\\(" "$crates_dir/$crate/src"; then
    echo "error-translation: crate '$crate' defines boundary error '$err_type' but exports no 'pub fn ${translator}(...)'"
    echo "  define the translator next to the error per docs/design-patterns/error_translation.md"
    fail=1
  fi
done

# --- Check 2: no translator definitions outside the registry/allowlist. ---
while IFS=: read -r file line text; do
  [[ -n "$file" ]] || continue
  name="$(sed -E 's/.*fn ([a-z0-9_]+_error_to_orbit)\(.*/\1/' <<<"$text")"
  crate="$(crate_of "$file")"
  if is_allowlisted "$name"; then
    continue
  fi
  if expected_crate="$(registered_translator_crate "$name")"; then
    if [[ "$crate" != "$expected_crate" ]]; then
      echo "error-translation: ${file#"$repo_root"/}:$line defines '$name' but the registry places it in '$expected_crate'"
      echo "  translators live in the crate that defines the error, not in callers"
      fail=1
    fi
  else
    echo "error-translation: ${file#"$repo_root"/}:$line defines unregistered translator '$name'"
    echo "  add it to the registry in scripts/check-error-translation.sh (owning crate = crate defining the error type),"
    echo "  or to the allowlist with a justifying comment if it is not a crate-boundary translator"
    fail=1
  fi
done < <(rust_sources "" | xargs rg -n "fn [a-z0-9_]+_error_to_orbit\\(" 2>/dev/null || true)

# --- Check 3: no same-line ad-hoc ErrorType -> OrbitError mapping outside the
# owning crate. ---
for entry in "${registry[@]}"; do
  IFS=':' read -r err_type crate translator <<<"$entry"
  while IFS=: read -r file line text; do
    [[ -n "$file" ]] || continue
    if [[ "$(crate_of "$file")" == "$crate" ]]; then
      continue # the owning crate's translator is the one legitimate site
    fi
    echo "error-translation: ${file#"$repo_root"/}:$line maps '$err_type' to OrbitError ad hoc:"
    echo "    ${text#"${text%%[![:space:]]*}"}"
    echo "  use .map_err(${translator}) from '$crate' instead"
    fail=1
  done < <(rust_sources "" | xargs rg -n "\\b${err_type}\\b.*OrbitError::|OrbitError::.*\\b${err_type}\\b" 2>/dev/null || true)
done

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "error translation guard passed"
