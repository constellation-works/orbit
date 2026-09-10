#!/usr/bin/env bash
# Regression fixtures for scripts/cross-revision-check.sh [ORB-11981].
#
# The fixture is a throwaway Git repository with ancient commit dates and a
# different marker per revision, so each guard is proved against the shape of
# the failure that motivated it rather than asserted in prose. No Rust compile:
# the helper is command-agnostic and the probe reports the environment it ran in.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="$ROOT/scripts/cross-revision-check.sh"
TMP="$(mktemp -d)"
SRC="$TMP/src"
START_EPOCH="$(date +%s)"
PRESERVED_WORK=""

# The read-only-.git case leaves the fixture unwritable; restore it before rm.
cleanup() {
  chmod -R u+w "$TMP" 2>/dev/null || true
  rm -rf "$TMP"
  [[ -z "$PRESERVED_WORK" ]] || rm -rf -- "$PRESERVED_WORK"
}
trap cleanup EXIT

# 2020-01-01: far enough back that a preserved archive mtime is unmistakable.
ARCHIVE_EPOCH=1577836800
ARCHIVE_DATE="@$ARCHIVE_EPOCH +0000"

fail() {
  printf 'test-cross-revision-check: FAIL: %s\n' "$*" >&2
  exit 1
}

assert_eq() {
  local got="$1" want="$2" msg="$3"
  [[ "$got" == "$want" ]] || fail "$msg: got '$got' want '$want'"
}

assert_contains() {
  local file="$1" needle="$2" msg="$3"
  grep -Fq -- "$needle" "$file" || fail "$msg: '$needle' not found in $file"
}

assert_absent() {
  local file="$1" needle="$2" msg="$3"
  grep -Fq -- "$needle" "$file" && fail "$msg: '$needle' unexpectedly found in $file"
  return 0
}

file_mtime() {
  if stat -c '%Y' "$1" >/dev/null 2>&1; then
    stat -c '%Y' "$1"
  else
    stat -f '%m' "$1"
  fi
}

# Recursive path + mtime listing, used to prove the source checkout is untouched.
snapshot() {
  local root="$1" path
  (
    cd "$root"
    find . -print | sort | while IFS= read -r path; do
      printf '%s %s\n' "$path" "$(file_mtime "$path")"
    done
  )
}

git_fixture() {
  git -c user.name=orbit-test -c user.email=orbit-test@example.invalid \
    -c init.defaultBranch=main -C "$SRC" "$@"
}

# --- fixture repository -------------------------------------------------------

mkdir -p "$SRC"
git -c init.defaultBranch=main init -q "$SRC"

cat >"$SRC/probe.sh" <<'EOF'
#!/bin/sh
# Reports the provenance and environment of the arm that ran it.
printf 'marker=%s\n' "$(cat marker.txt)"
printf 'target=%s\n' "$CARGO_TARGET_DIR"
printf 'cache=%s\n' "${ORBIT_COMPILER_CACHE-unset}"
printf 'wrapper=[%s]\n' "${RUSTC_WRAPPER-unset}"
printf 'build_wrapper=[%s]\n' "${CARGO_BUILD_RUSTC_WRAPPER-unset}"
printf 'incremental=%s\n' "${CARGO_INCREMENTAL-unset}"
printf 'source_mtime=%s\n' "$(stat -c '%Y' marker.txt 2>/dev/null || stat -f '%m' marker.txt)"
i=0
while [ "$i" -lt "${PROBE_NOISE_LINES:-0}" ]; do
  printf 'noise %s\n' "$i"
  i=$((i + 1))
done
exit "${PROBE_EXIT:-0}"
EOF
chmod +x "$SRC/probe.sh"

# Prints its own revision's marker and then, unconditionally, the candidate
# marker: the shape of a stale binary or reused fixture leaking a sibling
# revision's identity into an arm's output.
cat >"$SRC/leak.sh" <<'EOF'
#!/bin/sh
printf 'marker=%s\n' "$(cat marker.txt)"
printf 'leaked=MARKER_CANDIDATE\n'
EOF
chmod +x "$SRC/leak.sh"

printf 'MARKER_BASELINE\n' >"$SRC/marker.txt"
printf 'baseline-only\n' >"$SRC/baseline-only.txt"
git_fixture add probe.sh leak.sh marker.txt
git_fixture add baseline-only.txt
GIT_AUTHOR_DATE="$ARCHIVE_DATE" GIT_COMMITTER_DATE="$ARCHIVE_DATE" \
  git_fixture commit -q -m "baseline revision"
