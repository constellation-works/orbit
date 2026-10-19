#!/usr/bin/env bash
# Classify one ci.yml run for its `Change scope` job. Prints the reason and
# appends `mode=full|docs|skip` to $GITHUB_OUTPUT. It never fails: an
# unreadable answer, an API error or an unexpected shape means `full`.
#
#   docs  every changed path is Markdown outside crates/ and plugin/. Markdown
#         under crates/ is compiled in (include_str! assets) and plugin/ is the
#         generated skill mirror, so either one needs the full gate.
#         A push also requires a successful completed full check on BEFORE;
#         a previous push may have been replaced while pending.
#   skip  a push whose commit has exactly the tree of the merged pull request's
#         head, and that head's latest `Check / Clippy / Test` succeeded: the
#         same bytes already passed the full gate.
#   full  everything else.
#
# Inputs (environment): EVENT, GH_REPO, PR (pull_request), SHA and BEFORE (push).
set -uo pipefail

api() { gh api "$@" 2>/dev/null; }

docs_only() {
  local seen=0 path
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    seen=1
    case "$path" in
      crates/* | plugin/*) return 1 ;;
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
    fi
  fi
elif [ "${EVENT:-}" = push ]; then
  tree=$(api "repos/$GH_REPO/git/commits/$SHA" | jq -r '.tree.sha // empty' 2>/dev/null)
  head=$(api "repos/$GH_REPO/commits/$SHA/pulls" |
    SHA="$SHA" jq -r '[.[] | select(.merged_at != null and .merge_commit_sha == env.SHA)][0].head.sha // empty' 2>/dev/null)
  if [ -n "$tree" ] && [ -n "$head" ]; then
    head_tree=$(api "repos/$GH_REPO/git/commits/$head" | jq -r '.tree.sha // empty' 2>/dev/null)
    passed=$(api "repos/$GH_REPO/commits/$head/check-runs?check_name=Check%20%2F%20Clippy%20%2F%20Test&filter=latest" |
      jq -r '[.check_runs[] | select(.conclusion == "success" and .app.slug == "github-actions")] | length' 2>/dev/null)
    if [ "$tree" = "$head_tree" ] && [[ "$passed" =~ ^[1-9][0-9]*$ ]]; then
      mode=skip
      reason="tree $tree is pull request head $head, whose Check / Clippy / Test passed"
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
