#!/bin/sh
# Deterministic provider for one required aggregate check run's annotations.
# `__ROW__/case` names the one way this answer departs from the authentic one.
set -eu
row='__ROW__'
case=$(cat "$row/case")
head=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
printf '%s\n' "$*" >> "$row/calls"
[ "$1" = api ] || { echo "unexpected gh $*" >&2; exit 2; }
repo_id=500
repo='{"id":500,"full_name":"orbit/test","url":"https://api.github.com/repos/orbit/test"}'
pr_head=$head
if [ "$case" = moved_head ] && [ "$(grep -c 'pulls/42$' "$row/calls")" -gt 1 ]; then
    pr_head=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
fi
head_repo=$repo
[ "$case" != foreign_head_repo ] || head_repo='{"id":501,"full_name":"fork/test"}'
app=57789
[ "$case" != foreign_app ] || app=99
url=https://api.github.com/repos/orbit/test/check-runs/7001
[ "$case" != foreign_repo ] || url=https://api.github.com/repos/foreign/test/check-runs/7001
link_head=$head
[ "$case" != wrong_pr_link ] || link_head=cccccccccccccccccccccccccccccccccccccccc
conclusion=failure
if [ "$case" = changed_run ] && [ "$(grep -c 'check-runs/7001$' "$row/calls")" -gt 0 ]; then
    conclusion=success
fi
run="{\"id\":7001,\"name\":\"CodeQL\",\"head_sha\":\"$head\",\"url\":\"$url\",\"html_url\":\"https://github.com/orbit/test/runs/7001\",\"status\":\"completed\",\"conclusion\":\"$conclusion\",\"completed_at\":\"2026-10-15T01:39:59Z\",\"check_suite\":{\"id\":800},\"app\":{\"id\":$app,\"slug\":\"github-advanced-security\"},\"output\":{\"title\":\"2 new alerts\",\"annotations_count\":2},\"pull_requests\":[{\"number\":42,\"head\":{\"sha\":\"$link_head\"},\"base\":{\"ref\":\"agent-main\",\"repo\":{\"id\":$repo_id}}}]}"
annotation="{\"path\":\"src/lib.rs\",\"blob_href\":\"https://github.com/orbit/test/blob/$head/src/lib.rs\",\"start_line\":4,\"end_line\":4,\"annotation_level\":\"failure\",\"title\":\"Uncontrolled data used in path expression\",\"message\":\"This path depends on a user-provided value.\"}"
case "$2" in
    'repos/{owner}/{repo}') printf '%s\n' "$repo" ;;
    'repos/{owner}/{repo}/pulls/42')
        printf '{"number":42,"state":"open","head":{"sha":"%s","repo":%s},"base":{"ref":"agent-main","sha":"%s","repo":%s}}\n' "$pr_head" "$head_repo" dddddddddddddddddddddddddddddddddddddddd "$repo" ;;
    "repos/{owner}/{repo}/commits/$head/check-runs?check_name=CodeQL&filter=latest&per_page=100")
        case "$case" in
            duplicate) printf '{"total_count":2,"check_runs":[%s,%s]}\n' "$run" "$(printf '%s' "$run" | sed 's/"id":7001/"id":7003/')" ;;
            truncated_listing) printf '{"total_count":2,"check_runs":[%s]}\n' "$run" ;;
            *) printf '{"total_count":1,"check_runs":[%s]}\n' "$run" ;;
        esac ;;
    'repos/{owner}/{repo}/check-runs/7001/annotations?per_page=100&page=1')
        if [ "$case" = incomplete_annotations ]; then printf '[%s]\n' "$annotation"; exit; fi
        printf '[%s,%s]\n' "$annotation" "$(printf '%s' "$annotation" | sed 's/"start_line":4,"end_line":4/"start_line":9,"end_line":11/')" ;;
    'repos/{owner}/{repo}/check-runs/7001') printf '%s\n' "$run" ;;
    *) echo "unexpected gh $*" >&2; exit 2 ;;
esac
