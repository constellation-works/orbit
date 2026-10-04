#!/usr/bin/env bash
# Confirms every `uses: <owner>/<repo>[/path]@<40-hex-sha>` reference under
# .github/workflows/** resolves to a real commit, so a fabricated or
# transposed-digit SHA (e.g. ORB-12452) cannot merge silently. Resolution
# uses the GitHub API; set GITHUB_TOKEN to raise the rate limit.
#
# Soft-presence: transient API/network failures warn and exit 0 by default.
# Set ORBIT_STRICT_WORKFLOW_ACTION_PINS=1 to require every pin to resolve.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflows_dir=".github/workflows"
if [[ ! -d "$workflows_dir" ]]; then
  exit 0
fi

api_base="https://api.github.com"
max_attempts=3
# Expanded as ${auth_header[@]+"${auth_header[@]}"}: bash 3.2 (macOS /bin/bash)
# treats an empty array as unset under `set -u`.
auth_header=()
if [[ -n "${GITHUB_TOKEN:-}" ]]; then
  auth_header=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
fi

strict_mode="${ORBIT_STRICT_WORKFLOW_ACTION_PINS:-0}"

resolve_status() {
  local url="$1" attempt status
  for attempt in 1 2 3; do
    # Keep curl's emitted status even when it exits nonzero. Appending a
    # fallback with `|| echo 000` turns curl's emitted 000 into 000000.
    status="$(curl -s -o /dev/null -m 10 -w '%{http_code}' ${auth_header[@]+"${auth_header[@]}"} "$url" 2>/dev/null || true)"
    if [[ ! "$status" =~ ^[0-9]{3}$ ]]; then
      status="000"
    fi

    case "$status" in
      200 | 404 | 422)
        printf '%s' "$status"
        return
        ;;
    esac

    if [[ "$attempt" -lt "$max_attempts" ]]; then
      case "$attempt" in
        1) sleep 0.25 ;;
        2) sleep 0.5 ;;
      esac
    fi
  done

  printf '%s' "$status"
}

fail=0
skipped=0

# Each distinct owner/repo@sha is resolved once. Deduplication happens in the
# producer (sort on the first two fields) rather than an associative array so
# the script runs under bash 3.2 (macOS /bin/bash), which the guard self-tests
# use; only the first workflow location of a repeated pin is reported.
while IFS=$'\t' read -r owner_repo sha file line_no; do
  status="$(resolve_status "$api_base/repos/$owner_repo/commits/$sha")"

  case "$status" in
    200)
      ;;
    404 | 422)
      echo "check-workflow-action-pins: $file:$line_no: uses: $owner_repo@$sha does not resolve (status $status)" >&2
      fail=1
      ;;
    *)
      if [[ "$strict_mode" == "1" ]]; then
        echo "check-workflow-action-pins: $file:$line_no: inconclusive resolving $owner_repo@$sha (status $status after $max_attempts attempts); strict mode requires resolution" >&2
        fail=1
      else
        echo "check-workflow-action-pins: inconclusive resolving $owner_repo@$sha (status $status after $max_attempts attempts); skipping" >&2
        skipped=1
      fi
      ;;
  esac
done < <(
  grep -rnE 'uses:[[:space:]]*[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[A-Za-z0-9_./-]+)?@[0-9a-fA-F]{40}' "$workflows_dir" |
    sed -E 's#^([^:]+):([0-9]+):.*uses:[[:space:]]*([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)(/[A-Za-z0-9_./-]+)?@([0-9a-fA-F]{40}).*#\3\t\5\t\1\t\2#' |
    sort -t "$(printf '\t')" -k1,2 -u
)

if [[ "$fail" -ne 0 ]]; then
  echo "check-workflow-action-pins: one or more workflow action pins failed the resolution guard" >&2
  exit 1
fi

if [[ "$skipped" -ne 0 ]]; then
  echo "check-workflow-action-pins: pin resolution completed with inconclusive results skipped" >&2
else
  echo "check-workflow-action-pins: all workflow action pins resolved"
fi
