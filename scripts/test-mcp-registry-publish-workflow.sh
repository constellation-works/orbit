#!/usr/bin/env bash
# Behavioral checks for the manual MCP Registry publication boundary.
#
# Each workflow `run` step is extracted and executed against fixture commands.
# A check that only compares Actions structure names the invariant it protects:
# manual dispatch, checkout of the verified commit, preflight order, and no
# echo of a publication token. Those checks do not stand in for the tag,
# draft, checksum, login, or public-record behavior executed below.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
workflow="$repo_root/.github/workflows/publish-mcp-registry.yml"

# Reviewed publisher artifact. The download step must request this URL and pass
# this digest to sha256sum --check. A substituted archive must be rejected.
pinned_publisher_url="https://github.com/modelcontextprotocol/registry/releases/download/v1.8.1/mcp-publisher_linux_amd64.tar.gz"
pinned_publisher_sha256="a06c9096dcb9727c13555b6be26c7effa707b01f06a4c561ba7a3635443cf2cc"

release_commit_sha="0123456789abcdef0123456789abcdef01234567"
annotated_object_sha="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
annotated_commit_sha="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
github_api_url="https://github.example.test/api"
github_repository="constellation-works/orbit"
github_token="fixture-github-token-must-not-be-logged"
registry_token="fixture-registry-token-must-not-be-logged"
raw_publisher_output="fixture-raw-publisher-output-must-not-be-logged"
raw_auth_payload="fixture-raw-auth-payload-must-not-be-logged"
release_step="Resolve the published immutable release tag"
contract_step="Verify the registry and npm publication contract"
download_step="Download and verify the pinned MCP publisher"
validate_step="Validate registry metadata"
preflight_step="Require an active constellation-works administrator membership"
publish_step="Authenticate and publish"
record_step="Verify the public registry record"

system_path="/usr/bin:/bin"
jq_dir="$(dirname "$(command -v jq)")"
case ":$system_path:" in
  *":$jq_dir:"*) ;;
  *) system_path="$jq_dir:$system_path" ;;
esac
real_sha256sum=""
real_shasum=""
if command -v sha256sum >/dev/null 2>&1; then
  real_sha256sum="$(command -v sha256sum)"
fi
if command -v shasum >/dev/null 2>&1; then
  real_shasum="$(command -v shasum)"
fi
if [[ -z "$real_sha256sum" && -z "$real_shasum" ]]; then
  echo "checksum guard requires sha256sum or shasum" >&2
  exit 1
fi

temporary_directory="$(mktemp -d)"
trap 'rm -rf "$temporary_directory"' EXIT
mkdir -p "$temporary_directory/steps"
step_index=0
mutation_index=0

extract_step() {
  local step_name="$1"
  local destination="$2"
  python3 - "$workflow" "$step_name" "$destination" <<'PY'
import sys
from pathlib import Path
try:
    import yaml
except ImportError:
    sys.stderr.write("PyYAML is required to execute workflow steps\n")
    sys.exit(2)
workflow, step_name, destination = sys.argv[1:]
document = yaml.safe_load(Path(workflow).read_text())
matches = []
for job in document["jobs"].values():
    for step in job.get("steps") or []:
        if step.get("name") == step_name and "run" in step:
            matches.append(step["run"])
if len(matches) != 1:
    sys.stderr.write("expected one run step named %r, found %d\n" % (step_name, len(matches)))
    sys.exit(1)
text = matches[0]
if not text.endswith("\n"):
    text += "\n"
Path(destination).write_text(text)
PY
}

run_extracted() {
  local step_name="$1"
  local workdir="$2"
  local output_file="$3"
  shift 3
  step_index=$((step_index + 1))
  local script="$temporary_directory/steps/step-$step_index.sh"
  extract_step "$step_name" "$script" || return 2
  local status=0
  (
    # shellcheck disable=SC2164
    cd "$workdir"
    env -i LANG=C LC_ALL=C HOME="$temporary_directory" TMPDIR="$temporary_directory" \
      "$@" bash "$script"
  ) >"$output_file" 2>&1 || status=$?
  printf '%s' "$status"
}

hide_secrets() {
  local output_file="$1"
  shift
  local secret
  for secret in "$@"; do
    if grep -F -q -- "$secret" "$output_file"; then
      echo "property failed: step output exposed $secret" >&2
      return 1
    fi
  done
}

