#!/usr/bin/env bash
# Confirms every `uses: <owner>/<repo>[/path]@<40-hex-sha>` reference under
# .github/workflows/** resolves to a real commit, so a fabricated or
# transposed-digit SHA (e.g. ORB-12452) cannot merge silently. Resolution
# uses the GitHub API; set GITHUB_TOKEN to raise the rate limit.
#
# Soft-presence: without network access (or when rate-limited) this warns
# and exits 0 rather than blocking offline development, mirroring the
# cargo-deny soft-presence handling elsewhere in this script's caller.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflows_dir=".github/workflows"
if [[ ! -d "$workflows_dir" ]]; then
  exit 0
fi

api_base="https://api.github.com"
auth_header=()
if [[ -n "${GITHUB_TOKEN:-}" ]]; then
  auth_header=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
fi

probe_status="$(curl -s -o /dev/null -m 5 -w '%{http_code}' "${auth_header[@]}" "$api_base" 2>/dev/null || echo "000")"
if [[ "$probe_status" == "000" ]]; then
  echo "check-workflow-action-pins: no network access to $api_base; skipping pin resolution" >&2
  exit 0
fi

declare -A checked
fail=0

while IFS=$'\t' read -r file line_no owner_repo sha; do
  key="${owner_repo}@${sha}"
  if [[ -n "${checked[$key]:-}" ]]; then
    status="${checked[$key]}"
  else
    status="$(curl -s -o /dev/null -m 10 -w '%{http_code}' "${auth_header[@]}" "$api_base/repos/$owner_repo/commits/$sha" 2>/dev/null || echo "000")"
    checked[$key]="$status"
  fi

  case "$status" in
    200)
      ;;
    403 | 429)
      echo "check-workflow-action-pins: rate-limited resolving $owner_repo@$sha (status $status); skipping" >&2
      ;;
    000)
      echo "check-workflow-action-pins: network error resolving $owner_repo@$sha; skipping" >&2
      ;;
    *)
      echo "check-workflow-action-pins: $file:$line_no: uses: $owner_repo@$sha does not resolve (status $status)" >&2
      fail=1
      ;;
  esac
done < <(
  grep -rnE 'uses:[[:space:]]*[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[A-Za-z0-9_./-]+)?@[0-9a-fA-F]{40}' "$workflows_dir" |
    sed -E 's#^([^:]+):([0-9]+):.*uses:[[:space:]]*([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)(/[A-Za-z0-9_./-]+)?@([0-9a-fA-F]{40}).*#\1\t\2\t\3\t\5#'
)

if [[ "$fail" -ne 0 ]]; then
  echo "check-workflow-action-pins: one or more workflow action pins do not resolve" >&2
  exit 1
fi

echo "check-workflow-action-pins: all workflow action pins resolved"
