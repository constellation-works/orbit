#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

fail=0

allowed_internal_deps() {
  case "$1" in
    orbit-types)
      echo ""
      ;;
    orbit-common)
      echo "orbit-types"
      ;;
    orbit-config)
      # Config owns config.toml admission, layering, and persistence; it is a
      # leaf above the shared mechanism crates and depends on nothing higher.
      echo "orbit-common orbit-types"
      ;;
    orbit-registry)
      # Registry owns local machine/workspace files and needs only shared types.
      echo "orbit-common orbit-types"
      ;;
    orbit-policy | orbit-exec | orbit-store)
      echo "orbit-common orbit-types"
      ;;
    orbit-search)
      # ORB-10357 folded the former orbit-search-companion crate in as an
      # additional [[bin]] target; fastembed is a workspace dependency, not
      # an internal crate edge.
      echo "orbit-common orbit-types"
      ;;
    orbit-tools)
      echo "orbit-common orbit-exec orbit-policy orbit-types"
      ;;
    orbit-agent)
      echo "orbit-common orbit-tools orbit-types"
      ;;
    orbit-engine)
      echo "orbit-agent orbit-common orbit-exec orbit-store orbit-tools orbit-types"
      ;;
    orbit-automation)
      echo "orbit-common orbit-store orbit-types"
      ;;
    orbit-core)
      # ORB-10617: Linux sandbox regression tests compose Core with Exec; this
      # remains test-only and does not widen Core's production dependency graph.
      echo "orbit-automation orbit-common orbit-config orbit-search orbit-engine orbit-exec orbit-policy orbit-store orbit-tools orbit-types"
      ;;
    orbit-cmd)
      # The shared application composition layer joins Core runtime kernels to
      # machine-local Registry state for CLI and dashboard consumers.
      echo "orbit-common orbit-config orbit-core orbit-engine orbit-registry orbit-store orbit-types"
      ;;
    orbit-mcp)
      # MCP owns framing, canonical discovery, and direct SSH stdio transport.
      echo "orbit-common orbit-registry orbit-tools orbit-types"
      ;;
    orbit-web)
      echo "orbit-common orbit-cmd orbit-core orbit-registry orbit-types"
      ;;
    orbit-cli)
      # The executable assembles MCP and Web feature crates with Registry state
      # and Core's authoritative runtime dispatcher.
      echo "orbit-common orbit-cmd orbit-config orbit-core orbit-mcp orbit-registry orbit-web orbit-types"
      ;;
    *)
      return 1
      ;;
  esac
}

allowed_dev_only_deps() {
  case "$1" in
    orbit-core)
      echo "orbit-exec"
      ;;
    *)
      echo ""
      ;;
  esac
}

