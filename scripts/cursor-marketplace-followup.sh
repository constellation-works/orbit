#!/usr/bin/env bash
# Non-blocking, version-specific reminder that the Cursor marketplace listing
# is a human-reviewed catalog update — not part of package publication.
#
# Exit 0 when `.github/cursor-marketplace-followup/<version>.ack` names this
# version. Exit 1 and print the checklist when the ack is missing or wrong.
# This script never writes, never emails, and never calls Cursor or npm.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
ack_root=""
version=""

usage() {
  cat >&2 <<'EOF'
usage: cursor-marketplace-followup.sh [--repo-root DIR] [--ack-root DIR] [--version X.Y.Z]
EOF
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-root)
      [[ $# -ge 2 ]] || usage
      repo_root="$(cd "$2" && pwd)"
      shift 2
      ;;
    --ack-root)
      [[ $# -ge 2 ]] || usage
      ack_root="$2"
      shift 2
      ;;
    --version)
      [[ $# -ge 2 ]] || usage
      version="$2"
      shift 2
      ;;
    -h | --help)
      usage
      ;;
    *)
      echo "cursor-marketplace-followup: unknown argument: $1" >&2
      usage
      ;;
  esac
done

if [[ -z "$ack_root" ]]; then
  ack_root="$repo_root/.github/cursor-marketplace-followup"
fi

if [[ -z "$version" ]]; then
  plugin_manifest="$repo_root/plugin/plugin.json"
  if [[ ! -f "$plugin_manifest" ]]; then
    echo "cursor-marketplace-followup: $plugin_manifest is missing" >&2
    exit 2
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "cursor-marketplace-followup: required binary 'python3' not on PATH" >&2
    exit 2
  fi
  version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$plugin_manifest")"
fi

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "cursor-marketplace-followup: version $version is not a release version" >&2
  exit 2
fi

print_checklist() {
  local reason="$1"
  cat >&2 <<EOF
cursor-marketplace-followup: $reason

This is a human follow-up. Package publication (GitHub Release, Homebrew,
npm) is separate from Cursor's curated catalog and is not updated by this
check. There is no public catalog push API; do not treat a tagged release as
a marketplace publication.

Required procedure for Orbit v${version}:
1. Submit or update https://cursor.com/marketplace/publish using
   https://github.com/constellation-works/orbit and the plugin/ subdirectory.
2. Identify stale listing 2280865 (danieljhkim/orbit pinned at 0.5.1) and
   replace it with the constellation-works listing for this version.
3. Install through Cursor plugin search and verify the listed version and
   MCP surface match v${version} (npx -y @orbit-tools/cli@${version} mcp serve).
4. If review stalls, escalate to marketplace-publishing@cursor.com. Do not
   send that mail from CI or this script.
5. After the version-specific submission is done, add
   .github/cursor-marketplace-followup/${version}.ack containing:
   version=${version}
   That file records the follow-up, not that the catalog is live.

Do not retag, republish npm, rewrite historical provenance, or assume the
curated listing changed because this repository released.
EOF
  if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    echo "::error title=Cursor marketplace follow-up::Orbit v${version} Cursor listing is unacknowledged. Submit https://cursor.com/marketplace/publish from github.com/constellation-works/orbit plugin/; listing 2280865 still describes 0.5.1. This does not retract the GitHub Release."
  fi
}

ack_file="$ack_root/${version}.ack"
if [[ ! -f "$ack_file" ]]; then
  print_checklist "no acknowledgement for v${version} (${ack_file} is missing)"
  exit 1
fi

ack_version=""
while IFS= read -r line || [[ -n "$line" ]]; do
  case "$line" in
    '' | \#*)
      continue
      ;;
    version=*)
      ack_version="${line#version=}"
      ack_version="${ack_version%%[[:space:]]*}"
      ;;
  esac
done < "$ack_file"

if [[ "$ack_version" != "$version" ]]; then
  print_checklist "acknowledgement ${ack_file} is not for v${version} (found '${ack_version}')"
  exit 1
fi

echo "cursor-marketplace-followup: v${version} acknowledgement is present"
echo "cursor-marketplace-followup: this does not mean the Cursor catalog is published"
