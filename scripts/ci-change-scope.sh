#!/usr/bin/env bash
# Classify one run for its `Change scope` job (ci.yml, ci-cache-seed.yml,
# ci-macos.yml and codeql.yml). Prints the reason and
# appends `mode=full|docs|skip|affected:<crate>,...` to $GITHUB_OUTPUT. It
# never fails: an unreadable answer, an API error, a selection error or an
# unexpected shape means `full`.
#
#   docs      every changed path is Markdown outside crates/, plugin/ and
#             every prefix FILE_READERS declares in ci-test-affected.py.
#             Markdown under crates/ is compiled in (include_str! assets),
#             plugin/ is the generated skill mirror, and a Cargo test reads
#             docs/design/auto-tasks/1_overview.md and the configuration
#             reference page, so those prefixes are not a docs-only run.
#             scripts/test-* self-tests execute or digest RELEASING.md,
#             npm/README.md and docs/dev/qa/path-leases-listener-sudo.md,
#             which are not FILE_READERS entries, so each is not a docs-only
#             run either. A push also requires a successful
#             completed full check on BEFORE; a previous push may have been
#             replaced while pending. That check is `Check / Clippy / Test`
#             unless DOCS_BEFORE_CHECKS lists one GitHub check name per line.
#             When that variable is set, every named check on BEFORE must be
#             a successful completed GitHub Actions run, and `docs` then
#             overrides `skip`. Unset, `skip` still wins over `docs`.
#   affected:<crate>,...
#             a pull request whose changed paths sit in crates/ (Markdown
#             aside): `ci-test-affected.py --pull-request-scope` names the
#             changed crates and their reverse dependents, and the test build
#             archives only their tests. Everything else, the Check job's
#             guards and Clippy included, still runs workspace-wide.
#   skip      a push whose commit has exactly the tree of the merged pull
#             request's head, and that head's latest `Check / Clippy / Test`
#             succeeded in a run that tested every crate: the same bytes
#             already passed the full gate. The push-only ci-cache-seed.yml
#             uses this same proof before seeding missing dependency archives.
#   full      everything else. A push never narrows to affected crates.
#
# A pull request run that tests every crate says so in a `Test selection`
# notice reading `full`, which the push `skip` requires of the head's run.
#
# Inputs (environment): EVENT, GH_REPO, PR (pull_request), SHA and BEFORE (push).
# PR_CHANGED_FILES (optional): reuse the live PR read's count. When supplied
# but empty or invalid, run in full without another PR-object request. Callers
# without that live read may omit it and fetch the count here.
# DOCS_BEFORE_CHECKS (optional): one GitHub check name per line, replacing the
# default parent check. An empty list is not proof.
set -uo pipefail

scripts=$(cd "$(dirname "$0")" && pwd)

api() { gh api "$@" 2>/dev/null; }