BASELINE_SHA="$(git_fixture rev-parse HEAD)"

printf 'MARKER_CANDIDATE\n' >"$SRC/marker.txt"
git_fixture rm -q baseline-only.txt
git_fixture add marker.txt
GIT_AUTHOR_DATE="$ARCHIVE_DATE" GIT_COMMITTER_DATE="$ARCHIVE_DATE" \
  git_fixture commit -q -m "candidate revision"
CANDIDATE_SHA="$(git_fixture rev-parse HEAD)"

run_helper() {
  local out="$1"
  shift
  run_helper_revisions "$out" "$BASELINE_SHA" "$CANDIDATE_SHA" "$@"
}

run_helper_revisions() {
  local out="$1" baseline="$2" candidate="$3"
  shift 3
  local status=0
  "$HELPER" --repo "$SRC" --baseline "$baseline" --candidate "$candidate" \
    "$@" >"$out" 2>&1 || status=$?
  printf '%s' "$status"
}

# --- 0. The fixture really does carry stale archive mtimes --------------------
# Without this control, the mtime-normalization assertion below could pass
# against a fixture that was never stale to begin with.

mkdir -p "$TMP/raw-archive"
git -C "$SRC" archive "$BASELINE_SHA" | tar -x -C "$TMP/raw-archive"
assert_eq "$(file_mtime "$TMP/raw-archive/marker.txt")" "$ARCHIVE_EPOCH" \
  "plain git-archive extraction should preserve the ancient commit mtime"

# --- 1. Arm isolation, provenance, mtime normalization, cache opt-out ---------

work="$TMP/work-isolation"
out="$TMP/isolation.out"
status="$(run_helper "$out" --workdir "$work" \
  --baseline-marker MARKER_BASELINE --candidate-marker MARKER_CANDIDATE \
  -- ./probe.sh)"
assert_eq "$status" "0" "matching arms should succeed"

assert_contains "$work/baseline.log" "marker=MARKER_BASELINE" "baseline ran its own source set"
assert_contains "$work/candidate.log" "marker=MARKER_CANDIDATE" "candidate ran its own source set"
assert_absent "$work/baseline.log" "MARKER_CANDIDATE" "baseline must not see the candidate source set"
assert_absent "$work/candidate.log" "MARKER_BASELINE" "candidate must not see the baseline source set"

assert_contains "$work/baseline.log" "target=$work/baseline-target" "baseline uses a private target dir"
assert_contains "$work/candidate.log" "target=$work/candidate-target" "candidate uses a private target dir"
[[ -d "$work/baseline-target" && -d "$work/candidate-target" ]] \
  || fail "each arm should get its own target directory"

for arm in baseline candidate; do
  assert_contains "$work/$arm.log" "cache=0" "$arm must force ORBIT_COMPILER_CACHE=0"
  assert_contains "$work/$arm.log" "wrapper=[]" "$arm must clear RUSTC_WRAPPER"
  assert_contains "$work/$arm.log" "build_wrapper=[]" "$arm must clear CARGO_BUILD_RUSTC_WRAPPER"
  assert_contains "$work/$arm.log" "incremental=0" "$arm must pin CARGO_INCREMENTAL=0"

  arm_mtime="$(sed -n 's/^source_mtime=//p' "$work/$arm.log")"
  [[ -n "$arm_mtime" ]] || fail "$arm probe did not report a source mtime"
  [[ "$arm_mtime" -ge "$START_EPOCH" ]] \
    || fail "$arm source mtime $arm_mtime was not normalized past run start $START_EPOCH"
done

assert_contains "$out" "provenance=ok" "helper should report verified provenance"

# --- 2. Reusing a workdir refreshes trees and targets ------------------------

work="$TMP/work-reuse"
out="$TMP/reuse-first.out"
status="$(run_helper_revisions "$out" "$BASELINE_SHA" "$CANDIDATE_SHA" \
  --workdir "$work" --expect-baseline pass --expect-candidate fail \
  -- sh -c 'touch "$CARGO_TARGET_DIR/stale-target-marker"; test -e baseline-only.txt')"