require_status() {
  local actual="$1"
  local expected="$2"
  local description="$3"
  local output_file="$4"
  if [[ ! "$actual" =~ ^[0-9]+$ ]]; then
    echo "harness failed to execute $description" >&2
    return 2
  fi
  if [[ "$expected" == "0" && "$actual" -ne 0 ]]; then
    echo "property failed: $description unexpectedly failed" >&2
    sed -n '1,80p' "$output_file" >&2
    return 1
  fi
  if [[ "$expected" == "nonzero" && "$actual" -eq 0 ]]; then
    echo "property failed: $description unexpectedly succeeded" >&2
    sed -n '1,40p' "$output_file" >&2
    return 1
  fi
}

write_release_fixtures() {
  local tag_name="$1"
  local draft="$2"
  local object_type="$3"
  local object_sha="$4"
  jq -n --arg tag "$tag_name" --argjson draft "$draft" \
    '{tag_name: $tag, draft: $draft}' >"$case_dir/fixtures/release.json"
  jq -n --arg type "$object_type" --arg sha "$object_sha" \
    '{object: {type: $type, sha: $sha}}' >"$case_dir/fixtures/ref.json"
  rm -f "$case_dir/fixtures/annotated.json"
}

install_release_curl() {
  cat >"$case_dir/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
has_fail=false
has_auth=false
for argument in "$@"; do
  case "$argument" in
    https://*) url="$argument" ;;
  esac
  if [[ "$argument" == "--fail" ]]; then
    has_fail=true
  fi
  if [[ "$argument" == "Authorization: Bearer $GITHUB_TOKEN" ]]; then
    has_auth=true
  fi
done
printf '%s\n' "$url" >>"$MCP_TEST_REQUEST_LOG"
if [[ "$has_fail" != true || "$has_auth" != true || -z "$url" ]]; then
  echo "release lookup must use curl --fail and the Actions bearer token" >&2
  exit 64
fi
case "$url" in
  "$GITHUB_API_URL/repos/$GITHUB_REPOSITORY/releases/tags/"*)
    cat "$MCP_TEST_FIXTURES/release.json" ;;
  "$GITHUB_API_URL/repos/$GITHUB_REPOSITORY/git/ref/tags/"*)
    cat "$MCP_TEST_FIXTURES/ref.json" ;;
  "$GITHUB_API_URL/repos/$GITHUB_REPOSITORY/git/tags/"*)
    if [[ ! -f "$MCP_TEST_FIXTURES/annotated.json" ]]; then
      echo "annotated tag lookup was not expected" >&2
      exit 64
    fi
    cat "$MCP_TEST_FIXTURES/annotated.json" ;;
  *)
    echo "unexpected release lookup URL: $url" >&2
    exit 64 ;;
esac
EOF
  chmod 700 "$case_dir/bin/curl"
}

begin_release_case() {
  local case_name="$1"
  case_dir="$temporary_directory/release-$case_name"
  rm -rf "$case_dir"
  mkdir -p "$case_dir/fixtures" "$case_dir/bin" "$case_dir/work"
  case_output="$case_dir/output"
  case_requests="$case_dir/requests"
  case_github_output="$case_dir/github-output"
  : >"$case_requests"
  : >"$case_github_output"
  install_release_curl
}

run_release_case() {
  local tag="$1"
  release_status="$(run_extracted "$release_step" "$case_dir/work" "$case_output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="$tag" \
    GITHUB_API_URL="$github_api_url" \
    GITHUB_REPOSITORY="$github_repository" \
    GITHUB_TOKEN="$github_token" \
    GITHUB_OUTPUT="$case_github_output" \
    MCP_TEST_FIXTURES="$case_dir/fixtures" \
    MCP_TEST_REQUEST_LOG="$case_requests")" || true
}

assert_release_tag_rejects_untrusted_input() {
  local tag
  for tag in "latest" "v1.2" "v1.2.3;id" "V1.2.3" "v1.2.3 "; do
    begin_release_case "hostile-${tag//[^A-Za-z0-9]/_}"
    write_release_fixtures "$tag" false commit "$release_commit_sha"
    run_release_case "$tag"
    require_status "$release_status" nonzero "untrusted release tag $tag" "$case_output" || return
    if [[ -s "$case_requests" ]]; then
      echo "property failed: untrusted release tag $tag reached the GitHub API" >&2
      return 1
    fi
    if grep -F -q -- "commit_sha=" "$case_github_output"; then
      echo "property failed: untrusted release tag $tag resolved to a commit" >&2
      return 1
    fi
    hide_secrets "$case_output" "$github_token" || return
  done
}

