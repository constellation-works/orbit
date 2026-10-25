#!/usr/bin/env bash
set -euo pipefail

if ! command -v rg >/dev/null 2>&1; then
  echo "check-dependency-direction: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

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
      # Registry owns local machine/workspace files. Since ORB-12725 this
      # machine's identity is the `[machine]` table in the global config.toml,
      # so Registry reads it through orbit-config's admission rather than
      # keeping a second parser and a second set of validators.
      echo "orbit-common orbit-config orbit-types"
      ;;
    orbit-policy | orbit-exec | orbit-store)
      echo "orbit-common orbit-types"
      ;;
    orbit-search)
      echo "orbit-common orbit-types"
      ;;
    orbit-tools)
      echo "orbit-common orbit-exec orbit-policy orbit-types"
      ;;
    orbit-agent)
      echo "orbit-common orbit-types"
      ;;
    orbit-engine)
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-agent orbit-common orbit-engine orbit-exec orbit-store orbit-tools orbit-types"
      ;;
    orbit-automation)
      echo "orbit-common orbit-store orbit-types"
      ;;
    orbit-runtime)
      # The runtime kernel sits below Core's use cases. It reaches them only
      # through the port Core supplies, so it never depends on orbit-core.
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-common orbit-config orbit-engine orbit-exec orbit-policy orbit-runtime orbit-search orbit-store orbit-tools orbit-types"
      ;;
    orbit-core)
      # ORB-10617: Linux sandbox regression tests compose Core with Exec; this
      # remains test-only and does not widen Core's production dependency graph.
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-automation orbit-common orbit-config orbit-core orbit-search orbit-engine orbit-exec orbit-policy orbit-registry orbit-runtime orbit-store orbit-tools orbit-types"
      ;;
    orbit-cmd)
      # The shared application composition layer joins Core runtime kernels to
      # machine-local Registry state for CLI and dashboard consumers.
      echo "orbit-common orbit-config orbit-core orbit-mcp orbit-registry orbit-store orbit-tools orbit-types"
      ;;
    orbit-mcp)
      # MCP owns framing, canonical discovery, and direct SSH stdio transport.
      # Its dev-only self dependency enables scoped transport fixture caps.
      echo "orbit-common orbit-mcp orbit-registry orbit-tools orbit-types"
      ;;
    orbit-web)
      echo "orbit-common orbit-cmd orbit-core orbit-registry orbit-types orbit-engine orbit-store"
      ;;
    orbit-cli)
      # The executable assembles MCP and Web feature crates with Registry state
      # and Core's authoritative runtime dispatcher.
      echo "orbit-common orbit-cmd orbit-config orbit-core orbit-mcp orbit-registry orbit-web orbit-types orbit-engine orbit-exec orbit-tools"
      ;;
    *)
      return 1
      ;;
  esac
}

allowed_dev_only_deps() {
  case "$1" in
    orbit-mcp)
      # Only integration tests enable its test-support transport hooks.
      echo "orbit-mcp"
      ;;
    orbit-web)
      echo "orbit-engine orbit-store"
      ;;
    orbit-cli)
      echo "orbit-engine orbit-exec orbit-tools"
      ;;
    orbit-engine)
      # Engine's own tests enable its `test-support` feature to build host
      # doubles on the null host base; no other crate is reached.
      echo "orbit-engine"
      ;;
    orbit-runtime)
      # The kernel's unit tests enable its `test-support` feature.
      echo "orbit-runtime"
      ;;
    orbit-core)
      # Core's own integration tests enable its `test-support` feature to
      # substitute the detached pipeline worker; no other crate is reached.
      echo "orbit-core"
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

# Cargo metadata resolves workspace inheritance, so inspect the manifests to
# catch a member that pins a dependency already defined by the workspace.
# Include target-specific and dev/build dependencies, not just production ones.
if ! python3 - "$repo_root/Cargo.toml" "${workspace_manifests[@]}" <<'PY'
import pathlib
import sys
import tomllib

workspace_manifest = pathlib.Path(sys.argv[1])
workspace_deps = tomllib.loads(workspace_manifest.read_text())["workspace"]["dependencies"]
failed = False


def check_group(manifest, section, dependencies):
    global failed
    for name, declaration in dependencies.items():
        package = declaration.get("package", name) if isinstance(declaration, dict) else name
        if (name in workspace_deps or package in workspace_deps) and not (
            isinstance(declaration, dict) and declaration.get("workspace") is True
        ):
            print(
                f"{manifest}: {section}.{name} must inherit its workspace dependency "
                "with workspace = true"
            )
            failed = True