assert_eq "$status" "0" "the first reused-workdir run should match its revision contents"
[[ -e "$work/baseline/baseline-only.txt" ]] \
  || fail "the baseline revision should contain its baseline-only file"
[[ ! -e "$work/candidate/baseline-only.txt" ]] \
  || fail "the candidate revision should not contain the baseline-only file"

out="$TMP/reuse-second.out"
status="$(run_helper_revisions "$out" "$CANDIDATE_SHA" "$CANDIDATE_SHA" \
  --workdir "$work" \
  -- sh -c 'test ! -e baseline-only.txt && test ! -e "$CARGO_TARGET_DIR/stale-target-marker"')"
assert_eq "$status" "0" "a reused workdir must not retain prior tree or target state"
for arm in baseline candidate; do
  [[ ! -e "$work/$arm/baseline-only.txt" ]] \
    || fail "$arm tree retained a file absent from the current revision"
  [[ ! -e "$work/$arm-target/stale-target-marker" ]] \
    || fail "$arm target retained an artifact from the prior invocation"
done

# --- 3. A failing producer stays a failure behind bounded output -------------

work="$TMP/work-exit"
out="$TMP/exit.out"
status="$(
  export PROBE_EXIT=7 PROBE_NOISE_LINES=200
  run_helper "$out" --workdir "$work" --tail 40 \
    --baseline-marker MARKER_BASELINE --candidate-marker MARKER_CANDIDATE \
    -- ./probe.sh
)"
assert_eq "$status" "1" "a failing producer must fail the helper by default"
assert_contains "$out" "exit_code=7" "the producer exit code must survive the bounded display"
assert_contains "$out" "baseline expected to pass but exited 7" "the helper must name the unmet expectation"

# The bounded tail displays only the trailing noise, yet provenance still holds:
# marker verification reads the full log, and the status came from the producer
# rather than from whatever wrote last.
assert_absent "$out" "marker=MARKER_BASELINE" "tail -40 should have bounded the marker out of the display"
assert_contains "$out" "noise 199" "the bounded display should show the trailing lines"
assert_contains "$out" "provenance=ok" "provenance must be verified against the full log, not the tail"

# The same failing producer is a success only when the operator declared it.
work="$TMP/work-expected-fail"
out="$TMP/expected-fail.out"
status="$(
  export PROBE_EXIT=7
  run_helper "$out" --workdir "$work" --expect-baseline fail --expect-candidate fail -- ./probe.sh
)"
assert_eq "$status" "0" "an explicitly expected failure should pass"

# The regression-proof shape: baseline fails, candidate passes.
work="$TMP/work-ab"
out="$TMP/ab.out"
status=0
"$HELPER" --repo "$SRC" --baseline "$BASELINE_SHA" --candidate "$CANDIDATE_SHA" \
  --workdir "$work" --expect-baseline fail --expect-candidate pass \
  -- sh -c 'marker=$(cat marker.txt); echo "marker=$marker"; [ "$marker" = MARKER_CANDIDATE ]' \
  >"$out" 2>&1 || status=$?
assert_eq "$status" "0" "before-fails/after-passes is the regression-proof shape"

# --- 4. Provenance failures are detected, not assumed absent -----------------

work="$TMP/work-missing-marker"
out="$TMP/missing-marker.out"
status="$(run_helper "$out" --workdir "$work" --baseline-marker NOT_IN_ANY_LOG -- ./probe.sh)"
assert_eq "$status" "1" "a missing own marker must fail the run"
assert_contains "$out" "missing its own marker" "the helper should name the missing marker"

work="$TMP/work-contaminated"
out="$TMP/contaminated.out"
status="$(run_helper "$out" --workdir "$work" \
  --baseline-marker MARKER_BASELINE --candidate-marker MARKER_CANDIDATE -- ./leak.sh)"
assert_eq "$status" "1" "a sibling marker in an arm's log must fail the run"
assert_contains "$out" "contains the sibling revision marker" "the helper should name the contamination"
assert_contains "$out" "provenance=contaminated" "the report should mark the arm contaminated"

# --- 5. Read-only Git metadata and an unmodified source checkout -------------

before="$TMP/snapshot-before"
after="$TMP/snapshot-after"
snapshot "$SRC" >"$before"