assert_published_release_resolves_commit() {
  local tag
  for tag in "v1.2.3" "v1.2.3-rc.1"; do
    begin_release_case "published-${tag//[^A-Za-z0-9]/_}"
    write_release_fixtures "$tag" false commit "$release_commit_sha"
    run_release_case "$tag"
    require_status "$release_status" 0 "published release $tag" "$case_output" || return
    if [[ "$(cat "$case_github_output")" != "commit_sha=$release_commit_sha" ]]; then
      echo "property failed: published release $tag did not record the resolved commit" >&2
      cat "$case_github_output" >&2
      return 1
    fi
    if ! grep -F -x -q -- \
      "$github_api_url/repos/$github_repository/releases/tags/$tag" "$case_requests"; then
      echo "property failed: published release $tag did not query its release record" >&2
      return 1
    fi
    hide_secrets "$case_output" "$github_token" || return
  done
}

assert_draft_release_rejected() {
  begin_release_case draft
  write_release_fixtures "v1.2.3" true commit "$release_commit_sha"
  run_release_case "v1.2.3"
  require_status "$release_status" nonzero "draft release" "$case_output" || return
  if grep -F -q -- "commit_sha=" "$case_github_output"; then
    echo "property failed: a draft release resolved to a commit" >&2
    return 1
  fi
}

assert_release_tag_name_mismatch_rejected() {
  begin_release_case mismatch
  write_release_fixtures "v9.9.9" false commit "$release_commit_sha"
  run_release_case "v1.2.3"
  require_status "$release_status" nonzero "release whose tag_name differs" "$case_output" || return
  if grep -F -q -- "commit_sha=" "$case_github_output"; then
    echo "property failed: a mismatched release tag_name resolved to a commit" >&2
    return 1
  fi
}

assert_annotated_tag_resolves_to_commit() {
  begin_release_case annotated
  write_release_fixtures "v1.2.3" false tag "$annotated_object_sha"
  jq -n --arg sha "$annotated_commit_sha" \
    '{object: {type: "commit", sha: $sha}}' >"$case_dir/fixtures/annotated.json"
  run_release_case "v1.2.3"
  require_status "$release_status" 0 "annotated release tag" "$case_output" || return
  if [[ "$(cat "$case_github_output")" != "commit_sha=$annotated_commit_sha" ]]; then
    echo "property failed: annotated tag did not resolve to its commit" >&2
    cat "$case_github_output" >&2
    return 1
  fi
  if ! grep -F -x -q -- \
    "$github_api_url/repos/$github_repository/git/tags/$annotated_object_sha" "$case_requests"; then
    echo "property failed: annotated tag was not dereferenced" >&2
    return 1
  fi
}

assert_unresolved_objects_rejected() {
  begin_release_case blob
  write_release_fixtures "v1.2.3" false blob "$release_commit_sha"
  run_release_case "v1.2.3"
  require_status "$release_status" nonzero "release object that is not a commit" "$case_output" || return
  if grep -F -q -- "commit_sha=" "$case_github_output"; then
    echo "property failed: a non-commit release object resolved" >&2
    return 1
  fi

  begin_release_case short-sha
  write_release_fixtures "v1.2.3" false commit "abcd"
  run_release_case "v1.2.3"
  require_status "$release_status" nonzero "release object with a short sha" "$case_output" || return
  if grep -F -q -- "commit_sha=" "$case_github_output"; then
    echo "property failed: a short release sha resolved" >&2
    return 1
  fi
}

assert_checksum_rejects_substituted_archive() {
  local case_dir="$temporary_directory/checksum-$mutation_index"
  local payload="$case_dir/payload"
  local archive="$case_dir/substituted.tar.gz"
  local output="$case_dir/output"
  local curl_log="$case_dir/curl.log"
  local sha_log="$case_dir/sha.log"
  local sha_stdin="$case_dir/sha.stdin"
  mkdir -p "$payload" "$case_dir/work" "$case_dir/bin"
  printf '#!/bin/sh\necho substituted-publisher\n' >"$payload/mcp-publisher"
  chmod 755 "$payload/mcp-publisher"
  tar -C "$payload" -czf "$archive" mcp-publisher
  : >"$curl_log"
  : >"$sha_log"
  cat >"$case_dir/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=""
previous=""
for argument in "$@"; do
  printf '%s\n' "$argument" >>"$MCP_TEST_CURL_LOG"
  if [[ "$previous" == "--output" ]]; then
    output="$argument"
  fi
  previous="$argument"
done
if [[ -z "$output" ]]; then
  echo "publisher download did not pass --output" >&2
  exit 64
fi
cp "$MCP_TEST_ARCHIVE" "$output"
EOF
  cat >"$case_dir/bin/sha256sum" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MCP_TEST_SHA256_LOG"
if [[ "${1:-}" == "--check" ]]; then
  cat >"$MCP_TEST_SHA256_STDIN"
  if [[ -n "${MCP_TEST_REAL_SHA256SUM:-}" ]]; then
    exec "$MCP_TEST_REAL_SHA256SUM" --check "${@:2}" <"$MCP_TEST_SHA256_STDIN"
  fi
  status=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ -z "$line" ]] && continue
    digest="${line%% *}"
    file="${line#* }"
    file="${file# }"
    actual="$("$MCP_TEST_REAL_SHASUM" -a 256 "$file" | awk '{print $1}')"
    if [[ "$actual" != "$digest" ]]; then
      status=1
    fi
  done <"$MCP_TEST_SHA256_STDIN"
  exit "$status"