for raw_path in sys.argv[2:]:
    path = pathlib.Path(raw_path)
    manifest = path.relative_to(workspace_manifest.parent)
    data = tomllib.loads(path.read_text())
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        check_group(manifest, section, data.get(section, {}))
    for target, groups in data.get("target", {}).items():
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            check_group(manifest, f"target.{target}.{section}", groups.get(section, {}))

sys.exit(1 if failed else 0)
PY
then
  fail=1
fi

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
# The runtime kernel lives in orbit-runtime, which cannot reach Core's use
# cases or adapters (the dependency policy above); composition is the only
# owner allowed to join resolved config, bootstrap, runtime construction, and
# adapter registration.
if rg -n 'crate::adapter' \
  "$repo_root/crates/orbit-core/src/application" \
  -g '*.rs' -g '!**/tests/**'; then
  echo "forbidden orbit-core application-to-adapter import"
  fail=1
fi

# Within the application layer [ORB-17924]: constants and DTOs that several
# modules share live in `application::contracts`, which imports none of the
# application modules; `task` reaches the state scheduler only through its
# `TaskAutomationPort`, and `job` reaches review admission only through its
# `ReviewAdmissionHook`. Grouped `use crate::application::{…}` trees count too.
app_src="$repo_root/crates/orbit-core/src/application"
app_modules='task|automation|job|review|distributed|admission|workspace_auto|pull_drain|sweep_filing'
if rg -n "crate::application::(${app_modules})\\b|\\bsuper::super\\b" \
  "$app_src/contracts" -g '*.rs' ||
  rg -n -U "crate::application::\\{[^}]*\\b(${app_modules})\\b" \
    "$app_src/contracts" -g '*.rs'; then
  echo "orbit-core application contracts must not import the application modules"
  fail=1
fi

# Two task-pilot callers the cut did not list keep their direct edge, by the
# owner's stop-and-report ruling on ORB-17924; each is allowed only as that
# exact line.
task_automation_allowed='^[^:]*/task/pilot/no_diff\.rs:[0-9]+:use crate::application::automation::source::Source;$|^[^:]*/task/pilot/prepare\.rs:[0-9]+:    let held = crate::application::automation::pilot_reservation::reserve\($'
if {
  rg -n 'crate::application::automation\b|(super::)+automation\b' \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' || true
  rg -n -U 'crate::application::\{[^}]*\bautomation\b' \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' || true
} | grep -Ev "$task_automation_allowed"; then
  echo "orbit-core task must reach automation through task::TaskAutomationPort"
  fail=1
fi

# Print every `super::` path or use-tree in production code under application module
# `$2` that climbs out of it to `application` and names one of the
# `|`-separated modules `$3`; exit nonzero when one is found.
super_chains_into() {
  python3 - "$1" "$2" "$3" <<'PY'
import pathlib
import re
import sys

app = pathlib.Path(sys.argv[1])
module = sys.argv[2]
upper = set(sys.argv[3].split("|"))
token = re.compile(
    r"(?P<line>//[^\n]*)|(?P<block>/\*)"
    r'|(?P<raw>(?:br|cr|r)(?P<hashes>\#*)")'
    r'|(?P<literal>(?:b|c)?"(?:\\[\s\S]|[^"\\])*"'
    r"|b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|[^\n])|[^'\\\n])')"
    r"|::|[a-zA-Z_][a-zA-Z_0-9]*|\S"
)
block_delimiter = re.compile(r"/\*|\*/")


def code_tokens(source):
    """Skip Rust comments and literals without losing diagnostic offsets."""
    index = 0
    while match := token.search(source, index):
        index = match.end()
        if match.lastgroup in ("line", "literal"):
            continue
        if match.group("block") is not None:
            depth = 1
            while depth and (delimiter := block_delimiter.search(source, index)):
                depth += 1 if delimiter.group() == "/*" else -1
                index = delimiter.end()
            if depth:
                return
        elif match.group("raw") is not None:
            closing = '"' + match.group("hashes")
            end = source.find(closing, index)
            if end == -1:
                return
            index = end + len(closing)
        else:
            yield match.group(), match.start()


def use_tree(tokens, index, prefix=()):
    """Expand relative paths, carrying the prefix through nested use groups."""
    paths = []
    while index + 1 < len(tokens) and tokens[index + 1][0] == "::":
        prefix += (tokens[index][0],)
        index += 2
    if index < len(tokens) and tokens[index][0] == "{":
        index += 1
        while index < len(tokens) and tokens[index][0] != "}":
            member_paths, index = use_tree(tokens, index, prefix)
            paths.extend(member_paths)
            if index < len(tokens) and tokens[index][0] == ",":
                index += 1
            else:
                break
        if index < len(tokens) and tokens[index][0] == "}":
            index += 1
    elif index < len(tokens):
        paths.append(prefix + (tokens[index][0],))
        index += 1
        if index < len(tokens) and tokens[index][0] == "as":
            index += 2
    return paths, index


failed = False
for path in sorted((app / module).rglob("*.rs")):
    rel = path.relative_to(app)
    if "tests" in rel.parts[:-1] or rel.name == "tests.rs":
        continue
    parts = ["application", *rel.with_suffix("").parts]
    if parts[-1] == "mod":
        parts.pop()
    source = path.read_text()
    tokens = list(code_tokens(source))
    index = 0
    while index + 1 < len(tokens):
        if tokens[index][0] != "super" or tokens[index + 1][0] != "::":
            index += 1
            continue
        start = tokens[index][1]
        paths, index = use_tree(tokens, index)
        for segments in paths:
            ups = 0
            while ups < len(segments) and segments[ups] == "super":
                ups += 1
            if (parts[: len(parts) - ups] == ["application"]
                    and ups < len(segments) and segments[ups] in upper):
                number = source.count("\n", 0, start) + 1
                print(f"{path}:{number}:{source.splitlines()[number - 1]}")
                failed = True
                break
sys.exit(1 if failed else 0)
PY
}

