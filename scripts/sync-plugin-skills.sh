#!/usr/bin/env bash
# Materialize the agent plugin skill tree from Orbit's canonical embedded assets.
# Every directory directly under assets/skills is a shipped skill, except a
# leading-underscore name (an archived skill excluded from the catalog,
# mirroring crates/orbit-core/src/application/skill.rs's own filter).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
source_root="$repo_root/crates/orbit-core/assets/skills"
target_root="$repo_root/plugin/skills"

skill_dirs=()
for entry in "$source_root"/*/; do
  name="$(basename "$entry")"
  [[ "$name" == _* ]] && continue
  if [[ ! -f "$entry/SKILL.md" ]]; then
    echo "sync-plugin-skills: canonical skill is missing: ${entry}SKILL.md" >&2
    exit 1
  fi
  skill_dirs+=("$name")
done

if [[ "${#skill_dirs[@]}" -eq 0 ]]; then
  echo "sync-plugin-skills: no skills found under $source_root" >&2
  exit 1
fi

target_skill_dirs=()
if [[ -d "$target_root" ]]; then
  for entry in "$target_root"/*/ "$target_root"/.[!.]*/ "$target_root"/..?*/; do
    [[ -d "$entry" ]] || continue
    target_skill_dirs+=("$(basename "$entry")")
  done
fi

if [[ "${1:-}" == "--check" ]]; then
  if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 [--check]" >&2
    exit 2
  fi
  drifted=0
  for name in "${skill_dirs[@]}"; do
    if ! diff -qr "$source_root/$name" "$target_root/$name" >/dev/null 2>&1; then
      echo "sync-plugin-skills: plugin/skills/$name drifted from crates/orbit-core/assets/skills/$name" >&2
      diff -ru "$source_root/$name" "$target_root/$name" || true
      drifted=1
    fi
  done
  for name in "${target_skill_dirs[@]}"; do
    is_canonical=0
    for canonical_name in "${skill_dirs[@]}"; do
      if [[ "$name" == "$canonical_name" ]]; then
        is_canonical=1
        break
      fi
    done
    if [[ "$is_canonical" -eq 0 ]]; then
      echo "sync-plugin-skills: plugin/skills/$name has no matching canonical skill under crates/orbit-core/assets/skills" >&2
      drifted=1
    fi
  done
  if [[ "$drifted" -ne 0 ]]; then
    exit 1
  fi
  echo "sync-plugin-skills: committed plugin skill mirror matches canonical assets"
  exit 0
fi
if [[ "$#" -ne 0 ]]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi

mkdir -p "$target_root"
for name in "${target_skill_dirs[@]}"; do
  is_canonical=0
  for canonical_name in "${skill_dirs[@]}"; do
    if [[ "$name" == "$canonical_name" ]]; then
      is_canonical=1
      break
    fi
  done
  if [[ "$is_canonical" -eq 0 ]]; then
    rm -rf "$target_root/$name"
  fi
done
for name in "${skill_dirs[@]}"; do
  rm -rf "$target_root/$name"
  cp -R "$source_root/$name" "$target_root/$name"
done

echo "sync-plugin-skills: materialized plugin/skills/{${skill_dirs[*]}} from crates/orbit-core/assets/skills/"