fi
if [[ -n "${MCP_TEST_REAL_SHA256SUM:-}" ]]; then
  exec "$MCP_TEST_REAL_SHA256SUM" "$@"
fi
echo "sha256sum was invoked without --check" >&2
exit 127
EOF
  chmod 700 "$case_dir/bin/curl" "$case_dir/bin/sha256sum"
  local status
  status="$(run_extracted "$download_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    MCP_TEST_ARCHIVE="$archive" \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_SHA256_LOG="$sha_log" \
    MCP_TEST_SHA256_STDIN="$sha_stdin" \
    MCP_TEST_REAL_SHA256SUM="$real_sha256sum" \
    MCP_TEST_REAL_SHASUM="$real_shasum")" || true
  local invocation=""
  if [[ -s "$sha_log" ]]; then
    invocation="$(sed -n '1p' "$sha_log")"
  fi
  case "$invocation" in
    --check*) ;;
    *)
      echo "property failed: checksum verification did not invoke sha256sum --check" >&2
      return 1
      ;;
  esac
  local presented=""
  if [[ -s "$sha_stdin" ]]; then
    presented="$(awk 'NR==1 { print $1 }' "$sha_stdin")"
  fi
  if [[ "$presented" != "$pinned_publisher_sha256" ]]; then
    echo "property failed: sha256sum --check received ${presented:-no digest}, not the reviewed v1.8.1 publisher digest" >&2
    return 1
  fi
  require_status "$status" nonzero "substituted publisher archive" "$output" || return
  if [[ -e "$case_dir/work/mcp-publisher" ]]; then
    echo "property failed: substituted publisher archive was extracted" >&2
    return 1
  fi
  if ! grep -F -x -q -- "$pinned_publisher_url" "$curl_log"; then
    echo "property failed: publisher download did not request the reviewed v1.8.1 archive" >&2
    return 1
  fi
  local flag
  for flag in "--fail" "--proto" "=https" "--tlsv1.2"; do
    if ! grep -F -x -q -- "$flag" "$curl_log"; then
      echo "property failed: publisher download dropped $flag; the archive fetch must stay on HTTPS with TLS 1.2 and curl --fail" >&2
      return 1
    fi
  done
}

write_contract_server() {
  local version="$1"
  local operator_note="$2"
  local package_count="$3"
  jq -n \
    --arg version "$version" \
    --arg operator_note "$operator_note" \
    --argjson package_count "$package_count" \
    '{
      name: "io.github.constellation-works/orbit",
      version: $version,
      note: $operator_note,
      packages: [range($package_count) | {
        registryType: "npm",
        identifier: "@orbit-tools/cli",
        version: $version,
        transport: {type: "stdio"},
        packageArguments: [
          {type: "positional", value: "mcp"},
          {type: "positional", value: "serve"}
        ]
      }]
    }' >"$case_dir/work/server.json"
}

assert_registry_contract() {
  local case_dir="$temporary_directory/contract"
  mkdir -p "$case_dir/work" "$case_dir/bin"
  local output="$case_dir/output"
  local curl_log="$case_dir/curl.log"
  local npm_body="$case_dir/npm.json"
  : >"$curl_log"
  cat >"$case_dir/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
has_fail=false
for argument in "$@"; do
  printf '%s\n' "$argument" >>"$MCP_TEST_CURL_LOG"
  case "$argument" in
    https://*) url="$argument" ;;
  esac
  if [[ "$argument" == "--fail" ]]; then
    has_fail=true
  fi
done
if [[ "$has_fail" != true || -z "$url" ]]; then
  echo "npm metadata lookup must use curl --fail" >&2
  exit 64
fi
if [[ "$MCP_TEST_CASE" == "npm-error" ]]; then
  echo "simulated npm failure" >&2
  exit 22
fi
cat "$MCP_TEST_NPM_BODY"
EOF
  chmod 700 "$case_dir/bin/curl"
  local version="1.2.3"
  local npm_url="https://registry.npmjs.org/%40orbit-tools%2Fcli/$version"
  write_contract_server "$version" "" 1
  jq -n --arg version "$version" \
    '{version: $version, mcpName: "io.github.constellation-works/orbit"}' >"$npm_body"
  local status
  status="$(run_extracted "$contract_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v$version" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_NPM_BODY="$npm_body")" || true
  require_status "$status" 0 "matching server.json and npm metadata" "$output" || return
  if ! grep -F -x -q -- "$npm_url" "$curl_log"; then
    echo "property failed: contract check did not read the published npm package" >&2
    return 1
  fi

  write_contract_server "$version" "--operator" 1
  status="$(run_extracted "$contract_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v$version" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_NPM_BODY="$npm_body")" || true
  require_status "$status" nonzero "server.json that carries --operator" "$output" || return

  write_contract_server "$version" "" 2
  status="$(run_extracted "$contract_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v$version" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_NPM_BODY="$npm_body")" || true
  require_status "$status" nonzero "server.json with an extra package" "$output" || return

  write_contract_server "$version" "" 1
  jq -n '{version: "9.9.9", mcpName: "io.github.constellation-works/orbit"}' >"$npm_body"
  status="$(run_extracted "$contract_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v$version" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_NPM_BODY="$npm_body")" || true
  require_status "$status" nonzero "npm metadata for a different version" "$output" || return

  jq -n --arg version "$version" '{version: $version, mcpName: "example/other"}' >"$npm_body"
  status="$(run_extracted "$contract_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v$version" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_NPM_BODY="$npm_body")" || true
  require_status "$status" nonzero "npm metadata with a different MCP name" "$output" || return
}