# [ORB-17936] `task` is the lowest of the nine modules above contracts. It
# reaches `job`, `review`, `distributed`, `sweep_filing` and `workspace_auto`
# only through the ports it owns (`TaskJobPort`, `TaskReviewPort`,
# `TaskDistributedPort`, `TaskSweepFilingPort`, `TaskWorkspaceAutoPort`),
# which those modules implement on `OrbitRuntime`. A path, a grouped tree, a
# `super::` chain that climbs out of `task` into one of them, or a call to the
# inherent methods and functions the ports wrap is a reintroduced edge.
task_upper_modules='job|review|distributed|sweep_filing|workspace_auto'
task_upper_calls='show_job_run_observed|creation_crew_assignment|submit_keyed_pipeline_run|host_validation_gates|task_crew_source|unbound_publication_on_record|complexity_pool_candidates|unheld_task_crew_candidates|running_run_has_verified_owner|run_owner_liveness|tags_route_through_pr_pipeline|task_review_projection|resume_evidence_hold|merged_head_completion_refusal|pilot_run_crew|piloted_elsewhere|read_live_leaf_runs'
if rg -n "crate::application::(${task_upper_modules})\\b" \
  "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  rg -n -U "crate::application::\\{[^}]*\\b(${task_upper_modules})\\b" \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  rg -n "\\b(${task_upper_calls})\\s*(::<[^>]*>)?\\s*\\(" \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  ! super_chains_into "$app_src" task "$task_upper_modules"; then
  echo "orbit-core task must reach job, review, distributed, sweep_filing and workspace_auto through its task ports"
  fail=1
fi

# [ORB-17972] The remaining inherent `OrbitRuntime` methods `job` defines
# reach `task` only through `TaskJobPort` under distinct names, so a call on
# a `self`/`runtime` receiver to any of them is a reintroduced task -> job
# edge. Store reads (`stores().jobs().read_run_state`) are not job methods.
task_job_inherent='get_job_run_backend|read_run_state|published_owner_requeue_record|failure_handoff_requeue_record|preservation_to_append|inspection_capable_default_crew|inspection_capable_pool|rerate_task_crew|auto_crew_pools_for_input|draw_crew_pool|purpose_assignment_candidates|observe_task_delivery'
# Store reads through `stores().jobs()` are not `job` inherent methods; only
# those exact lines are allowed.
task_job_store_allowed='task/steering\.rs:[0-9]+:.*stores\(\)\.jobs\(\)\.read_run_state|task/steering\.rs:[0-9]+:\s+\.read_run_state\(run_id\)|task/steering\.rs:[0-9]+:\s+\.read_run_state\(&source\.run_id\)|task/pilot/drain_promotion\.rs:[0-9]+:\s+\.read_run_state\(run_id\)'
if {
  rg -n "\\.(${task_job_inherent})\\s*(::<[^>]*>)?\\s*\\(" \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' || true
} | grep -Ev "$task_job_store_allowed"; then
  echo "orbit-core task must reach job's remaining inherent methods through task::TaskJobPort [ORB-17972]"
  fail=1
fi

# [ORB-17937] `job` reaches the distributed drain and pull settlement only
# through the ports it owns (`JobDrainPort`, `JobPullSettlementPort`), which
# `distributed` and `pull_drain` implement on `OrbitRuntime`; the shapes they
# share (`DrainClaimedLeaf`, `HostProviderHold`) are contracts. A path, a
# grouped tree, a `super::` chain out of `job` into either, or a call to the
# inherent methods the ports wrap is a reintroduced edge.
job_drain_modules='distributed|pull_drain'
job_drain_calls='drain_entry_admission|preflight_git_protection|admission_resource_throttle|standing_host_provider_holds|pull_drain_claimed_leaves|pull_drain_admissions|owner_completion_authority|admission_boundary|settle_pending_pulls|carry_settlements_for|deliver_claimed_leaf_settlement|retry_own_claimed_leaf_settlement|record_forced_leaf_release'
if rg -n "crate::application::(${job_drain_modules})\\b" \
  "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  rg -n -U "crate::application::\\{[^}]*\\b(${job_drain_modules})\\b" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  rg -n "\\b(${job_drain_calls})\\s*(::<[^>]*>)?\\s*\\(" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  ! super_chains_into "$app_src" job "$job_drain_modules"; then
  echo "orbit-core job must reach distributed and pull_drain through job::JobDrainPort and job::JobPullSettlementPort"
  fail=1
fi

# [ORB-17938] `job` reaches review admission, state automation, backlog
# admission and the auto drain's concurrency mode only through the ports it
# owns (`ReviewAdmissionHook`, `JobAutomationPort`, `JobAdmissionPort`,
# `JobAutoConcurrencyPort`), which `review`, `automation`, `admission` and
# `workspace_auto` implement on `OrbitRuntime`; the job name and filing tags
# it shares with `review` and `sweep_filing` are contracts. A path, a grouped
# tree, a `super::` chain out of `job` into any of them, or a call to the
# inherent methods and functions the automation, admission and
# auto-concurrency ports wrap is a reintroduced edge. (The review hook's
# methods share their names with the inherent ones it wraps, so a path is
# the only way back into `review`.)
job_upper_modules='admission|automation|review|sweep_filing|workspace_auto'
job_upper_calls='bundle_crews|task_pilot_warning|deferred_footprint_reservation|capture_auto_concurrency'
if rg -n "crate::application::(${job_upper_modules})\\b" \
  "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  rg -n -U "crate::application::\\{[^}]*\\b(${job_upper_modules})\\b" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  rg -n "\\b(${job_upper_calls})\\s*(::<[^>]*>)?\\s*\\(" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  ! super_chains_into "$app_src" job "$job_upper_modules"; then
  echo "orbit-core job must reach review, automation, admission and workspace_auto through its job ports"
  fail=1
fi

shared_contracts='PULL_DRAIN_JOB|DrainClaimedLeaf|ClaimedLeafStage|HostProviderHold|ESCALATION_TRIAGE_JOB|FINDING_VERIFICATION_JOB|DrainEntryPoint|PullSettlementEntry|PullSettlementOutcome|SUPERSEDED_BY_SOURCE|PilotFingerprints|review_layer_json|CreationCrewAssignment|CrewCandidate|InspectionPool|PURPOSE_SOURCE_PREFIX|PROVIDER_LIMIT_REASSIGNMENT_PREFIX|auto_task_minted|automatic_crew_source|RetryKey|LOCAL_ROUTE_JOB|CI_FAILURE_TAG|DEPENDABOT_TAG|CODE_SCANNING_TAG|RECONCILIATION_FOLLOW_UP_TAG_PREFIX|PublishedDelivery'
if rg -n "\\b(const|enum|struct|fn) (${shared_contracts})\\b" \
  "$repo_root/crates/orbit-core/src" -g '*.rs' -g "!**/application/contracts/**" ||
  rg -n "\\b(${app_modules})(::[a-z_]+)*::(${shared_contracts})\\b" \
    "$repo_root/crates" -g '*.rs' ||
  rg -n -U "\\b(${app_modules})(::[a-z_]+)*::\\{[^}]*\\b(${shared_contracts})\\b" \
    "$repo_root/crates" -g '*.rs'; then
  echo "shared application contracts are defined and imported only at application::contracts"
  fail=1
fi

if rg -n 'ResolvedConfig::load|ConfigRoots::' \
  "$repo_root/crates/orbit-runtime/src" \
  -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-runtime must consume resolved config supplied by composition"
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
  "$repo_root/crates/orbit-runtime/src/orbit_tool_host" \
  "$repo_root/crates/orbit-runtime/src/engine/runtime_host.rs"; do
  if [[ -e "$retired_path" ]]; then
    echo "retired runtime ownership path still exists: $retired_path"
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

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "dependency direction guard passed"