chmod -R a-w "$SRC/.git"
work="$TMP/work-readonly"
out="$TMP/readonly.out"
readonly_status="$(run_helper "$out" --workdir "$work" \
  --baseline-marker MARKER_BASELINE --candidate-marker MARKER_CANDIDATE -- ./probe.sh)"
chmod -R u+w "$SRC/.git"
assert_eq "$readonly_status" "0" "the helper must work with read-only Git metadata: $(cat "$out")"

snapshot "$SRC" >"$after"
diff -u "$before" "$after" \
  || fail "the helper must not modify the source checkout (including .git mtimes)"
[[ ! -e "$SRC/.git/index.lock" ]] || fail "the helper must not leave a Git index lock behind"

# --- 6. Scratch state stays outside the checkout and outside Orbit state -----

out="$TMP/inside-repo.out"
status="$(run_helper "$out" --workdir "$SRC/scratch" -- ./probe.sh)"
assert_eq "$status" "1" "a workdir inside the source checkout must be refused"
assert_contains "$out" "must live outside the source checkout" "the refusal should explain itself"
[[ ! -e "$SRC/scratch" ]] || fail "a refused workdir must not be created inside the checkout"

out="$TMP/orbit-state.out"
status="$(run_helper "$out" --workdir "$TMP/.orbit/state/scratch" -- ./probe.sh)"
assert_eq "$status" "1" "a workdir inside .orbit must be refused"
assert_contains "$out" "must not be inside Orbit state" "the refusal should name Orbit state"

# A failed run with an auto-created workdir must retain its logs for diagnosis.
out="$TMP/auto-workdir-failure.out"
status=0
PROBE_EXIT=9 "$HELPER" --repo "$SRC" --baseline "$BASELINE_SHA" --candidate "$CANDIDATE_SHA" \
  -- ./probe.sh >"$out" 2>&1 || status=$?
assert_eq "$status" "1" "an unexpected producer failure should fail the helper"
PRESERVED_WORK="$(sed -n 's/^cross-revision-check: logs preserved at //p' "$out")"
[[ -n "$PRESERVED_WORK" && -d "$PRESERVED_WORK" ]] \
  || fail "a failed auto-workdir run should preserve its scratch root"
[[ -f "$PRESERVED_WORK/baseline.log" && -f "$PRESERVED_WORK/candidate.log" ]] \
  || fail "a failed auto-workdir run should preserve both arm logs"

# --- 7. The cache opt-out is a default, not a hard-coded policy --------------

work="$TMP/work-keep-cache"
out="$TMP/keep-cache.out"
status="$(
  export ORBIT_COMPILER_CACHE=inherited RUSTC_WRAPPER=/inherited/wrapper
  run_helper "$out" --workdir "$work" --keep-compiler-cache -- ./probe.sh
)"
assert_eq "$status" "0" "--keep-compiler-cache should still run both arms"
assert_contains "$work/baseline.log" "cache=inherited" "--keep-compiler-cache must not force the opt-out"
assert_contains "$work/baseline.log" "wrapper=[/inherited/wrapper]" "--keep-compiler-cache must keep the wrapper"
assert_contains "$out" "enabled by --keep-compiler-cache" "the report should disclose the opt-in"

# --- 7. Argument handling ----------------------------------------------------

"$HELPER" --help >/dev/null || fail "--help should succeed"

status=0
"$HELPER" --repo "$SRC" --baseline "$BASELINE_SHA" --candidate "$CANDIDATE_SHA" \
  >"$TMP/no-command.out" 2>&1 || status=$?
assert_eq "$status" "1" "a missing command must be rejected"
assert_contains "$TMP/no-command.out" "no command given" "the helper should name the missing command"

status=0
"$HELPER" --repo "$SRC" --baseline "$BASELINE_SHA" --candidate no-such-revision \
  -- ./probe.sh >"$TMP/bad-rev.out" 2>&1 || status=$?
assert_eq "$status" "1" "an unresolvable revision must be rejected"
assert_contains "$TMP/bad-rev.out" "cannot resolve revision" "the helper should name the bad revision"

status=0
"$HELPER" --repo "$SRC" --baseline "$BASELINE_SHA" --candidate "$CANDIDATE_SHA" \
  --expect-baseline maybe -- ./probe.sh >"$TMP/bad-expect.out" 2>&1 || status=$?
assert_eq "$status" "1" "an unknown expectation must be rejected"

printf 'test-cross-revision-check: ok\n'
