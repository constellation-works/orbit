#!/usr/bin/env bash
# Provenance-safe before/after validation across two immutable revisions [ORB-11981].
#
# Runs the same command once per revision in an independent scratch extract with
# its own build target directory, then reports each arm's producer exit status
# and a bounded tail of its log. Each guard below answers a measured false
# before/after result, not a suspected cache defect:
#
#   F2026-09-064  a shared CARGO_TARGET_DIR let the second arm reuse the first
#                 extract's embedded fixture paths     -> per-arm target dirs
#   F2026-09-081  archive/tar mtimes are older than an existing build, so the
#                 build skipped and reran a stale binary -> mtime normalization
#   F2026-09-082  an archived baseline compiled under the configured rustc
#                 wrapper and listed current-worktree tests -> cache opt-out
#   F2026-09-077  `cargo ... | tail` reported tail's success -> status captured
#                 from a redirect before any bounded display
#
# Git metadata is read-only (rev-parse + archive under GIT_OPTIONAL_LOCKS=0).
# The source checkout is never written and no Orbit state is touched.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

REPO="$ROOT"
BASELINE_REV=""
CANDIDATE_REV=""
BASELINE_MARKER=""
CANDIDATE_MARKER=""
EXPECT_BASELINE="pass"
EXPECT_CANDIDATE="pass"
WORKDIR=""
TAIL_LINES="40"
KEEP_COMPILER_CACHE=0
COMMAND=()
OWNED_WORKDIR=0

usage() {
  cat <<'EOF'
usage: scripts/cross-revision-check.sh --baseline <rev> --candidate <rev> [options] -- <command> [args...]

Runs <command> once per revision in an isolated extract and reports both arms.

Required:
  --baseline <rev>            revision validated as "before"
  --candidate <rev>           revision validated as "after"

Options:
  --repo <dir>                source checkout to read revisions from (default: this repository)
  --baseline-marker <text>    literal text the baseline log must contain
  --candidate-marker <text>   literal text the candidate log must contain
  --expect-baseline <pass|fail>   intended baseline outcome (default: pass)
  --expect-candidate <pass|fail>  intended candidate outcome (default: pass)
  --workdir <dir>             scratch root; must live outside the source checkout
  --tail <n>                  log lines to display per arm (default: 40)
  --keep-compiler-cache       do not force the compiler-cache opt-out
  -h, --help                  show this help

When both markers are given, each arm's log must contain its own marker and must
not contain the other arm's. That is how a stale binary or a reused fixture path
from the sibling revision is detected rather than assumed absent.

Proving a regression test detects the original fault:

  scripts/cross-revision-check.sh \
    --baseline <pre-fix-sha> --candidate <post-fix-sha> \
    --expect-baseline fail --expect-candidate pass \
    --baseline-marker 'test result: FAILED' \
    --candidate-marker 'test result: ok' \
    -- cargo test -p orbit-core --lib

Pass the producer directly. Do not wrap it in a pipeline: a filter at the end of
your own pipeline replaces the producer's exit status, which is the failure this
helper exists to prevent. Bounding is already handled by --tail.
EOF
}

die() {
  printf 'cross-revision-check: %s\n' "$*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --baseline) BASELINE_REV="${2:-}"; shift 2 ;;
    --candidate) CANDIDATE_REV="${2:-}"; shift 2 ;;
    --repo) REPO="${2:-}"; shift 2 ;;
    --baseline-marker) BASELINE_MARKER="${2:-}"; shift 2 ;;
    --candidate-marker) CANDIDATE_MARKER="${2:-}"; shift 2 ;;
    --expect-baseline) EXPECT_BASELINE="${2:-}"; shift 2 ;;
    --expect-candidate) EXPECT_CANDIDATE="${2:-}"; shift 2 ;;
    --workdir) WORKDIR="${2:-}"; shift 2 ;;
    --tail) TAIL_LINES="${2:-}"; shift 2 ;;
    --keep-compiler-cache) KEEP_COMPILER_CACHE=1; shift ;;
    -h | --help) usage; exit 0 ;;
    --) shift; COMMAND=("$@"); break ;;
    *) die "unknown argument: $1 (use -- before the command)" ;;
  esac
done

[[ -n "$BASELINE_REV" ]] || die "--baseline is required"
[[ -n "$CANDIDATE_REV" ]] || die "--candidate is required"
[[ "${#COMMAND[@]}" -gt 0 ]] || die "no command given; put it after --"
[[ "$TAIL_LINES" =~ ^[0-9]+$ ]] || die "--tail expects a number, got '$TAIL_LINES'"