assert_publisher_validate() {
  local case_dir="$temporary_directory/validate"
  mkdir -p "$case_dir/work"
  local output="$case_dir/output"
  local log="$case_dir/publisher.log"
  : >"$log"
  cat >"$case_dir/work/mcp-publisher" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MCP_TEST_PUBLISHER_LOG"
printf '%s\n' "$MCP_TEST_RAW_PUBLISHER_OUTPUT"
if [[ "$MCP_TEST_CASE" == "invalid" ]]; then
  exit 1
fi
EOF
  chmod 700 "$case_dir/work/mcp-publisher"
  local status
  status="$(run_extracted "$validate_step" "$case_dir/work" "$output" \
    PATH="$system_path" \
    MCP_TEST_CASE=ok \
    MCP_TEST_PUBLISHER_LOG="$log" \
    MCP_TEST_RAW_PUBLISHER_OUTPUT="$raw_publisher_output")" || true
  require_status "$status" 0 "registry metadata validation" "$output" || return
  if [[ "$(cat "$log")" != "validate server.json" ]]; then
    echo "property failed: metadata validation did not run mcp-publisher validate" >&2
    return 1
  fi
  if ! grep -F -x -q -- "MCP Registry metadata validated" "$output"; then
    echo "property failed: metadata validation did not report success" >&2
    return 1
  fi
  hide_secrets "$output" "$raw_publisher_output" || return

  : >"$log"
  status="$(run_extracted "$validate_step" "$case_dir/work" "$output" \
    PATH="$system_path" \
    MCP_TEST_CASE=invalid \
    MCP_TEST_PUBLISHER_LOG="$log" \
    MCP_TEST_RAW_PUBLISHER_OUTPUT="$raw_publisher_output")" || true
  require_status "$status" nonzero "invalid registry metadata" "$output" || return
  hide_secrets "$output" "$raw_publisher_output" || return
}

install_membership_fixtures() {
  local fixtures="$1"
  jq -n '[range(0; 100) | {state: "active", role: "member", organization: {login: ("other-" + tostring)}}]' \
    >"$fixtures/page-one.json"
  jq -n '[{state: "active", role: "admin", organization: {login: "constellation-works"}}]' \
    >"$fixtures/page-two-admin.json"
  jq -n '[{state: "active", role: "member", organization: {login: "constellation-works"}}]' \
    >"$fixtures/member.json"
  jq -n '[{state: "active", role: "owner", organization: {login: "constellation-works"}}]' \
    >"$fixtures/owner.json"
  jq -n '[{state: "pending", role: "admin", organization: {login: "constellation-works"}}]' \
    >"$fixtures/pending.json"
  jq -n '[{state: "active", organization: {login: "constellation-works"}}]' \
    >"$fixtures/malformed.json"
  jq -n '[{state: "active", role: "member", organization: {login: "another-org"}}]' \
    >"$fixtures/missing.json"
}