contains_word() {
  local haystack="$1"
  local needle="$2"
  for word in $haystack; do
    if [[ "$word" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

load_workspace_crates() {
  cargo metadata --format-version 1 --no-deps --manifest-path "$repo_root/Cargo.toml" |
    python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
workspace_members = set(metadata["workspace_members"])
workspace_crates = sorted(
    (package["name"], package["manifest_path"])
    for package in metadata["packages"]
    if package["id"] in workspace_members
    and package["name"].startswith("orbit-")
)
for crate, manifest_path in workspace_crates:
    print(f"{crate}\t{manifest_path}")
'
}

load_workspace_dependencies() {
  cargo metadata --format-version 1 --no-deps --manifest-path "$repo_root/Cargo.toml" |
    python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
workspace_members = set(metadata["workspace_members"])
workspace_crates = {
    package["name"]: package
    for package in metadata["packages"]
    if package["id"] in workspace_members
    and package["name"].startswith("orbit-")
}
for crate in sorted(workspace_crates):
    manifest = workspace_crates[crate]["manifest_path"]
    for dependency in workspace_crates[crate]["dependencies"]:
        dependency_name = dependency["name"]
        if dependency_name.startswith("orbit-"):
            kind = dependency["kind"] or "normal"
            print(f"{crate}\t{manifest}\t{dependency_name}\t{kind}")
'
}

workspace_crates=()
workspace_manifests=()
while IFS=$'\t' read -r crate manifest; do
  if [[ -n "$crate" ]]; then
    workspace_crates+=("$crate")
    workspace_manifests+=("$manifest")
  fi
done < <(load_workspace_crates)

if [[ "${#workspace_crates[@]}" -eq 0 ]]; then
  echo "no orbit workspace crates discovered from cargo metadata"
  exit 1
fi

for index in "${!workspace_crates[@]}"; do
  crate="${workspace_crates[$index]}"
  manifest="${workspace_manifests[$index]}"
  if [[ ! -f "$manifest" ]]; then
    echo "missing manifest for ${crate}: ${manifest}"
    fail=1
    continue
  fi

  if ! allowed_internal_deps "$crate" >/dev/null; then
    echo "missing dependency direction policy for workspace crate '${crate}'"
    fail=1
  fi
done

while IFS=$'\t' read -r crate manifest dependency kind; do
  allowed="$(allowed_internal_deps "$crate")"

  if contains_word "$(allowed_dev_only_deps "$crate")" "$dependency" && [[ "$kind" != "dev" ]]; then
    echo "dependency '${dependency}' in ${manifest} must remain dev-only (kind: ${kind})"
    fail=1
  fi

  if [[ -n "$dependency" ]] && ! contains_word "$allowed" "$dependency"; then
    echo "forbidden dependency '${dependency}' found in ${manifest}"
    echo "  allowed internal deps for ${crate}: ${allowed:-<none>}"
    fail=1
  fi
done < <(load_workspace_dependencies)

# orbit-core is one crate, but its internal boundaries are directional too.
# Keep the runtime kernel independent from use cases and protocol adapters;
# composition is the only owner allowed to join resolved config, bootstrap,
# runtime construction, and adapter registration.
if rg -n 'crate::(command|application|bootstrap|adapter)' \
  "$repo_root/crates/orbit-core/src/runtime" \
  -g '*.rs' -g '!**/tests/**'; then
  echo "forbidden orbit-core runtime-to-command/application/bootstrap/adapter import"
  fail=1
fi

if rg -n 'crate::adapter' \
  "$repo_root/crates/orbit-core/src/application" \
  -g '*.rs' -g '!**/tests/**'; then
  echo "forbidden orbit-core application-to-adapter import"
  fail=1
fi

if rg -n 'ResolvedConfig::load|ConfigRoots::' \
  "$repo_root/crates/orbit-core/src/runtime" \
  -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-core runtime must consume resolved config supplied by composition"
  fail=1
fi

# orbit-store is intentionally one crate, but its internal persistence graph
# is directional: contracts and shared fs mechanics are leaves; each driver is
# isolated; only repositories, workflows, and composition may join drivers.
store_src="$repo_root/crates/orbit-store/src"
if rg -n 'crate::(driver|repository|workflow)|\brusqlite\b' \
  "$store_src/contracts" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store contracts must not import persistence implementations"
  fail=1
fi

if rg -n 'crate::(driver::sqlite|repository|workflow)|use crate::(Store|StoreTx)' \
  "$store_src/driver/file" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store file driver must not import SQLite, repositories, or workflows"
  fail=1
fi

if rg -n 'crate::(driver::file|repository|workflow)' \
  "$store_src/driver/sqlite" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store SQLite driver must not import file drivers, repositories, or workflows"
  fail=1
fi

if rg -n 'crate::(driver|repository|workflow)' \
  "$store_src/fs" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store filesystem primitives must not import drivers or orchestration"
  fail=1
fi

for retired_path in \
  "$store_src/backend" \
  "$store_src/file" \
  "$store_src/sqlite" \
  "$store_src/state_io" \
  "$store_src/task_migration"; do
  if [[ -e "$retired_path" ]]; then
    echo "retired orbit-store ownership path still exists: $retired_path"
    fail=1
  fi
done

for retired_path in \
  "$repo_root/crates/orbit-core/src/command" \
  "$repo_root/crates/orbit-core/src/runtime/orbit_tool_host" \
  "$repo_root/crates/orbit-core/src/runtime/engine/runtime_host.rs"; do
  if [[ -e "$retired_path" ]]; then
    echo "retired orbit-core ownership path still exists: $retired_path"
    fail=1
  fi
done

# State consumers share Automation's evaluator and Store checkpoint owner.
# Core may gather authoritative facts, but must not grow a second checkpoint
# implementation or hashing/coverage acceptance path [ORB-11331].
if rg -n 'automation_commit\(|Sha256' \
  "$repo_root/crates/orbit-core/src/application/automation" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "Core automation must use the shared scheduling/checkpoint contract"
  fail=1
fi

# Operation mode composes authority over the shared scheduling domain
# [ORB-11332]. Core's operation module may consume accepted assessments and
# hand the evaluator constraints, but must not grow its own evaluator,
# fingerprint, checkpoint, or coverage acceptance; Automation must never read
# grants, so authority stays in Core/Store.
if rg -n 'automation_commit\(|Sha256|fn evaluate\b|fn fingerprint\b|definition_epoch' \
  "$repo_root/crates/orbit-core/src/application/operation" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "Core operation mode must use the shared automation evaluator and checkpoint contract"
  fail=1
fi

# Before-PR review [ORB-11333] composes authority and Git evidence in Core and
# persists through Store; the coverage acceptance and exclusion rules, task
# meaning digests, and landing classification stay in Automation's `review`
# module. Core's review module must not grow its own digest, validator,
# checkpoint, or evaluator.
if rg -n 'automation_commit\(|Sha256|fn evaluate\b|fn exclusion\b|fn certificate_acceptable\b|fn task_meaning_digest\b|definition_epoch' \
  "$repo_root/crates/orbit-core/src/application/review" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "Core review must use orbit-automation's shared coverage rules and Store persistence"
  fail=1
fi

if rg -n 'ReviewStoreBackend|review_certificate_record|review_reserve' \
  "$repo_root/crates/orbit-automation/src" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "orbit-automation must not persist review evidence; Store owns ledgers and certificates"
  fail=1
fi

if rg -n 'OperationGrant|operation_grant|OperationStoreBackend' \
  "$repo_root/crates/orbit-automation/src" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "orbit-automation must not read operation-mode grants; authority is composed in Core"
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "dependency direction guard passed"