for expectation in "$EXPECT_BASELINE" "$EXPECT_CANDIDATE"; do
  case "$expectation" in
    pass | fail) ;;
    *) die "--expect-baseline/--expect-candidate accept pass or fail, got '$expectation'" ;;
  esac
done

[[ -d "$REPO" ]] || die "--repo is not a directory: $REPO"
REPO="$(cd "$REPO" && pwd -P)"

# All Git access is read-only. GIT_OPTIONAL_LOCKS=0 keeps a managed read-only
# .git mount from failing on an index refresh it never needed.
git_ro() {
  GIT_OPTIONAL_LOCKS=0 git -C "$REPO" "$@"
}

git_ro rev-parse --git-dir >/dev/null 2>&1 || die "not a Git repository: $REPO"

resolve_rev() {
  local rev="$1"
  git_ro rev-parse --verify --quiet "${rev}^{commit}" \
    || die "cannot resolve revision '$rev' in $REPO"
}

BASELINE_SHA="$(resolve_rev "$BASELINE_REV")"
CANDIDATE_SHA="$(resolve_rev "$CANDIDATE_REV")"

# Canonicalize a path that need not exist yet, by resolving its closest existing
# ancestor. A refused --workdir must not have been created first.
abs_path() {
  local path="$1" suffix=""
  case "$path" in
    /*) ;;
    *) path="$PWD/$path" ;;
  esac
  while [[ ! -d "$path" ]]; do
    suffix="/$(basename "$path")$suffix"
    path="$(dirname "$path")"
    [[ "$path" != "/" ]] || break
  done
  printf '%s%s\n' "$(cd "$path" && pwd -P)" "$suffix"
}

if [[ -n "$WORKDIR" ]]; then
  WORKDIR="$(abs_path "$WORKDIR")"
  # Writing scratch trees or build output into the checkout would mutate the
  # very state under validation, and .orbit is canonical Orbit state.
  case "$WORKDIR" in
    "$REPO" | "$REPO"/*)
      die "--workdir must live outside the source checkout ($WORKDIR is inside $REPO)"
      ;;
    */.orbit | */.orbit/*)
      die "--workdir must not be inside Orbit state: $WORKDIR"
      ;;
  esac
  mkdir -p "$WORKDIR"
else
  WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/orbit-cross-revision-XXXXXX")"
  OWNED_WORKDIR=1
fi

# A self-made scratch root is disposable only when every check held; otherwise
# the logs are the evidence and must outlive the run.
RUN_FAILED=1

cleanup() {
  [[ "$OWNED_WORKDIR" -eq 1 ]] || return 0
  if [[ "$RUN_FAILED" -eq 0 ]]; then
    rm -rf "$WORKDIR"
  else
    printf 'cross-revision-check: logs preserved at %s\n' "$WORKDIR" >&2
  fi
}
trap cleanup EXIT

# Extract a revision into its own tree. `git archive` embeds the commit time in
# the entries it writes, so a tree restored beside an older build can look
# up-to-date; normalize every extracted mtime to now (F2026-09-081).
extract_revision() {
  local sha="$1" tree="$2"
  mkdir -p "$tree"
  git_ro archive "$sha" | tar -x -C "$tree"
  find "$tree" \( -type f -o -type d \) -exec touch {} +
}

# Report fields for the arm just executed.
ARM_STATUS=0
ARM_LOG=""
ARM_TREE=""
ARM_TARGET=""

run_arm() {
  local arm="$1" sha="$2"
  ARM_TREE="$WORKDIR/$arm"
  ARM_TARGET="$WORKDIR/$arm-target"
  ARM_LOG="$WORKDIR/$arm.log"

  extract_revision "$sha" "$ARM_TREE"
  mkdir -p "$ARM_TARGET"

  local arm_env=(
    "CARGO_TARGET_DIR=$ARM_TARGET"
    "CARGO_INCREMENTAL=0"
    # The extract has no .git. Stop any git the command runs from walking out of
    # the scratch root and describing an unrelated repository as this revision.
    "GIT_CEILING_DIRECTORIES=$WORKDIR"
  )
  if [[ "$KEEP_COMPILER_CACHE" -eq 0 ]]; then
    # The repository's .cargo/config.toml sets build.rustc-wrapper, and the
    # extract carries it. An empty RUSTC_WRAPPER overrides that config key;
    # ORBIT_COMPILER_CACHE=0 makes the committed wrapper exec plain rustc if
    # some other path still reaches it (F2026-09-082).
    arm_env+=(
      "ORBIT_COMPILER_CACHE=0"
      "RUSTC_WRAPPER="
      "CARGO_BUILD_RUSTC_WRAPPER="
    )
  fi

  # Redirect, then capture. A filter here would report its own success
  # (F2026-09-077); the bounded display reads the file afterwards instead.
  ARM_STATUS=0
  (
    cd "$ARM_TREE"
    env "${arm_env[@]}" "${COMMAND[@]}"
  ) >"$ARM_LOG" 2>&1 || ARM_STATUS=$?
}

outcome_of() {
  [[ "$1" -eq 0 ]] && printf 'pass' || printf 'fail'
}

# Marker verification is the provenance check: an arm that never printed its own
# marker, or that printed its sibling's, did not run the source set it claims.
verify_markers() {
  local arm="$1" log="$2" own="$3" other="$4"
  local result="skipped"

  if [[ -n "$own" ]]; then
    if grep -Fq -- "$own" "$log"; then
      result="ok"
    else
      printf 'cross-revision-check: %s log is missing its own marker: %s\n' "$arm" "$own" >&2
      printf 'unverified'
      return 1
    fi
  fi

  if [[ -n "$other" ]] && grep -Fq -- "$other" "$log"; then
    printf 'cross-revision-check: %s log contains the sibling revision marker: %s\n' "$arm" "$other" >&2
    printf 'contaminated'
    return 1
  fi

  printf '%s' "$result"
}

cache_mode="disabled (ORBIT_COMPILER_CACHE=0, empty RUSTC_WRAPPER)"
[[ "$KEEP_COMPILER_CACHE" -eq 0 ]] || cache_mode="enabled by --keep-compiler-cache"

printf 'cross-revision-check\n'
printf '  repo=%s\n' "$REPO"
printf '  baseline=%s (%s)\n' "$BASELINE_SHA" "$BASELINE_REV"
printf '  candidate=%s (%s)\n' "$CANDIDATE_SHA" "$CANDIDATE_REV"
printf '  command=%s\n' "${COMMAND[*]}"
printf '  workdir=%s\n' "$WORKDIR"
printf '  compiler_cache=%s\n' "$cache_mode"

failures=0

report_arm() {
  local arm="$1" sha="$2" expectation="$3" own_marker="$4" other_marker="$5"

  run_arm "$arm" "$sha"

  local status="$ARM_STATUS" log="$ARM_LOG"
  local actual
  actual="$(outcome_of "$status")"

  local marker_result marker_ok=1
  marker_result="$(verify_markers "$arm" "$log" "$own_marker" "$other_marker")" || marker_ok=0

  printf '\n==> %s  rev=%s\n' "$arm" "$sha"
  printf '    tree=%s\n' "$ARM_TREE"
  printf '    target=%s\n' "$ARM_TARGET"
  printf '    exit_code=%s  outcome=%s  expected=%s\n' "$status" "$actual" "$expectation"
  printf '    provenance=%s\n' "$marker_result"
  printf '    --- last %s log lines (%s) ---\n' "$TAIL_LINES" "$log"
  tail -n "$TAIL_LINES" "$log" | sed 's/^/    /'

  if [[ "$actual" != "$expectation" ]]; then
    printf 'cross-revision-check: %s expected to %s but exited %s\n' "$arm" "$expectation" "$status" >&2
    failures=$((failures + 1))
  fi
  if [[ "$marker_ok" -eq 0 ]]; then
    failures=$((failures + 1))
  fi
}

report_arm baseline "$BASELINE_SHA" "$EXPECT_BASELINE" "$BASELINE_MARKER" "$CANDIDATE_MARKER"
report_arm candidate "$CANDIDATE_SHA" "$EXPECT_CANDIDATE" "$CANDIDATE_MARKER" "$BASELINE_MARKER"

printf '\n'
if [[ "$failures" -ne 0 ]]; then
  printf 'cross-revision-check: FAILED (%s check(s) did not hold)\n' "$failures" >&2
  exit 1
fi

RUN_FAILED=0
printf 'cross-revision-check: ok (both arms matched their expected outcome)\n'
