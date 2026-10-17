#!/bin/sh
# Deterministic authenticated provider for isolated public-boundary regressions.
set -eu
forge='__FORGE__'
remote=$(cat "$forge/remote")
slug=$(cat "$forge/slug")
branch=$(cat "$forge/pr-head")
base=$(cat "$forge/pr-base")
export GIT_AUTHOR_NAME=Forge GIT_AUTHOR_EMAIL=forge@example.invalid
export GIT_COMMITTER_NAME=Forge GIT_COMMITTER_EMAIL=forge@example.invalid
printf '%s\n' "$*" >> "$forge/calls"
head=$(git --git-dir="$remote" rev-parse "refs/heads/$branch")
base_sha=$(git --git-dir="$remote" rev-parse "refs/heads/$base")
repo="{\"id\":500,\"full_name\":\"$slug\",\"url\":\"https://api.github.com/repos/$slug\"}"
rollup='{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS","isRequired":false}'
checks="{\"id\":1,\"name\":\"test\",\"head_sha\":\"$head\",\"app\":{\"id\":900},\"check_suite\":{\"id\":800},\"status\":\"completed\",\"conclusion\":\"success\"}"
state=OPEN
merge_state=BLOCKED
merged_at=null
merge_commit=null
if [ -f "$forge/base-applied" ]; then
    merge_state=CLEAN
    rollup="$rollup,"'{"__typename":"CheckRun","name":"Protected (rust)","status":"COMPLETED","conclusion":"SUCCESS","isRequired":true}'
    checks="$checks,{\"id\":2,\"name\":\"Protected (rust)\",\"head_sha\":\"$head\",\"app\":{\"id\":900},\"check_suite\":{\"id\":800},\"status\":\"completed\",\"conclusion\":\"success\"}"
fi
if [ -f "$forge/gates-pending" ] && [ -f "$forge/base-applied" ]; then
    merge_state=BLOCKED
    rollup=$(printf '%s' "$rollup" | sed 's/"Protected (rust)","status":"COMPLETED","conclusion":"SUCCESS"/"Protected (rust)","status":"IN_PROGRESS","conclusion":""/')
    checks=$(printf '%s' "$checks" | sed 's/"name":"Protected (rust)"/"name":"Not emitted"/')
fi
if [ -f "$forge/clean-missing" ] && [ -f "$forge/base-applied" ]; then
    checks=$(printf '%s' "$checks" | sed 's/"name":"Protected (rust)"/"name":"Not emitted"/')
fi
if [ -f "$forge/legacy-state" ]; then
    rollup="$rollup,"'{"__typename":"StatusContext","context":"external-ci","state":"SUCCESS","isRequired":true}'
fi
if [ -f "$forge/merged" ]; then
    state=MERGED; merge_state=UNKNOWN
    merged_at='"2026-10-15T08:00:00Z"'
    merge_commit="{\"oid\":\"$(cat "$forge/merged")\"}"