# Prefixes a Cargo test reads. docs_only fails closed (not docs) when this
# list cannot be loaded, so a broken import runs the build and test jobs.
cargo_read_markdown_prefixes() {
  python3 - "$scripts/ci-test-affected.py" <<'PY'
import importlib.util
import sys
spec = importlib.util.spec_from_file_location("ci_test_affected", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
for prefix, _readers in module.FILE_READERS:
    print(prefix.as_posix())
PY
}

docs_only() {
  local seen=0 path prefixes prefix
  # A failed load is not docs: skipping tests on an unknown reader list would
  # hide the Cargo-read Markdown this predicate exists to keep.
  if ! prefixes=$(cargo_read_markdown_prefixes); then
    return 1
  fi
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    seen=1
    case "$path" in
      crates/* | plugin/*) return 1 ;;
      # Executed by test-release-promotion.py; copied and digested by the
      # test-qa-full-sweep.py self-test (check_npm_package.py RUNTIME_FILES).
      # test-path-leases-listener-sudo.py executes the listener-sudo recipe.
      # These are not FILE_READERS entries.
      RELEASING.md | npm/README.md | docs/dev/qa/path-leases-listener-sudo.md) return 1 ;;
      *.md)
        # Exact file or any Markdown under a directory prefix. Derived from
        # FILE_READERS so a new Cargo-read page cannot stay docs-only.
        while IFS= read -r prefix; do
          [ -n "$prefix" ] || continue
          case "$path" in
            "$prefix" | "$prefix"/*) return 1 ;;
          esac
        done <<<"$prefixes"
        ;;
      *) return 1 ;;
    esac
  done
  [ "$seen" = 1 ]
}

# Every non-empty name in DOCS_BEFORE_CHECKS must be a successful completed
# GitHub Actions run on BEFORE. An empty list, a missing, failed, cancelled,
# skipped or pending check, or an API error is not proof.
docs_parent_checks_succeeded() {
  local name encoded passed any
  any=0
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    any=1
    encoded=$(jq -nr --arg s "$name" '$s | @uri')
    passed=$(api "repos/$GH_REPO/commits/$BEFORE/check-runs?check_name=${encoded}&filter=latest" |
      jq -r '[.check_runs[] | select(.status == "completed" and .conclusion == "success" and .app.slug == "github-actions")] | length' 2>/dev/null) || true
    if ! [[ "$passed" =~ ^[1-9][0-9]*$ ]]; then
      return 1
    fi
  done < <(printf '%s\n' "$DOCS_BEFORE_CHECKS")
  [ "$any" = 1 ]
}

mode=full
reason="no narrower scope applies"

if [ "${EVENT:-}" = pull_request ]; then
  if [[ -v PR_CHANGED_FILES ]]; then
    count=$PR_CHANGED_FILES
  else
    count=$(api "repos/$GH_REPO/pulls/$PR" | jq -r '.changed_files // empty' 2>/dev/null)
  fi
  # The files endpoint lists at most 3000 entries; beyond that, run in full.
  if [[ "$count" =~ ^[0-9]+$ ]] && ((count > 0 && count < 3000)); then
    pages=$(api --paginate "repos/$GH_REPO/pulls/$PR/files?per_page=100")
    listed=$(jq -s 'map(length) | add // 0' <<<"$pages" 2>/dev/null)
    paths=$(jq -r '.[] | .filename, (.previous_filename // empty)' <<<"$pages" 2>/dev/null)
    if [ "$listed" = "$count" ] && docs_only <<<"$paths"; then
      mode=docs
      reason="all $count changed paths are Markdown outside crates/, plugin/ and Cargo-read paths"
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
  # Unset, only a still-full run can become docs, so skip wins. Set, a
  # docs-only delta whose named parent checks all succeeded becomes docs
  # even when the same tree would otherwise skip.
  if { [ "$mode" = full ] || [ -v DOCS_BEFORE_CHECKS ]; } &&
    [[ "${BEFORE:-}" =~ ^[0-9a-f]{40}$ ]] && [[ ! "$BEFORE" =~ ^0+$ ]]; then
    compare=$(api "repos/$GH_REPO/compare/$BEFORE...$SHA")
    # The compare endpoint lists at most 300 files; beyond that, run in full.
    if [ "$(jq -r '.status // empty' <<<"$compare" 2>/dev/null)" = ahead ] &&
      [[ "$(jq -r '.files | length' <<<"$compare" 2>/dev/null)" =~ ^([1-9]|[1-9][0-9]|[12][0-9][0-9])$ ]] &&
      docs_only <<<"$(jq -r '.files[] | .filename, (.previous_filename // empty)' <<<"$compare" 2>/dev/null)"; then
      if [ -v DOCS_BEFORE_CHECKS ]; then
        if docs_parent_checks_succeeded; then
          mode=docs
          reason="every path pushed since tested commit $BEFORE is Markdown outside crates/, plugin/ and Cargo-read paths, and every configured parent check succeeded"
        fi
      else
        before_passed=$(api "repos/$GH_REPO/commits/$BEFORE/check-runs?check_name=Check%20%2F%20Clippy%20%2F%20Test&filter=latest" |
          jq -r '[.check_runs[] | select(.status == "completed" and .conclusion == "success" and .app.slug == "github-actions")] | length' 2>/dev/null)
        # BEFORE is only the previous tip, not necessarily a tested commit:
        # branch concurrency can replace its pending run. Skipped or unfinished
        # checks do not establish a safe base for a Markdown-only delta.
        if [[ "$before_passed" =~ ^[1-9][0-9]*$ ]]; then
          mode=docs
          reason="every path pushed since tested commit $BEFORE is Markdown outside crates/, plugin/ and Cargo-read paths"
        fi
      fi
    fi
  fi
fi

echo "change scope: $mode ($reason)"
echo "mode=$mode" >>"${GITHUB_OUTPUT:-/dev/null}"
exit 0