assert_membership_preflight() {
  local case_dir="$temporary_directory/preflight"
  mkdir -p "$case_dir/bin" "$case_dir/fixtures"
  install_membership_fixtures "$case_dir/fixtures"
  cat >"$case_dir/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
has_fail=false
has_auth=false
for argument in "$@"; do
  case "$argument" in
    https://*) url="$argument" ;;
  esac
  if [[ "$argument" == "--fail" ]]; then
    has_fail=true
  fi
  if [[ "$argument" == "Authorization: Bearer $MCP_REGISTRY_PAT_TOKEN" ]]; then
    has_auth=true
  fi
done
printf '%s\n' "$url" >>"$MCP_TEST_REQUEST_LOG"
if [[ "$has_fail" != true || "$has_auth" != true || -z "$url" ]]; then
  echo "membership preflight must use curl --fail and the dedicated bearer token" >&2
  exit 64
fi
case "$url" in
  'https://api.github.com/user/memberships/orgs?state=active&per_page=100&page='*) ;;
  *)
    echo "membership preflight left the active-membership endpoint: $url" >&2
    exit 64
    ;;
esac
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
  *) echo "unexpected membership fixture $MCP_TEST_CASE" >&2; exit 64 ;;
esac
EOF
  chmod 700 "$case_dir/bin/curl"

  run_membership_case() {
    local case_name="$1"
    local expected="$2"
    local require_page_two="$3"
    local output="$case_dir/$case_name.output"
    local request_log="$case_dir/$case_name.requests"
    : >"$request_log"
    local status
    status="$(run_extracted "$preflight_step" "$case_dir" "$output" \
      PATH="$case_dir/bin:$system_path" \
      MCP_REGISTRY_PAT_TOKEN="$registry_token" \
      MCP_TEST_CASE="$case_name" \
      MCP_TEST_FIXTURES="$case_dir/fixtures" \
      MCP_TEST_REQUEST_LOG="$request_log" \
      MCP_TEST_RAW_AUTH_PAYLOAD="$raw_auth_payload")" || true
    require_status "$status" "$expected" "membership preflight $case_name" "$output" || return
    if [[ "$require_page_two" == true ]] && ! grep -F -q -- 'page=2' "$request_log"; then
      echo "property failed: membership preflight $case_name did not request the second page" >&2
      return 1
    fi
    hide_secrets "$output" "$registry_token" "$raw_auth_payload" || return
  }

  run_membership_case admin-page-two 0 true || return
  run_membership_case member nonzero false || return
  run_membership_case owner nonzero false || return
  run_membership_case pending nonzero false || return
  run_membership_case missing nonzero false || return
  run_membership_case malformed nonzero false || return
  run_membership_case api-error nonzero false || return
}

assert_login_and_publish() {
  local case_dir="$temporary_directory/publish"
  mkdir -p "$case_dir/work"
  local output="$case_dir/output"
  local log="$case_dir/publisher.log"
  cat >"$case_dir/work/mcp-publisher" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MCP_TEST_PUBLISHER_LOG"
if [[ "${1:-}" == "login" && "$MCP_TEST_CASE" == "login-fail" ]]; then
  printf '%s\n' "$MCP_TEST_RAW_PUBLISHER_OUTPUT"
  exit 1
fi
if [[ "${1:-}" == "publish" && "$MCP_TEST_CASE" == "publish-fail" ]]; then
  printf '%s\n' "$MCP_TEST_RAW_PUBLISHER_OUTPUT"
  exit 1
fi
if [[ "${1:-}" != "login" && "${1:-}" != "publish" ]]; then
  echo "unexpected publisher command" >&2
  exit 64
fi
EOF
  chmod 700 "$case_dir/work/mcp-publisher"

  run_publish_case() {
    local case_name="$1"
    local expected="$2"
    : >"$log"
    local status
    status="$(run_extracted "$publish_step" "$case_dir/work" "$output" \
      PATH="$system_path" \
      MCP_REGISTRY_PAT_TOKEN="$registry_token" \
      MCP_TEST_CASE="$case_name" \
      MCP_TEST_PUBLISHER_LOG="$log" \
      MCP_TEST_RAW_PUBLISHER_OUTPUT="$raw_publisher_output")" || true
    require_status "$status" "$expected" "publisher $case_name" "$output" || return
    hide_secrets "$output" "$registry_token" "$raw_publisher_output" || return
  }

  run_publish_case success 0 || return
  if [[ "$(sed -n '1p' "$log")" != "login github --token $registry_token" ]]; then
    echo "property failed: publication did not log in with the dedicated token" >&2
    return 1
  fi
  if [[ "$(sed -n '2p' "$log")" != "publish server.json" ]]; then
    echo "property failed: publication did not publish server.json after login" >&2
    return 1
  fi
  if ! grep -F -x -q -- "MCP Registry publication completed" "$output"; then
    echo "property failed: successful publication did not report completion" >&2
    return 1
  fi

  run_publish_case login-fail nonzero || return
  if grep -F -q -- "publish server.json" "$log"; then
    echo "property failed: publisher login failure continued into publish" >&2
    return 1
  fi
  if ! grep -F -q -- \
    "MCP Registry authentication failed; verify the dedicated token's active constellation-works admin membership" \
    "$output"; then
    echo "property failed: publisher login failure was not replaced with the sanitized authentication message" >&2
    return 1
  fi

  run_publish_case publish-fail nonzero || return
  if ! grep -F -q -- "publish server.json" "$log"; then
    echo "property failed: publication failure did not attempt publish" >&2
    return 1
  fi
  if ! grep -F -q -- \
    "MCP Registry publication failed; inspect registry availability and validated metadata" \
    "$output"; then
    echo "property failed: publisher failure was not replaced with the sanitized publication message" >&2
    return 1
  fi
}