fi
review=''
[ ! -f "$forge/review" ] || review=REVIEW_REQUIRED
if [ -f "$forge/auth" ] && [ "$1" = api ]; then echo 'HTTP 403 forbidden' >&2; exit 1; fi
[ ! -f "$forge/review-required" ] || review=REVIEW_REQUIRED
case "$1 ${2:-}" in
    'pr view')
        printf '{"number":42,"state":"%s","mergedAt":%s,"mergeable":"MERGEABLE","mergeStateStatus":"%s","reviewDecision":"%s","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":%s,"url":"https://github.com/%s/pull/42"}\n' "$state" "$merged_at" "$merge_state" "$review" "$rollup" "$branch" "$head" "$base" "$merge_commit" "$slug" ;;
    'repo view') printf '{"nameWithOwner":"%s"}\n' "$slug" ;;
    'api graphql')
        if [ -f "$forge/policy-unreadable" ]; then echo '{"errors":[{"message":"unavailable"}]}'; exit; fi
        case "$*" in
            *requiredStatusChecks*)
                if [ -f "$forge/policy-no-rule" ]; then printf '{"data":{"repository":{"pullRequest":{"headRefOid":"%s","baseRefName":"%s","reviewDecision":null,"baseRef":{"branchProtectionRule":null}}}}}\n' "$head" "$base"; exit; fi
                printf '{"data":{"repository":{"pullRequest":{"headRefOid":"%s","baseRefName":"%s","reviewDecision":null,"baseRef":{"branchProtectionRule":{"id":"RULE-1","requiredStatusChecks":[]}}}}}}\n' "$head" "$base" ;;
            *statusCheckRollup*)
                printf '{"data":{"repository":{"pullRequest":{"headRefOid":"%s","commits":{"nodes":[{"commit":{"oid":"%s","statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[%s]}}}}]}}}}}\n' "$head" "$head" "$rollup" ;;
            *merge*|*Merge*) echo '{"data":{"repository":{"nameWithOwner":"'"$slug"'","mergeCommitAllowed":true,"squashMergeAllowed":true,"rebaseMergeAllowed":false,"autoMergeAllowed":false,"pullRequest":{"baseRefName":"'"$base"'","baseRef":{"branchProtectionRule":{"requiresLinearHistory":false}}}}}}' ;;
            *) echo "unknown graphql $*" >&2; exit 2 ;;
        esac ;;
    'api repos/{owner}/{repo}')
        # One controlled read delay, never synthetic host load. The former
        # one-second allowance timed out here before reservation or mutation.
        if [ -f "$forge/delay-read" ] && [ ! -f "$forge/read-delay-entered" ]; then
            touch "$forge/read-delay-entered"
            sleep 2
            touch "$forge/read-delay-finished"
        fi
        printf '%s\n' "$repo" ;;
    'api repos/{owner}/{repo}/pulls/42')
        if [ -f "$forge/changed_head" ]; then head=$(printf '%040d' 1); fi
        printf '{"number":42,"state":"open","merged":false,"head":{"sha":"%s","ref":"%s","repo":%s},"base":{"sha":"%s","ref":"%s","repo":%s}}\n' "$head" "$branch" "$repo" "$base_sha" "$base" "$repo" ;;
    'api repos/{owner}/{repo}/actions/workflows?per_page=100')
        if [ -f "$forge/truncated" ]; then echo '{"total_count":2,"workflows":[]}'; exit; fi
        echo '{"total_count":1,"workflows":[{"id":600,"path":".github/workflows/ci.yml","state":"active"}]}' ;;
    'api apps/github-actions') echo '{"id":900,"slug":"github-actions"}' ;;
    'api repos/{owner}/{repo}/pulls/42/update-branch')
        if [ ! -f "$forge/base-reserved" ]; then
            # Real owner fixtures require the journal's persisted event, not a
            # test-host marker. This reads state the public operation wrote.
            [ -f "$forge/owner-events" ] && grep -q '"type":"pr_base_update_reserved"' "$(cat "$forge/owner-events")" || { echo 'no durable reservation' >&2; exit 1; }
        fi
        case "$*" in *"expected_head_sha=$head") ;; *) echo 'head lease' >&2; exit 1 ;; esac
        printf 'update\n' >> "$forge/mutations"
        [ ! -f "$forge/no-apply" ] || exit 1
        tree=$(git --git-dir="$remote" merge-tree --write-tree "$head" "$base_sha")
        updated=$(git --git-dir="$remote" commit-tree "$tree" -p "$head" -p "$base_sha" -m 'Provider base update')
        git --git-dir="$remote" update-ref "refs/heads/$branch" "$updated" "$head"
        touch "$forge/base-applied"
        if [ -f "$forge/foreign-after" ]; then
            foreign=$(git --git-dir="$remote" commit-tree "$tree" -p "$updated" -m 'Foreign source head')
            git --git-dir="$remote" update-ref "refs/heads/$branch" "$foreign" "$updated"
        fi
        [ ! -f "$forge/lost-reply" ] || exit 1
        echo '{}' ;;
    'api repos/{owner}/{repo}/pulls/42/merge')
        [ -f "$forge/base-applied" ] && [ ! -f "$forge/gates-pending" ] || { echo 'merge before checks' >&2; exit 1; }
        case "$*" in *"sha=$head"*) ;; *) echo 'merge lease' >&2; exit 1 ;; esac
        printf 'merge\n' >> "$forge/mutations"
        tree=$(git --git-dir="$remote" rev-parse "$head^{tree}")
        landed=$(git --git-dir="$remote" commit-tree "$tree" -p "$base_sha" -m 'Squash PR 42')
        git --git-dir="$remote" update-ref "refs/heads/$base" "$landed" "$base_sha"
        printf '%s\n' "$landed" > "$forge/merged"
        printf '{"sha":"%s","merged":true,"message":"merged"}\n' "$landed" ;;
    'api '*)
        case "$2" in
            *'/rules/branches/'*)
                if [ -f "$forge/policy-empty" ] || [ -f "$forge/policy-no-rule" ]; then echo '[]'; exit; fi
                if [ -f "$forge/legacy-extra" ]; then
                    echo '[{"type":"required_status_checks","ruleset_id":55,"ruleset_source":"orbit/test","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"Protected (rust)","integration_id":900},{"context":"external-ci","integration_id":null}]}}]'; exit
                fi
                if [ -f "$forge/legacy-substitute" ]; then
                    echo '[{"type":"required_status_checks","ruleset_id":55,"ruleset_source":"orbit/test","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"Protected (rust)","integration_id":null}]}}]'; exit
                fi
                if [ -f "$forge/legacy-state" ]; then
                    integration=null
                    [ ! -f "$forge/legacy-app-bound" ] || integration=900
                    printf '[{"type":"required_status_checks","ruleset_id":55,"ruleset_source":"%s","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"external-ci","integration_id":%s}]}}]\n' "$slug" "$integration"; exit
                fi
                if [ -f "$forge/policy-after" ] && [ -f "$forge/base-applied" ]; then echo '[{"type":"required_status_checks","ruleset_id":56,"ruleset_source":"orbit/test","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"Protected (rust)","integration_id":900}]}}]'; exit; fi
                if [ -f "$forge/foreign_app" ]; then echo '[{"type":"required_status_checks","ruleset_id":55,"ruleset_source":"orbit/test","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"Protected (rust)","integration_id":901}]}}]'; exit; fi
                # Policy changes once the update is durably reserved, however
                # many current-gates reads preceded the emission evidence.
                if [ -f "$forge/changed_policy" ] && [ -f "$forge/base-reserved" ]; then
                    echo '[{"type":"required_status_checks","ruleset_id":56,"ruleset_source":"orbit/test","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"Protected (rust)","integration_id":900}]}}]'; exit
                fi
                echo '[{"type":"required_status_checks","ruleset_id":55,"ruleset_source":"'"$slug"'","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"Protected (rust)","integration_id":900}]}}]' ;;
            *'/check-runs?'*)
                if [ -f "$forge/unreadable-after" ] && [ -f "$forge/base-applied" ]; then echo '{"total_count":3,"check_runs":[]}'; exit; fi
                if [ -f "$forge/legacy-shadow-pending" ]; then checks=$(printf '%s' "$checks" | sed 's/"name":"Protected (rust)"/"name":"external-ci"/;s/"status":"completed"/"status":"in_progress"/g'); fi
                if [ -f "$forge/legacy-substitute" ] && [ -f "$forge/base-applied" ]; then checks=$(printf '%s' "$checks" | sed 's/"name":"Protected (rust)"/"name":"Not emitted"/'); fi
                printf '{"total_count":%s,"check_runs":[%s]}\n' "$([ -f "$forge/base-applied" ] && echo 2 || echo 1)" "$checks" ;;
            *'/status?'*)
                status_head=$head; status_repo=$repo; statuses=''; count=0
                if [ -f "$forge/legacy-state" ]; then
                    status_state=$(cat "$forge/legacy-state")
                    row_head=$head; row_slug=$slug; row_host=https://api.github.com
                    [ ! -f "$forge/legacy-wrong-head" ] || status_head=$(printf '%040d' 1)
                    [ ! -f "$forge/legacy-foreign" ] || status_repo='{"id":501,"full_name":"foreign/repo"}'
                    [ ! -f "$forge/legacy-wrong-row" ] || row_head=$(printf '%040d' 1)
                    [ ! -f "$forge/legacy-foreign-row" ] || row_slug=foreign/repo
                    [ ! -f "$forge/legacy-foreign-host" ] || row_host=https://foreign.invalid
                    statuses="{\"id\":10,\"context\":\"external-ci\",\"state\":\"$status_state\",\"url\":\"$row_host/repos/$row_slug/statuses/$row_head\"}"
                    count=1
                fi
                if [ -f "$forge/legacy-substitute" ] && [ -f "$forge/base-applied" ]; then
                    statuses="{\"id\":10,\"context\":\"Protected (rust)\",\"state\":\"success\",\"url\":\"https://api.github.com/repos/$slug/statuses/$head\"}"
                    count=1
                fi
                printf '{"sha":"%s","repository":%s,"total_count":%s,"statuses":[%s]}\n' "$status_head" "$status_repo" "$count" "$statuses" ;;

            *'/actions/runs?'*|*'/actions/runs/700')
                if [ -f "$forge/no-runs" ]; then echo '{"total_count":0,"workflow_runs":[]}'; exit; fi
                old_base=$(cat "$forge/old-base")
                run="{\"id\":700,\"run_attempt\":1,\"workflow_id\":600,\"check_suite_id\":800,\"path\":\".github/workflows/ci.yml\",\"head_sha\":\"$head\",\"head_branch\":\"$branch\",\"repository\":$repo,\"head_repository\":$repo,\"event\":\"pull_request\",\"actor\":{\"id\":123,\"login\":\"owner\"},\"status\":\"completed\",\"conclusion\":\"success\",\"pull_requests\":[{\"number\":42,\"head\":{\"sha\":\"$head\"},\"base\":{\"sha\":\"$old_base\",\"ref\":\"$base\",\"repo\":$repo}}]}"
                if [ -f "$forge/push" ]; then run=$(printf '%s' "$run" | sed 's/"event":"pull_request"/"event":"push"/'); fi
                case "$2" in *'/700') printf '%s\n' "$run" ;; *) printf '{"total_count":1,"workflow_runs":[%s]}\n' "$run" ;; esac ;;
            *'/actions/workflows/600') echo '{"id":600,"path":".github/workflows/ci.yml"}' ;;
            *'/check-suites/800') printf '{"id":800,"head_sha":"%s","repository":%s,"app":{"id":900,"slug":"github-actions"}}\n' "$head" "$repo" ;;
            *'/rulesets/'*)
                id=${2##*/}
                if [ -f "$forge/revision-after" ] && [ -f "$forge/base-applied" ]; then printf '{"id":%s,"updated_at":"2026-10-15T01:00:00Z","enforcement":"active"}\n' "$id"; exit; fi
                printf '{"id":%s,"updated_at":"2026-10-15T00:00:00Z","enforcement":"active"}\n' "$id" ;;

            *'/contents/'*)
                path=${2#*'/contents/'}; ref=${path#*'?ref='}; path=${path%'?ref='*}
                blob=$(git --git-dir="$remote" rev-parse "$ref:$path")
                size=$(git --git-dir="$remote" cat-file -s "$blob")
                [ ! -f "$forge/source-truncated" ] || size=$((size+1))
                printf '{"type":"file","path":"%s","sha":"%s","size":%s}\n' "$path" "$blob" "$size" ;;
            *) echo "unknown api $*" >&2; exit 2 ;;
        esac ;;
    *) echo "unknown provider command $*" >&2; exit 2 ;;
esac
