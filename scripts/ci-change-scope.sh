#!/usr/bin/env bash
# Classify one ci.yml run for its `Change scope` job. Prints the reason and
# appends `mode=full|docs|skip|affected:<crate>,...` to $GITHUB_OUTPUT. It
# never fails: an unreadable answer, an API error, a selection error or an
# unexpected shape means `full`.
#
#   docs      every changed path is Markdown outside crates/ and plugin/.
#             Markdown under crates/ is compiled in (include_str! assets),
#             plugin/ is the generated skill mirror, and orbit-cli's tests
#             read the configuration reference page, so each needs the full
#             gate. A push also requires a successful completed full check on
#             BEFORE; a previous push may have been replaced while pending.
#   affected:<crate>,...
#             a pull request whose changed paths sit in crates/ (Markdown
#             aside): `ci-test-affected.py --pull-request-scope` names the
#             changed crates and their reverse dependents, and the test build
#             archives only their tests. Everything else, the Check job's
#             guards and Clippy included, still runs workspace-wide.
#   skip      a push whose commit has exactly the tree of the merged pull
#             request's head, and that head's latest `Check / Clippy / Test`
#             succeeded in a run that tested every crate: the same bytes
#             already passed the full gate.
#   full      everything else. A push never narrows to affected crates.
#
# A pull request run that tests every crate says so in a `Test selection`
# notice reading `full`, which the push `skip` requires of the head's run.
#
# Inputs (environment): EVENT, GH_REPO, PR (pull_request), SHA and BEFORE (push).
set -uo pipefail

scripts=$(cd "$(dirname "$0")" && pwd)

api() { gh api "$@" 2>/dev/null; }

docs_only() {
  local seen=0 path
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    seen=1
    case "$path" in
      crates/* | plugin/*) return 1 ;;
      # Read by orbit-cli's tests (FILE_READERS in ci-test-affected.py).
      website/src/content/docs/reference/config.md) return 1 ;;
      *.md) ;;
      *) return 1 ;;
    esac
  done
  [ "$seen" = 1 ]
}

mode=full
reason="no narrower scope applies"

if [ "${EVENT:-}" = pull_request ]; then
  count=$(api "repos/$GH_REPO/pulls/$PR" | jq -r '.changed_files // empty' 2>/dev/null)
  # The files endpoint lists at most 3000 entries; beyond that, run in full.
  if [[ "$count" =~ ^[0-9]+$ ]] && ((count > 0 && count < 3000)); then
    pages=$(api --paginate "repos/$GH_REPO/pulls/$PR/files?per_page=100")
    listed=$(jq -s 'map(length) | add // 0' <<<"$pages" 2>/dev/null)
    paths=$(jq -r '.[] | .filename, (.previous_filename // empty)' <<<"$pages" 2>/dev/null)
    if [ "$listed" = "$count" ] && docs_only <<<"$paths"; then
      mode=docs
      reason="all $count changed paths are Markdown outside crates/ and plugin/"
    elif [ "$listed" = "$count" ]; then
      # Cargo metadata of this checkout (the merge ref) gives the crate graph.
      # The runner's stable toolchain reads it; the pinned one is not needed.
      if selection=$(RUSTUP_TOOLCHAIN=stable python3 "$scripts/ci-test-affected.py" \
        --pull-request-scope - <<<"$paths"); then
        selected=$(sed -n 1p <<<"$selection")
        if [[ "$selected" =~ ^affected:[A-Za-z0-9_-]+(,[A-Za-z0-9_-]+)*$ ]]; then
          mode=$selected
        fi
        reason="test selection: $(sed -n 2p <<<"$selection")"
      else
        reason="test selection failed"
      fi
    fi
  fi
  if [ "$mode" = full ]; then
    echo "::notice title=Test selection::full"
  fi
elif [ "${EVENT:-}" = push ]; then
  tree=$(api "repos/$GH_REPO/git/commits/$SHA" | jq -r '.tree.sha // empty' 2>/dev/null)
  head=$(api "repos/$GH_REPO/commits/$SHA/pulls" |
    SHA="$SHA" jq -r '[.[] | select(.merged_at != null and .merge_commit_sha == env.SHA)][0].head.sha // empty' 2>/dev/null)
  if [ -n "$tree" ] && [ -n "$head" ]; then
    head_tree=$(api "repos/$GH_REPO/git/commits/$head" | jq -r '.tree.sha // empty' 2>/dev/null)
    suite=$(api "repos/$GH_REPO/commits/$head/check-runs?check_name=Check%20%2F%20Clippy%20%2F%20Test&filter=latest" |
      jq -r '[.check_runs[] | select(.conclusion == "success" and .app.slug == "github-actions")][0].check_suite.id // empty' 2>/dev/null)
    # A pull request run may have tested only the affected crates. Only the
    # `Test selection: full` notice of the same run's Change scope job proves
    # that the passing check covered every crate.
    full=0
    if [[ "$suite" =~ ^[0-9]+$ ]]; then
      scope_run=$(api "repos/$GH_REPO/commits/$head/check-runs?check_name=Change%20scope&filter=all" |
        SUITE="$suite" jq -r '[.check_runs[] | select(.app.slug == "github-actions" and (.check_suite.id | tostring) == env.SUITE)][0].id // empty' 2>/dev/null)
      if [[ "$scope_run" =~ ^[0-9]+$ ]]; then
        full=$(api "repos/$GH_REPO/check-runs/$scope_run/annotations" |
          jq -r '[.[] | select(.title == "Test selection" and .message == "full")] | length' 2>/dev/null)
      fi
    fi
    if [ "$tree" = "$head_tree" ] && [[ "$full" =~ ^[1-9][0-9]*$ ]]; then
      mode=skip
      reason="tree $tree is pull request head $head, whose Check / Clippy / Test passed on every crate"
    fi
  fi
  if [ "$mode" = full ] && [[ "${BEFORE:-}" =~ ^[0-9a-f]{40}$ ]] && [[ ! "$BEFORE" =~ ^0+$ ]]; then
    compare=$(api "repos/$GH_REPO/compare/$BEFORE...$SHA")
    # The compare endpoint lists at most 300 files; beyond that, run in full.
    if [ "$(jq -r '.status // empty' <<<"$compare" 2>/dev/null)" = ahead ] &&
      [[ "$(jq -r '.files | length' <<<"$compare" 2>/dev/null)" =~ ^([1-9]|[1-9][0-9]|[12][0-9][0-9])$ ]] &&
      docs_only <<<"$(jq -r '.files[] | .filename, (.previous_filename // empty)' <<<"$compare" 2>/dev/null)"; then
      before_passed=$(api "repos/$GH_REPO/commits/$BEFORE/check-runs?check_name=Check%20%2F%20Clippy%20%2F%20Test&filter=latest" |
        jq -r '[.check_runs[] | select(.status == "completed" and .conclusion == "success" and .app.slug == "github-actions")] | length' 2>/dev/null)
      # BEFORE is only the previous tip, not necessarily a tested commit:
      # branch concurrency can replace its pending run. Skipped or unfinished
      # checks do not establish a safe base for a Markdown-only delta.
      if [[ "$before_passed" =~ ^[1-9][0-9]*$ ]]; then
        mode=docs
        reason="every path pushed since tested commit $BEFORE is Markdown outside crates/ and plugin/"
      fi
    fi
  fi
fi

echo "change scope: $mode ($reason)"
echo "mode=$mode" >>"${GITHUB_OUTPUT:-/dev/null}"
exit 0
