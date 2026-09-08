#!/usr/bin/env bash
# Regression checks for the manual MCP Registry publication security boundary.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
workflow="$repo_root/.github/workflows/publish-mcp-registry.yml"

require() {
  local description="$1"
  local text="$2"

  if ! grep --fixed-strings --quiet -- "$text" "$workflow"; then
    echo "missing $description" >&2
    exit 1
  fi
}

forbid() {
  local description="$1"
  local text="$2"

  if grep --fixed-strings --quiet -- "$text" "$workflow"; then
    echo "forbidden $description" >&2
    exit 1
  fi
}

require "manual trigger" "workflow_dispatch:"
forbid "automatic push publication" "  push:"
require "strict release tag validation" "tag must be an explicit vMAJOR.MINOR.PATCH release tag"
require "published-release check" "published, non-draft GitHub release"
require "commit-pinned checkout" 'ref: ${{ needs.verify-release.outputs.commit_sha }}'
forbid "unchecked input checkout" 'ref: ${{ inputs.tag }}'
require "exact active-membership endpoint" "/user/memberships/orgs?state=active&per_page=100&page=\$page"
require "administrator-role check" '"$state" != "active" || "$role" != "admin"'
require "pinned publisher version" "releases/download/v1.8.1/mcp-publisher_linux_amd64.tar.gz"
require "publisher checksum" "a06c9096dcb9727c13555b6be26c7effa707b01f06a4c561ba7a3635443cf2cc"
require "official token login" './mcp-publisher login github --token "$MCP_REGISTRY_PAT_TOKEN"'
require "sanitized publisher authentication failure" "MCP Registry authentication failed; verify the dedicated token's active constellation-works admin membership"
require "public record verification" "registry.modelcontextprotocol.io/v0.1/servers/\$server_path/versions/\$VERSION"
require "public registry response handling" ".server.name == \$name"
forbid "secret echo" 'echo "$MCP_REGISTRY_PAT_TOKEN"'

temporary_directory="$(mktemp -d)"
trap 'rm -rf "$temporary_directory"' EXIT

preflight="$temporary_directory/preflight.sh"
awk '
  /      - name: Require an active constellation-works administrator membership/ { in_step = 1 }
  /      - name: Authenticate and publish/ { exit }
  in_step && /^        run: \|$/ { in_run = 1; next }
  in_run { sub(/^          /, ""); print }
' "$workflow" > "$preflight"
chmod 700 "$preflight"

if ! grep --fixed-strings --quiet -- '"$role" != "admin"' "$preflight"; then
  echo "extracted production preflight does not require the GitHub admin role" >&2
  exit 1
fi

preflight_line="$(grep --line-number --fixed-strings -- 'Require an active constellation-works administrator membership' "$workflow" | cut -d: -f1)"
publisher_login_line="$(grep --line-number --fixed-strings -- './mcp-publisher login github --token "$MCP_REGISTRY_PAT_TOKEN"' "$workflow" | cut -d: -f1)"
if [[ "$preflight_line" -ge "$publisher_login_line" ]]; then
  echo "membership preflight must run before publisher login" >&2
  exit 1
fi

fixture_token='fixture-token-must-not-be-logged'
raw_auth_payload='fixture-raw-auth-payload-must-not-be-logged'
mock_bin="$temporary_directory/bin"
mkdir "$mock_bin"

jq -n '[range(0; 100) | {state: "active", role: "member", organization: {login: ("other-" + tostring)}}]' \
  > "$temporary_directory/page-one.json"
jq -n '[{state: "active", role: "admin", organization: {login: "constellation-works"}}]' \
  > "$temporary_directory/page-two-admin.json"
jq -n '[{state: "active", role: "member", organization: {login: "constellation-works"}}]' \
  > "$temporary_directory/member.json"
jq -n '[{state: "active", role: "owner", organization: {login: "constellation-works"}}]' \
  > "$temporary_directory/owner.json"