write_public_record() {
  local name="$1"
  local version="$2"
  local identifier="$3"
  local argument_json="$4"
  jq -n \
    --arg name "$name" \
    --arg version "$version" \
    --arg identifier "$identifier" \
    --argjson arguments "$argument_json" \
    '{
      server: {
        name: $name,
        version: $version,
        packages: [{
          identifier: $identifier,
          version: $version,
          packageArguments: $arguments
        }]
      }
    }' >"$case_dir/body.json"
}

assert_public_record() {
  local case_dir="$temporary_directory/record"
  mkdir -p "$case_dir/work" "$case_dir/bin"
  local output="$case_dir/output"
  local curl_log="$case_dir/curl.log"
  : >"$curl_log"
  cat >"$case_dir/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
has_fail=false
for argument in "$@"; do
  printf '%s\n' "$argument" >>"$MCP_TEST_CURL_LOG"
  case "$argument" in
    https://*) url="$argument" ;;
  esac
  if [[ "$argument" == "--fail" ]]; then
    has_fail=true
  fi
done
if [[ "$has_fail" != true || -z "$url" ]]; then
  echo "public registry readback must use curl --fail" >&2
  exit 64
fi
if [[ "$MCP_TEST_CASE" == "api-error" ]]; then
  echo "simulated registry failure" >&2
  exit 22
fi
cat "$MCP_TEST_BODY"
EOF
  chmod 700 "$case_dir/bin/curl"
  local arguments='[{"type":"positional","value":"mcp"},{"type":"positional","value":"serve"}]'
  local record_url="https://registry.modelcontextprotocol.io/v0.1/servers/io.github.constellation-works%2Forbit/versions/1.2.3"

  write_public_record "io.github.constellation-works/orbit" "1.2.3" "@orbit-tools/cli" "$arguments"
  local status
  status="$(run_extracted "$record_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v1.2.3" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_BODY="$case_dir/body.json")" || true
  require_status "$status" 0 "matching public registry record" "$output" || return
  if ! grep -F -x -q -- "$record_url" "$curl_log"; then
    echo "property failed: public readback did not request the registry record for the release version" >&2
    sed -n '1,20p' "$curl_log" >&2
    return 1
  fi

  write_public_record "example/other" "1.2.3" "@orbit-tools/cli" "$arguments"
  status="$(run_extracted "$record_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v1.2.3" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_BODY="$case_dir/body.json")" || true
  require_status "$status" nonzero "public record with a different server name" "$output" || return

  local operator_arguments='[{"type":"positional","value":"mcp"},{"type":"positional","value":"serve"},{"type":"positional","value":"--operator"}]'
  write_public_record "io.github.constellation-works/orbit" "1.2.3" "@orbit-tools/cli" "$operator_arguments"
  status="$(run_extracted "$record_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v1.2.3" \
    MCP_TEST_CASE=ok \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_BODY="$case_dir/body.json")" || true
  require_status "$status" nonzero "public record that adds --operator" "$output" || return

  status="$(run_extracted "$record_step" "$case_dir/work" "$output" \
    PATH="$case_dir/bin:$system_path" \
    RELEASE_TAG="v1.2.3" \
    MCP_TEST_CASE=api-error \
    MCP_TEST_CURL_LOG="$curl_log" \
    MCP_TEST_BODY="$case_dir/body.json")" || true
  require_status "$status" nonzero "public registry readback failure" "$output" || return
}

