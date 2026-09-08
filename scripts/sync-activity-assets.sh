#!/usr/bin/env bash
# Materialize activity assets that have a byte-identity assertion against the
# workspace resources. The assertions are discovered from include_str! paths
# so adding another asserted pair does not require editing this guard.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
source_root="$repo_root/crates/orbit-core/assets/activities"
target_root="$repo_root/.orbit/resources/activities"

activity_names=()
while IFS= read -r name; do
  activity_names+=("$name")
done < <(
  rg --no-filename --only-matching \
    'include_str!\("[^"]*\.orbit/resources/activities/[^\"]+\.yaml"\)' \
    "$repo_root/crates" \
    | sed -E 's#.*\.orbit/resources/activities/([^"/]+\.yaml).*#\1#' \
    | sort -u
)

if [[ "${#activity_names[@]}" -eq 0 ]]; then
  echo "sync-activity-assets: no asserted activity mirrors found under crates" >&2
  exit 1
fi

if [[ "${1:-}" == "--check" ]]; then
  if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 [--check]" >&2
    exit 2
  fi

  drifted=0
  for name in "${activity_names[@]}"; do
    source_path="$source_root/$name"
    target_path="$target_root/$name"
    if [[ ! -f "$source_path" || ! -f "$target_path" ]]; then
      echo "sync-activity-assets: asserted mirror is missing one side:" >&2
      echo "  canonical: crates/orbit-core/assets/activities/$name" >&2
      echo "  workspace: .orbit/resources/activities/$name" >&2
      echo "  resync: ./scripts/sync-activity-assets.sh" >&2
      drifted=1
      continue
    fi
    if ! cmp -s "$source_path" "$target_path"; then
      echo "sync-activity-assets: mirror drift detected:" >&2
      echo "  canonical: crates/orbit-core/assets/activities/$name" >&2
      echo "  workspace: .orbit/resources/activities/$name" >&2
      echo "  resync: ./scripts/sync-activity-assets.sh" >&2
      diff -u "$source_path" "$target_path" || true
      drifted=1
    fi
  done
  if [[ "$drifted" -ne 0 ]]; then
    exit 1
  fi
  echo "sync-activity-assets: asserted activity mirrors match canonical assets"
  exit 0
fi

if [[ "$#" -ne 0 ]]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi

mkdir -p "$target_root"
for name in "${activity_names[@]}"; do
  source_path="$source_root/$name"
  target_path="$target_root/$name"
  if [[ ! -f "$source_path" ]]; then
    echo "sync-activity-assets: canonical activity is missing: $source_path" >&2
    exit 1
  fi
  cp "$source_path" "$target_path"
done

echo "sync-activity-assets: materialized asserted activity mirrors"