jq -n '[{state: "pending", role: "admin", organization: {login: "constellation-works"}}]' \
  > "$temporary_directory/pending.json"
jq -n '[{state: "active", organization: {login: "constellation-works"}}]' \
  > "$temporary_directory/malformed.json"
jq -n '[{state: "active", role: "member", organization: {login: "another-org"}}]' \
  > "$temporary_directory/missing.json"

cat > "$mock_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

url="${!#}"
printf '%s\n' "$url" >> "$MCP_TEST_REQUEST_LOG"

has_expected_authorization=false
for argument in "$@"; do
  if [[ "$argument" == "Authorization: Bearer $MCP_REGISTRY_PAT_TOKEN" ]]; then
    has_expected_authorization=true
  fi
done
if [[ "$has_expected_authorization" != true ]]; then
  echo "mock curl did not receive the dedicated authorization header" >&2
  exit 64
fi

if [[ "$MCP_TEST_CASE" == "api-error" ]]; then
  printf '%s\n' "$MCP_TEST_RAW_AUTH_PAYLOAD"
  echo "simulated GitHub API failure" >&2
  exit 22
fi

case "$MCP_TEST_CASE:$url" in
  admin-page-two:*'page=1') cat "$MCP_TEST_FIXTURES/page-one.json" ;;
  admin-page-two:*'page=2') cat "$MCP_TEST_FIXTURES/page-two-admin.json" ;;
  member:*) cat "$MCP_TEST_FIXTURES/member.json" ;;
  owner:*) cat "$MCP_TEST_FIXTURES/owner.json" ;;
  pending:*) cat "$MCP_TEST_FIXTURES/pending.json" ;;
  malformed:*) cat "$MCP_TEST_FIXTURES/malformed.json" ;;
  missing:*) cat "$MCP_TEST_FIXTURES/missing.json" ;;
  *) echo "unexpected mock curl request" >&2; exit 64 ;;
esac
EOF
chmod 700 "$mock_bin/curl"

run_preflight_case() {
  local case_name="$1"
  local expected_status="$2"
  local require_page_two="$3"
  local output="$temporary_directory/$case_name.output"
  local request_log="$temporary_directory/$case_name.requests"
  local status

  if PATH="$mock_bin:$PATH" \
    MCP_REGISTRY_PAT_TOKEN="$fixture_token" \
    MCP_TEST_CASE="$case_name" \
    MCP_TEST_FIXTURES="$temporary_directory" \
    MCP_TEST_REQUEST_LOG="$request_log" \
    MCP_TEST_RAW_AUTH_PAYLOAD="$raw_auth_payload" \
    bash "$preflight" >"$output" 2>&1; then
    status=0
  else
    status=$?
  fi

  if [[ "$expected_status" == success && "$status" -ne 0 ]]; then
    echo "preflight case $case_name unexpectedly failed" >&2
    sed -n '1,80p' "$output" >&2
    exit 1
  fi
  if [[ "$expected_status" == failure && "$status" -eq 0 ]]; then
    echo "preflight case $case_name unexpectedly succeeded" >&2
    exit 1
  fi
  if [[ "$require_page_two" == true ]] && ! grep --fixed-strings --quiet -- 'page=2' "$request_log"; then
    echo "preflight case $case_name did not request the second membership page" >&2
    exit 1
  fi
  if grep --fixed-strings --quiet -- "$fixture_token" "$output" \
    || grep --fixed-strings --quiet -- "$raw_auth_payload" "$output"; then
    echo "preflight case $case_name exposed a sensitive fixture value" >&2
    exit 1
  fi
}

run_preflight_case admin-page-two success true
run_preflight_case member failure false
run_preflight_case owner failure false
run_preflight_case pending failure false
run_preflight_case missing failure false
run_preflight_case malformed failure false
run_preflight_case api-error failure false

echo "MCP Registry publication workflow checks passed"