assert_structural_invariants() {
  python3 - "$workflow" <<'PY'
import re
import sys
from pathlib import Path
try:
    import yaml
except ImportError:
    sys.stderr.write("PyYAML is required to read workflow structure\n")
    sys.exit(2)

document = yaml.safe_load(Path(sys.argv[1]).read_text())
errors = []
# PyYAML's YAML 1.1 loader treats the unquoted Actions key `on` as True.
events = document.get("on", document.get(True))
if list(events or {}) != ["workflow_dispatch"]:
    errors.append(
        "structural invariant: publication is manual workflow_dispatch only, "
        "so a push, tag, schedule, or pull_request event cannot publish "
        "(found %s)" % list(events or {})
    )

jobs = document.get("jobs") or {}
publish = jobs.get("publish") or {}
checkout_ref = "${{ needs.verify-release.outputs.commit_sha }}"
checkout_found = False
for step in publish.get("steps") or []:
    uses = str(step.get("uses") or "")
    if uses.startswith("actions/checkout@"):
        checkout_found = True
        if (step.get("with") or {}).get("ref") != checkout_ref:
            errors.append(
                "structural invariant: the publish job checks out "
                "needs.verify-release.outputs.commit_sha, the commit the verify "
                "job resolved, and never the caller-supplied tag"
            )
if not checkout_found:
    errors.append(
        "structural invariant: the publish job checks out "
        "needs.verify-release.outputs.commit_sha, the commit the verify job resolved"
    )
for job in jobs.values():
    for step in job.get("steps") or []:
        ref = (step.get("with") or {}).get("ref")
        if ref in ("${{ inputs.tag }}", "${{ github.ref }}", "${{ github.sha }}"):
            errors.append(
                "structural invariant: no step checks out the caller-supplied tag "
                "or the workflow ref (%s)" % ref
            )

names = [step.get("name") for step in publish.get("steps") or []]
preflight = "Require an active constellation-works administrator membership"
login = "Authenticate and publish"
if preflight not in names or login not in names or names.index(preflight) >= names.index(login):
    errors.append(
        "structural invariant: the administrator membership preflight stays before "
        "publisher login, so a rejected token is not presented to the registry"
    )

echo_token = re.compile(
    r"^\s*(?:echo|printf)\b.*\$(?:\{)?(?:MCP_REGISTRY_PAT_TOKEN|GITHUB_TOKEN)\b"
)
for job in jobs.values():
    for step in job.get("steps") or []:
        script = step.get("run")
        if not isinstance(script, str):
            continue
        for line in script.splitlines():
            if echo_token.search(line):
                errors.append(
                    "structural invariant: no run step echoes MCP_REGISTRY_PAT_TOKEN "
                    "or GITHUB_TOKEN; the dedicated registry credential and the Actions "
                    "token must not be written to the log (%s)" % step.get("name")
                )
                break

if errors:
    sys.stderr.write("\n".join(errors) + "\n")
    sys.exit(1)
PY
}

expect_guard_rejects() {
  local description="$1"
  local old="$2"
  local new="$3"
  local check="$4"
  mutation_index=$((mutation_index + 1))
  local copy="$temporary_directory/mutated-$mutation_index.yml"
  python3 - "$workflow" "$copy" "$old" "$new" <<'PY'
import pathlib
import sys
source, destination, old, new = sys.argv[1:]
text = pathlib.Path(source).read_text()
count = text.count(old)
if count != 1:
    sys.stderr.write("mutation target occurred %d times: %s\n" % (count, old))
    sys.exit(1)
pathlib.Path(destination).write_text(text.replace(old, new, 1))
PY
  local saved_workflow="$workflow"
  local probe_log="$temporary_directory/probe-$mutation_index.log"
  workflow="$copy"
  set +e
  "$check" >"$probe_log" 2>&1
  local status=$?
  set -e
  workflow="$saved_workflow"
  if [[ "$status" -eq 0 ]]; then
    echo "guard accepted the workflow after $description" >&2
    cat "$probe_log" >&2
    exit 1
  fi
  if [[ "$status" -ne 1 ]]; then
    echo "mutation probe could not evaluate the guard after $description (status $status)" >&2
    cat "$probe_log" >&2
    exit 1
  fi
  echo "mutation rejected: $description"
}

assert_structural_invariants
assert_release_tag_rejects_untrusted_input
assert_published_release_resolves_commit
assert_draft_release_rejected
assert_release_tag_name_mismatch_rejected
assert_annotated_tag_resolves_to_commit
assert_unresolved_objects_rejected
assert_checksum_rejects_substituted_archive
assert_registry_contract
assert_publisher_validate
assert_membership_preflight
assert_login_and_publish
assert_public_record

expect_guard_rejects \
  "weakening the release-tag pattern to ^.*$" \
  '=~ ^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$' \
  '=~ ^.*$' \
  assert_release_tag_rejects_untrusted_input
expect_guard_rejects \
  "removing the non-draft release requirement" \
  ' and .draft == false' \
  '' \
  assert_draft_release_rejected
expect_guard_rejects \
  "replacing sha256sum --check with cat" \
  'sha256sum --check' \
  'cat' \
  assert_checksum_rejects_substituted_archive

echo "MCP Registry publication workflow checks passed"
