#!/usr/bin/env bash
set -euo pipefail

if ! command -v rg >/dev/null 2>&1; then
  echo "check-dependency-direction: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

fail=0

# Conditional scans intentionally accept rg's no-match status (1), but an IO
# error is itself a violation. Emit a match on stdout too, so command
# substitutions and exclusion pipelines cannot turn a scan error into clean.
dependency_matches() {
  local status=0
  command rg "$@" || status=$?
  if [[ "$status" -gt 1 ]]; then
    printf 'dependency scan failed (ripgrep exit %s)\n' "$status"
    return 0
  fi
  return "$status"
}

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
    orbit-clock)
      # The host's sweep clock (clock.toml, unit rendering, install, health).
      # It names only the shared mechanisms so it compiles beside the
      # orbit-store chain, not inside the serial orbit-core library [ORB-22688].
      echo "orbit-common"
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
    orbit-auth)
      # Credential code only: no HTTP framework, runtime handle or Orbit store,
      # so its surfaces (CLI, listener) wrap it rather than the reverse.
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
      # The sweep pass re-exports the clock's tick budget [ORB-22688].
      echo "orbit-clock orbit-common orbit-store orbit-types"
      ;;
    orbit-runtime)
      # The runtime kernel sits below Core's use cases. It reaches them only
      # through the port Core supplies, so it never depends on orbit-core.
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-common orbit-config orbit-engine orbit-exec orbit-policy orbit-runtime orbit-search orbit-store orbit-tools orbit-types"
      ;;
    orbit-core)
      # Core's script steps, CodeQL fulfilment, plugin conformance, browser kit
      # reads and host probes use Exec in production; sandbox regression tests
      # also compose Core with Exec's `test-support` feature.
      # Its dev-only self dependency enables the `test-support` feature.
      echo "orbit-automation orbit-clock orbit-common orbit-config orbit-core orbit-search orbit-engine orbit-exec orbit-policy orbit-registry orbit-runtime orbit-store orbit-tools orbit-types"
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
      # Lease token scopes and startup machine settings are host-local inputs.
      echo "orbit-auth orbit-common orbit-cmd orbit-config orbit-core orbit-registry orbit-types orbit-engine orbit-store"
      ;;
    orbit-cli)
      # The executable assembles MCP and Web feature crates with Registry state
      # and Core's authoritative runtime dispatcher.
      echo "orbit-auth orbit-common orbit-cmd orbit-config orbit-core orbit-mcp orbit-registry orbit-web orbit-types orbit-engine orbit-exec orbit-tools"
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
      # substitute the detached pipeline worker, and its integration tests
      # reach orbit-policy directly; production code reaches policy through
      # orbit-runtime (and orbit-tools), never directly.
      echo "orbit-core orbit-policy"
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
if dependency_matches -n 'crate::adapter' \
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
app_modules='task|automation|job|review|distributed|admission|workspace_auto|backlog_forecast|pull_drain|sweep_filing'
# [ORB-22623] The clock settings and schedule display state keep contracts
# below `routines` and `auto_tasks`.
contracts_higher="${app_modules}|routines|auto_tasks"

# Print every `super::` path or use-tree in production code under application module
# `$2` that climbs out of it to `application`, or past it to the crate root and back
# down through `application`, and names one of the `|`-separated modules `$3`; exit
# nonzero when one is found. With `$4` set to `crate-root`, `$1` is any layer under
# the crate source and the modules are crate-root modules: a `super::` chain counts
# when it climbs out to the crate root, and a `crate::` path or use-tree counts too
# [ORB-22005]. With `$4` set to `application-root`, also expand absolute
# `crate::application::` paths and nested use-trees into the application modules.
super_chains_into() {
  python3 - "$1" "$2" "$3" "${4:-}" <<'PY'
import pathlib
import re
import sys

app = pathlib.Path(sys.argv[1])
module = sys.argv[2]
upper = set(sys.argv[3].split("|"))
crate_root = sys.argv[4] == "crate-root"
climb_target = [] if crate_root else [app.name]
path_starts = ("super", "crate") if sys.argv[4] in ("crate-root", "application-root") else ("super",)
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
target = app / module
paths = [target] if target.is_file() else sorted(target.rglob("*.rs"))
for path in paths:
    rel = path.relative_to(app)
    if "tests" in rel.parts[:-1] or rel.name == "tests.rs":
        continue
    parts = ([] if crate_root and module else [app.name]) + list(rel.with_suffix("").parts)
    if parts[-1] == "mod":
        parts.pop()
    source = path.read_text()
    tokens = list(code_tokens(source))
    index = 0
    while index + 1 < len(tokens):
        if tokens[index][0] not in path_starts or tokens[index + 1][0] != "::":
            index += 1
            continue
        start = tokens[index][1]
        paths, index = use_tree(tokens, index)
        for segments in paths:
            ups = 0
            while ups < len(segments) and segments[ups] == "super":
                ups += 1
            if segments[0] == "crate":
                absolute = segments[1:] if crate_root else segments[2:]
                named = (len(absolute) > 0 and absolute[0] in upper
                         and (crate_root or segments[1:2] == (app.name,)))
            else:
                # Resolve the chain from this file's module depth [ORB-22674]. It
                # names an upper module when it lands on the climb target and an
                # `upper` module follows, or when it climbs past the target to the
                # crate root and descends `application::<upper>`. A landing inside
                # a module, such as `application::task::distributed`, names none.
                climbed = parts[: max(len(parts) - ups, 0)]
                resolved = [*climbed, *segments[ups:]]
                depth = len(climb_target)
                named = (len(climbed) <= depth
                         and resolved[:depth] == climb_target
                         and len(resolved) > depth
                         and resolved[depth] in upper)
            if named:
                number = source.count("\n", 0, start) + 1
                print(f"{path}:{number}:{source.splitlines()[number - 1]}")
                failed = True
                break
sys.exit(1 if failed else 0)
PY
}

# Resolve relative use trees at each file's module depth: grouped and multiline
# imports into higher modules count, while imports within contracts stay legal.
if dependency_matches -n "crate::application::(${contracts_higher})\\b" \
  "$app_src/contracts" -g '*.rs' ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${contracts_higher})\\b" \
    "$app_src/contracts" -g '*.rs' ||
  ! super_chains_into "$app_src" contracts "$contracts_higher"; then
  echo "orbit-core application contracts must not import the application modules"
  fail=1
fi

# Config path primitives are below aliases and operation, which must not
# depend on layered admission. The leaf may only use TOML and std.
config_src="$repo_root/crates/orbit-config/src"
# These are policy owners, not optional files. A rename must update the rule,
# rather than silently dropping that boundary from the scan.
for module in aliases.rs seeding.rs config_document.rs crew operation provider_limit \
  layering plugin_enablement.rs unknown.rs resolved/compatibility.rs config_io.rs \
  registry crew_defaults.rs; do
  if [[ ! -e "$config_src/$module" ]]; then
    echo "dependency scan path missing: $config_src/$module" >&2
    exit 1
  fi
done
for module in aliases.rs operation; do
  if ! super_chains_into "$config_src" "$module" layering crate-root; then
    echo "orbit-config ${module} must not name layering"
    fail=1
  fi
done
# [ORB-22707] The leaf also owns the typed path read, so it may use serde and
# orbit-common's error and redaction, but never a sibling module.
if dependency_matches -n '\b(crate|super|self)::|^[[:space:]]*extern crate[[:space:]]' "$config_src/config_document.rs"; then
  echo "orbit-config config_document must remain a TOML/serde/orbit-common/std leaf"
  fail=1
fi
if dependency_matches -n '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]' "$config_src/config_document.rs" |
  grep -vE '[[:space:]]use[[:space:]]+(std|toml|serde|orbit_common)::|:use[[:space:]]+(std|toml|serde|orbit_common)::'; then
  echo "orbit-config config_document must remain a TOML/serde/orbit-common/std leaf"
  fail=1
fi

# [ORB-22707] The key registry sits above the domain admission modules whose
# admitters and choices its key table calls. Those modules read TOML through
# config_document and leave snapshot projections to registry/settings, so none
# names the registry; its crate-root re-exports count as the same edge.
config_registry_names='registry|AutoConcurrencySettings|CONFIG_KEY_REGISTRY|ConfigKeyDescriptor|ConfigKeyTier|ConfigSection|ConfigSnapshot|ExperimentSettings|ExploreBriefSettings|ExploreSettings|LeaseListenerSettings|LeaseListenerTlsSettings|MachineSettings|MidRunSteeringSettings|NoticeKindHours|NoticeSettings|RegressionRiskSettings|ResourceThrottleSettings|WorkerContainmentSettings|admit_config_key|admit_settable_config_key|config_key_options|describe_config_key|immutable_key_note|is_global_only_key'
for module in aliases.rs seeding.rs crew operation provider_limit; do
  if ! super_chains_into "$config_src" "$module" "$config_registry_names" crate-root; then
    echo "orbit-config ${module} must not name the registry; read TOML through config_document"
    fail=1
  fi
done

# [ORB-22315] File reads and path admission live in the config_io leaf, below
# loading and diagnostics and below the mutable ConfigStore those modules must
# not reach. The crate-root re-exports of the store's types count as the same
# edge. The leaf itself uses only std and orbit-common: no sibling module and no
# TOML or serde parsing.
config_store_names='store|ConfigStore|ConfigScope|WorkspaceInitMode'
for module in layering plugin_enablement.rs unknown.rs seeding.rs; do
  if ! super_chains_into "$config_src" "$module" "$config_store_names" crate-root; then
    echo "orbit-config ${module} must not name the store; read through config_io"
    fail=1
  fi
done
if ! super_chains_into "$config_src" resolved/compatibility.rs store crate-root; then
  echo "orbit-config resolved/compatibility must read through config_io, not the store"
  fail=1
fi
if dependency_matches -n '\b(crate|super|self|toml|toml_edit|serde)::|^[[:space:]]*extern crate[[:space:]]' "$config_src/config_io.rs"; then
  echo "orbit-config config_io must remain a std/orbit-common leaf"
  fail=1
fi
if dependency_matches -n '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]' "$config_src/config_io.rs" |
  grep -vE '[[:space:]]use[[:space:]]+(std|orbit_common)::|:use[[:space:]]+(std|orbit_common)::'; then
  echo "orbit-config config_io must remain a std/orbit-common leaf"
  fail=1
fi

# [ORB-22417] Built-in crew defaults live in the crew_defaults leaf, below
# snapshot admission, resolved config, layering and seeding, so the registry
# admits the default crews without naming resolved config (the crate-root
# re-exports of resolved config count as the same edge). The leaf itself uses
# only std, orbit-common and orbit-types: no sibling module.
config_resolved_names='resolved|ResolvedConfig|PrSettings|CodexExecutionPolicy|ExecutionEnvPolicy|CompatibilityConfigKey|CompatibilityConfigKeyDisposition|RETIRED_SYSTEM_CREW|RETIRED_SYSTEM_CREW_WARNING|compatibility_config_keys_at|compatibility_config_keys_present|disabled_crew_message|executor_host_os|names_retired_system_crew'
if ! super_chains_into "$config_src" registry "$config_resolved_names" crate-root; then
  echo "orbit-config registry must not name resolved config; read crew defaults through crew_defaults"
  fail=1
fi
if dependency_matches -n '\b(crate|super|self|toml|toml_edit|serde|serde_json|jsonschema|tracing)::|^[[:space:]]*extern crate[[:space:]]' "$config_src/crew_defaults.rs"; then
  echo "orbit-config crew_defaults must remain a std/orbit-common/orbit-types leaf"
  fail=1
fi
if dependency_matches -n '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]' "$config_src/crew_defaults.rs" |
  grep -vE '[[:space:]]use[[:space:]]+(std|orbit_common|orbit_types)::|:use[[:space:]]+(std|orbit_common|orbit_types)::'; then
  echo "orbit-config crew_defaults must remain a std/orbit-common/orbit-types leaf"
  fail=1
fi

# Print every call in production code under application module `$2` (outside
# the `|`-separated path prefixes `$5`, relative to `$1`) to an inherent
# `OrbitRuntime` method that an `impl OrbitRuntime` block under one of the
# `|`-separated higher modules `$3` defines with a visibility that reaches
# `$2`; exit nonzero when one is found. The names are derived, so a new
# inherent method is covered without being listed. A call whose
# `<path>:<line>:<text>` matches the regex `$4` is a named exact-line
# allowance for a same-named method on another type (a store read).
# `$6` (default `$1`) is the directory `$3` is relative to, when the defining
# modules are not application modules: `$3` empty means all of it [ORB-22004].
# An entry of `$3` may name one source file rather than a module directory, and
# `$1` may be another layer under the crate source [ORB-22005].
inherent_calls_into() {
  python3 - "$1" "$2" "$3" "$4" "$5" "${6:-}" <<'PY'
import pathlib, re, sys
app = pathlib.Path(sys.argv[1]); module = sys.argv[2]
upper = sys.argv[3].split("|")
allowed = re.compile(sys.argv[4]) if len(sys.argv) > 4 and sys.argv[4] else None
exempt = [p for p in (sys.argv[5].split("|") if len(sys.argv) > 5 and sys.argv[5] else [])]
defs_root = pathlib.Path(sys.argv[6]) if len(sys.argv) > 6 and sys.argv[6] else app
def_prefix = list(defs_root.relative_to(app.parent).parts)
token = re.compile(
    r"(?P<line>//[^\n]*)|(?P<block>/\*)"
    r'|(?P<raw>(?:br|cr|r)(?P<hashes>\#*)")'
    r'|(?P<literal>(?:b|c)?"(?:\\[\s\S]|[^"\\])*"'
    r"|b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|[^\n])|[^'\\\n])')"
    r"|::|[a-zA-Z_][a-zA-Z_0-9]*|\S"
)
block_delimiter = re.compile(r"/\*|\*/")
def code_tokens(source):
    index = 0
    while match := token.search(source, index):
        index = match.end()
        if match.lastgroup in ("line", "literal"):
            continue
        if match.group("block") is not None:
            depth = 1
            while depth and (d := block_delimiter.search(source, index)):
                depth += 1 if d.group() == "/*" else -1
                index = d.end()
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
def production(root, base):
    for path in [root] if root.is_file() else sorted(root.rglob("*.rs")):
        rel = path.relative_to(base)
        if "tests" in rel.parts[:-1] or rel.name == "tests.rs":
            continue
        yield path, rel
def visibility(toks, i, parts):
    """The module path an item's `pub` restriction names, or None if private."""
    while i > 0 and toks[i - 1][0] in ("async", "const", "unsafe"):
        i -= 1
    if i > 0 and toks[i - 1][0] == "pub":
        return []
    if i > 0 and toks[i - 1][0] == ")":
        j = i - 1
        while j > 0 and toks[j][0] != "(":
            j -= 1
        if j > 0 and toks[j - 1][0] == "pub":
            inner = [t for t, _ in toks[j + 1:i - 1] if t != "::"]
            if inner == ["crate"]:
                return []
            if inner == ["super"]:
                return parts[:-1]
            if inner[:1] == ["in"] and inner[1:2] == ["crate"]:
                return inner[2:]
    return None
lower_path = [app.name, *([module] if module else [])]
names = set()
for top in upper:
    # A module is a directory or, like `tag_instructions`, a single file.
    top_root = defs_root / top
    if not top_root.exists() and (defs_root / f"{top}.rs").is_file():
        top_root = defs_root / f"{top}.rs"
    for path, rel in production(top_root, defs_root):
        parts = [*def_prefix, *rel.with_suffix("").parts]
        if parts[-1] == "mod":
            parts.pop()
        toks = list(code_tokens(path.read_text()))
        i = 0
        while i < len(toks):
            t = toks[i][0]
            j = i + 1
            if t == "impl":
                if j + 1 < len(toks) and toks[j][0] == "crate" and toks[j + 1][0] == "::":
                    j += 2
                if j + 1 < len(toks) and toks[j][0] == "OrbitRuntime" and toks[j + 1][0] == "{":
                    depth = 0
                    k = j + 1
                    while k < len(toks):
                        tk = toks[k][0]
                        if tk == "{":
                            depth += 1
                        elif tk == "}":
                            depth -= 1
                            if depth == 0:
                                break
                        elif tk == "fn" and depth == 1 and k + 1 < len(toks):
                            scope = visibility(toks, k, parts)
                            if scope is not None and lower_path[:len(scope)] == scope:
                                names.add(toks[k + 1][0])
                        k += 1
                    i = k
            i += 1
identifier = re.compile(r"[a-zA-Z_][a-zA-Z_0-9]*")
def qualified_type(toks, end):
    """The type that the `::` after `toks[end]` qualifies, or None.

    `Type::` and `<Type>::` (or `<path::Type>::`) name the type; `<Type>` is
    the qualified-self form. A turbofish (`Vec::<T>::`) names `Vec`, not `T`.
    """
    if end < 0:
        return None
    if toks[end][0] != ">":
        return toks[end][0]
    k = end - 1
    if k < 0 or not identifier.fullmatch(toks[k][0]):
        return None
    name = toks[k][0]
    while k >= 2 and toks[k - 1][0] == "::" and identifier.fullmatch(toks[k - 2][0]):
        k -= 2
    if k >= 1 and toks[k - 1][0] == "<" and not (k >= 2 and toks[k - 2][0] == "::"):
        return name
    return None
failed = False
for path, rel in production(app / module, app):
    if any(str(rel).startswith(e) for e in exempt):
        continue
    source = path.read_text()
    lines = source.splitlines()
    toks = list(code_tokens(source))
    for i, (t, start) in enumerate(toks):
        if t not in names or i + 1 >= len(toks) or i == 0 or toks[i - 1][0] == "fn":
            continue
        nxt = toks[i + 1][0]
        if nxt != "(" and not (nxt == "::" and i + 2 < len(toks) and toks[i + 2][0] == "<"):
            continue
        # An inherent method is reached by `receiver.name(`, `OrbitRuntime::name(`
        # (or `Self::`), or the qualified-self form `<OrbitRuntime>::name(` and
        # `<crate::OrbitRuntime>::name(` [ORB-22159]. A bare `name(` is a free
        # function, which the path checks already cover, so it is not an up-call.
        prev = toks[i - 1][0]
        if prev != "." and not (prev == "::" and qualified_type(toks, i - 2) in ("OrbitRuntime", "Self")):
            continue
        number = source.count("\n", 0, start) + 1
        hit = f"{rel}:{number}:{lines[number - 1]}"
        if allowed and allowed.search(hit):
            continue
        print(f"{path}:{number}:{lines[number - 1]}")
        failed = True
sys.exit(1 if failed else 0)
PY
}

# [ORB-22004] The `crate::adapter` grep above cannot see an inherent `OrbitRuntime`
# method, which carries no import. The adapter defines those on `impl OrbitRuntime`
# blocks; application reaches them only through the ports the adapter implements
# (`ToolInvocationPort`, `PullTransportPort`). The names are derived from the
# adapter's blocks, so a new adapter method is covered without being listed.
# Same-named methods on receivers that are not `OrbitRuntime`: the skill catalog's
# `doctor` and the plugin store's `list_plugins`. Exact lines only.
adapter_upcall_allowed='^health/artifact_diagnosis\.rs:[0-9]+:.*runtime\.skill_catalog\(\)\.doctor\(|^skill\.rs:[0-9]+:.*self\.skill_catalog\(\)\.doctor\(|^plugin/inspect/summary\.rs:[0-9]+:\s+\.list_plugins\(\)\?$|^plugin/secrets\.rs:[0-9]+:.*stores\(\)\.plugins\(\)\.list_plugins\('
if ! inherent_calls_into "$app_src" "" "" "$adapter_upcall_allowed" '' "$repo_root/crates/orbit-core/src/adapter"; then
  echo "orbit-core application must not call inherent OrbitRuntime methods defined under adapter; reach them through an application-owned port [ORB-22004]"
  fail=1
fi

# [ORB-22005] Composition is the only joiner, and `bootstrap -> application`
# stays because bootstrap seeds through application-owned definitions, so
# neither application nor the adapter may name bootstrap or composition or call
# an inherent `OrbitRuntime` method either defines. They reach them only through
# `contracts::TaskBundleMaintenancePort`.
# The names are derived from the `impl OrbitRuntime` blocks under `composition/`
# and under `bootstrap/`. Exact lines allowed: the lexical index store's own
# `reindex_tasks`, and the `#[cfg(test)]` fixture that opens a runtime.
core_src="$repo_root/crates/orbit-core/src"
joiner_upcall_allowed='^search/runtime\.rs:[0-9]+:.*lexical_index\(\)\.store\(\)\?\.reindex_tasks\(|^test_support\.rs:[0-9]+:.*OrbitRuntime::from_roots\('
for layer in application adapter; do
  if ! super_chains_into "$core_src/$layer" "" 'bootstrap|composition' crate-root ||
    ! inherent_calls_into "$core_src/$layer" "" 'composition|bootstrap' \
      "$joiner_upcall_allowed" '' "$core_src"; then
    echo "orbit-core ${layer} must not name bootstrap or composition or call their inherent OrbitRuntime methods; reach them through an application-owned port [ORB-22005]"
    fail=1
  fi
done

# Runtime is the lowest core layer. Composition supplies its application port;
# runtime must not name higher layers or call their inherent runtime methods.
if ! super_chains_into "$core_src/runtime" "" 'application|adapter|bootstrap|composition' crate-root ||
  ! inherent_calls_into "$core_src/runtime" "" 'application|adapter' '' '' "$core_src"; then
  echo "orbit-core runtime must not name higher layers or call application or adapter inherent OrbitRuntime methods"
  fail=1
fi

# [ORB-17924], [ORB-17943] `task` reaches the state scheduler and its bounded
# `Source` Git reader only through `task::TaskAutomationPort` and
# `task::SourceGit`, with no allowlist. A call to an inherent `OrbitRuntime`
# method that automation defines is the same edge by name [ORB-22035].
if dependency_matches -n 'crate::application::automation\b|(super::)+automation\b' \
  "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bautomation\b' \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  ! super_chains_into "$app_src" task automation ||
  ! inherent_calls_into "$app_src" task automation '' ''; then
  echo "orbit-core task must reach automation through task::TaskAutomationPort and must not call its inherent methods"
  fail=1
fi

# [ORB-17936] `task` is the lowest of the ten modules above contracts. It
# reaches `job`, `review`, `distributed`, `pull_drain`, `admission`,
# `sweep_filing` and `workspace_auto` only through the ports it owns
# (`TaskJobPort`, `TaskReviewPort`, `TaskDistributedPort`,
# `TaskSweepFilingPort`, `TaskWorkspaceAutoPort`), which those modules
# implement on `OrbitRuntime`. A path, a grouped tree, a `super::` chain that
# climbs out of `task` into one of them, or a call to an inherent method they
# define is a reintroduced edge [ORB-17972], [ORB-22003]. The names are
# derived from their `impl OrbitRuntime` blocks, so port methods use distinct
# names. `tag_instructions` sits outside the module order but above `task`
# [ORB-22289]: task reads commit-pinned files through its own
# `task::base_revision`, never through it. `backlog_forecast` is above
# `workspace_auto` and task has no port to it [ORB-22287]: only the two
# re-export lines below name it, a compatibility shim for `lib.rs` and
# `orbit-web`. `codebase` sits outside the order but above `task` [ORB-22625]:
# task owns the pure block classification (`task::classify_block`) and records
# a done landing's diff stats through `TaskCodebasePort`, which `codebase`
# implements on `OrbitRuntime`; `codebase` reads `distributed` itself.
# `resident` sits outside the order but above `task` [ORB-22955]: it drives
# task writes, so a decision answer applies a resident grant, and a task
# comment write admits a resident record and refuses one needing redaction,
# only through `TaskResidentPort`, which `resident` implements on
# `OrbitRuntime`. `experiment` sits outside the order but above `task`
# [ORB-23015]: it reads tasks and their crew source back, so task show's
# assignment readout and the provider-limit gate's experiment-pool check go
# only through `TaskExperimentPort`, which `experiment` implements on
# `OrbitRuntime`. `search` sits above `task` [ORB-23016]: it reads task
# candidates and re-exports task-owned selector matching; task must never
# reach search for that pure helper. `notice` sits outside the order but
# above `task` [ORB-23017]: it reads tasks for poster authority and
# linked-task status, so a transition into done closes linked notices only
# through `TaskNoticePort`, which `notice` implements on `OrbitRuntime` in
# `notice/close.rs` by delegating to the existing materializer. Tick repair,
# dedupe, revision checks and owner fencing stay in notice. `evolution` sits
# above `task` [ORB-23018]: it reads tasks and replays their comments, so task
# projects committed checkpoint comments only through `TaskEvolutionPort`,
# implemented in `evolution/operations.rs`. Ingestion and source-keyed replay
# stay in evolution, including post-commit error propagation.
task_upper_modules='search|experiment|job|review|distributed|pull_drain|admission|backlog_forecast|sweep_filing|workspace_auto|tag_instructions|codebase|resident|notice|evolution'
task_forecast_shim_allowed='/application/task/mod\.rs:[0-9]+:pub use crate::application::backlog_forecast::(BacklogForecastHistory|COMPUTE_BUDGET_ENV);$'
# Same-named methods on other types are not upper inherent methods; only
# these exact lines are allowed: store reads through `stores().jobs()` and
# `stores().tasks()`.
task_upper_store_allowed='^task/steering\.rs:[0-9]+:.*stores\(\)\.jobs\(\)\.read_run_state|^task/steering\.rs:[0-9]+:\s+\.read_run_state\(run_id\)|^task/steering\.rs:[0-9]+:\s+\.read_run_state\(&source\.run_id\)|^task/pilot/drain_promotion\.rs:[0-9]+:\s+\.read_run_state\(run_id\)|^task/external_merge/candidate\.rs:[0-9]+:\s+\.landing_start_requests\(\)'
task_upper_paths="$(dependency_matches -n "crate::application::(${task_upper_modules})\\b" \
  "$app_src/task" -g '*.rs' -g '!**/tests/**' | grep -Ev "$task_forecast_shim_allowed" || true)"
if { [[ -n "$task_upper_paths" ]] && printf '%s\n' "$task_upper_paths"; } ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${task_upper_modules})\\b" \
    "$app_src/task" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" task "$task_upper_modules" \
    "$task_upper_store_allowed" '' ||
  ! super_chains_into "$app_src" task "$task_upper_modules"; then
  echo "orbit-core task must reach job, review, distributed, pull_drain, admission, sweep_filing, workspace_auto, codebase, resident, experiment, notice and evolution through its task ports, must not route through tag_instructions or search, and names backlog_forecast only in its two task/mod.rs re-exports"
  fail=1
fi

# [ORB-17937] `job` reaches the distributed drain and pull settlement only
# through the ports it owns (`JobDrainPort`, `JobPullSettlementPort`), which
# `distributed` and `pull_drain` implement on `OrbitRuntime`; the shapes they
# share (`DrainClaimedLeaf`, `HostProviderHold`) are contracts. A path, a
# grouped tree, a `super::` chain out of `job` into either, or a call to an
# inherent method either defines (derived from their `impl OrbitRuntime`
# blocks [ORB-22003]) is a reintroduced edge.
job_drain_modules='distributed|pull_drain'
if dependency_matches -n "crate::application::(${job_drain_modules})\\b" \
  "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${job_drain_modules})\\b" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" job "$job_drain_modules" '' '' ||
  ! super_chains_into "$app_src" job "$job_drain_modules"; then
  echo "orbit-core job must reach distributed and pull_drain through job::JobDrainPort and job::JobPullSettlementPort"
  fail=1
fi

# [ORB-17938] `job` reaches review admission, state automation, backlog
# admission and the auto drain's concurrency mode and leaf settlement only
# through the ports it owns (`ReviewAdmissionHook`, `JobAutomationPort`,
# `JobAdmissionPort`, `JobAutoConcurrencyPort`, `JobDrainLeafPort`
# [ORB-23198]), which `review`, `automation`, `admission` and
# `workspace_auto` implement on `OrbitRuntime`; the job name and filing tags
# it shares with `review` and `sweep_filing` are contracts. A path, a grouped
# tree, a `super::` chain out of `job` into any of them, or a call to an
# inherent method they define (derived from their `impl OrbitRuntime`
# blocks [ORB-22003]) is a reintroduced edge. Their free functions are
# reachable only by the paths these checks refuse. `experiment` sits above
# `job` [ORB-23015]: crew admission, fresh claim routing, claimed-leaf crew
# installation and claimed-leaf capture validation reach it only through
# `JobExperimentPort`, and the pool they read is the
# `contracts::ExperimentCrewPool` value.
job_upper_modules='admission|experiment|automation|backlog_forecast|review|sweep_filing|workspace_auto'
if dependency_matches -n "crate::application::(${job_upper_modules})\\b" \
  "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U "crate::application::\\{[^}]*\\b(${job_upper_modules})\\b" \
    "$app_src/job" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" job "$job_upper_modules" '' '' ||
  ! super_chains_into "$app_src" job "$job_upper_modules"; then
  echo "orbit-core job must reach review, automation, admission, workspace_auto and experiment through its job ports"
  fail=1
fi

# [ORB-17939] Above `job`, each module names only the modules below it in
# `automation < review < distributed < pull_drain < admission <
# workspace_auto < backlog_forecast < sweep_filing`. The cut edges go through ports the lower
# module owns: `automation::AutomationReviewPort` (before-PR coverage and the
# reviewer pools, implemented by `review`), `review::ReviewDistributedPort`
# (the owner's ship contract and machine) and `distributed::
# DistributedAdmissionPort` (the executor census); automation's pilot
# reservations and review's handoff console take the owner's commit boundary
# and completion authority through `job::JobDrainPort`, and admission's
# backlog listing takes the live leaves' tasks through
# `task::TaskWorkspaceAutoPort`. The distributed mutation gate is a contract.
# A path, a grouped tree, a `super::` chain into a higher module, or a call
# to the inherent methods and functions those ports wrap is a reintroduced
# edge.

# Fail when production code under application module `$1` names one of the
# `|`-separated higher modules `$2` or calls one of the `|`-separated inherent
# methods and functions `$3` (empty for none).
guard_upper_layer() {
  local module="$1" higher="$2" calls="$3"
  if dependency_matches -n "crate::application::(${higher})\\b" \
    "$app_src/$module" -g '*.rs' -g '!**/tests/**' ||
    dependency_matches -n -U "crate::application::\\{[^}]*\\b(${higher})\\b" \
      "$app_src/$module" -g '*.rs' -g '!**/tests/**' ||
    { [[ -n "$calls" ]] && dependency_matches -n "\\b(${calls})\\s*(::<[^>]*>)?\\s*\\(" \
      "$app_src/$module" -g '*.rs' -g '!**/tests/**'; } ||
    ! super_chains_into "$app_src" "$module" "$higher"; then
    echo "orbit-core ${module} must reach ${higher//|/, } only through ports the lower module owns"
    fail=1
  fi
}
guard_upper_layer automation 'review|distributed|pull_drain|admission|workspace_auto|backlog_forecast|sweep_filing' \
  'configured_pool_members|review_pool_by_os|select_review_crew|review_crew_candidates|admission_boundary'
guard_upper_layer review 'distributed|pull_drain|admission|workspace_auto|backlog_forecast|sweep_filing' \
  'admission_boundary|owner_completion_authority|owner_ship_contract|distributed_owner_machine_id'
guard_upper_layer distributed 'pull_drain|admission|workspace_auto|backlog_forecast|sweep_filing' 'executor_census'
guard_upper_layer pull_drain 'admission|workspace_auto|backlog_forecast|sweep_filing' ''
guard_upper_layer admission 'workspace_auto|backlog_forecast|sweep_filing' 'read_live_leaf_runs'
guard_upper_layer workspace_auto 'backlog_forecast|sweep_filing' ''
guard_upper_layer backlog_forecast 'sweep_filing' ''

# [ORB-23334] `auto_tasks` sits above `automation`: its scheduler evaluates
# delivery and conditions through automation, its listing projection reads
# automation inspection and its enable path uses automation's preview.
# Automation reads the catalog, validated definitions, discovery paths,
# accepted history, open-instance dedupe and template/mint parameters only
# through `automation::AutomationAutoTaskPort`, which `auto_tasks` implements on
# `OrbitRuntime` in `auto_tasks/scheduler.rs`. A path, a grouped tree, a
# `super::` chain into `auto_tasks`, or a call to an inherent method it defines
# (names derived, UFCS included, so the port methods keep distinct names) is a
# reintroduced edge. The downward edges from `auto_tasks` stay legal.
if dependency_matches -n 'crate::application::auto_tasks\b' \
  "$app_src/automation" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bauto_tasks\b' \
    "$app_src/automation" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" automation auto_tasks '' '' ||
  ! super_chains_into "$app_src" automation auto_tasks; then
  echo "orbit-core automation must reach auto_tasks through automation::AutomationAutoTaskPort and must not call its inherent methods"
  fail=1
fi

# [ORB-22416] `experiment` sits outside the order, above `distributed`: its
# readout reads the claim receipt `distributed` owns, so the owner's capture of a
# claimed task's experiments reaches `distributed` only through
# `distributed::DistributedExperimentPort`, which `experiment` implements on
# `OrbitRuntime`. A path, a grouped tree, a `super::` chain into `experiment`,
# or a call to an inherent method it defines (names derived, so the port method
# keeps a distinct name) is a reintroduced edge. The reverse edge, `experiment`
# reading the claim receipt, stays legal.
if dependency_matches -n 'crate::application::experiment\b' \
  "$app_src/distributed" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bexperiment\b' \
    "$app_src/distributed" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" distributed experiment '' '' ||
  ! super_chains_into "$app_src" distributed experiment; then
  echo "orbit-core distributed must reach experiment through distributed::DistributedExperimentPort and must not call its inherent methods"
  fail=1
fi

# Experiment readout consumes agent facts; agents must not reach back into
# experiment by path or by an inherent OrbitRuntime method it owns.
if dependency_matches -n 'crate::application::experiment\b' \
  "$app_src/agents" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bexperiment\b' \
    "$app_src/agents" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" agents experiment '' '' ||
  ! super_chains_into "$app_src" agents experiment; then
  echo "orbit-core agents must not name experiment or call its inherent methods"
  fail=1
fi

# [ORB-23019] `experiment` sits above `tag_instructions`: its overlays read
# `LazyRevision`, `TagInstructionFailure` and `read_blob_at`, and call
# `claim_base_revision`. Tag instructions reach overlay policy only through
# `tag_instructions::TagInstructionExperimentPort`, which `experiment`
# implements on `OrbitRuntime`. `tag_instructions` is one file, so the caller
# scan names that file. A path, a grouped tree, a `super::` chain into
# `experiment`, or a call to an inherent method it defines (names derived, so
# the port method keeps a distinct name) is a reintroduced edge. The reverse
# edge, experiment reading tag-instruction helpers, stays legal.
if [[ ! -f "$app_src/tag_instructions.rs" ]]; then
  echo "dependency scan path missing: $app_src/tag_instructions.rs" >&2
  exit 1
fi
if dependency_matches -n 'crate::application::experiment\b' \
  "$app_src/tag_instructions.rs" ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bexperiment\b' \
    "$app_src/tag_instructions.rs" ||
  ! inherent_calls_into "$app_src" tag_instructions.rs experiment '' '' ||
  ! super_chains_into "$app_src" tag_instructions.rs experiment; then
  echo "orbit-core tag_instructions must reach experiment through tag_instructions::TagInstructionExperimentPort and must not call its inherent methods"
  fail=1
fi

# [ORB-22708] Landing sits above distributed. Task
# owns the pure handoff-to-claim read; review records authority and reaches
# dispatch only through its own ReviewLandingDispatchPort. Refuse paths,
# grouped trees, relative chains and calls into landing's inherent methods.
# These exact lines read the task store, whose method shares a runtime name.
landing_store_reads_allowed='^task/external_merge/candidate\.rs:[0-9]+:\s+\.landing_attempts\(\)$|^task/external_merge/rescued\.rs:[0-9]+:\s+if tasks\.landing_attempts\(\)\?\.iter\(\)\.any\(\|attempt\| \{$|^review/handoff\.rs:[0-9]+:\s+self\.stores\(\)\.tasks\(\)\.landing_attempts\(\)\?$'
for lower_module in task job review distributed; do
  if dependency_matches -n 'crate::application::landing\b' \
    "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    dependency_matches -n -U 'crate::application::\{[^}]*\blanding\b' \
      "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    ! inherent_calls_into "$app_src" "$lower_module" landing "$landing_store_reads_allowed" '' ||
    ! super_chains_into "$app_src" "$lower_module" landing; then
    echo "orbit-core ${lower_module} must not name landing or call its inherent methods; task owns handoff claim resolution, and review dispatches through review::ReviewLandingDispatchPort"
    fail=1
  fi
done

# [ORB-22623] `routines` (the sweep clock) sits outside the order, above
# `automation` and `auto_tasks`: it calls automation's evaluation and the
# auto-task scheduler, and reaches `review`, `pull_drain` and `backlog_forecast`.
# The clock settings and schedule display state those two read live in
# `contracts`. A path, a grouped tree, a `super::` chain into `routines`, or a
# call to an inherent method it defines (names derived) is a reintroduced edge.
# Resident reads the selected-checkout cycle switch through its own
# ResidentRoutineStatusPort, implemented by routines [ORB-22956], so the
# routines -> automation -> resident path has no back-edge into routines.
for lower_module in automation auto_tasks resident; do
  if dependency_matches -n 'crate::application::routines\b' \
    "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    dependency_matches -n -U 'crate::application::\{[^}]*\broutines\b' \
      "$app_src/$lower_module" -g '*.rs' -g '!**/tests/**' ||
    ! inherent_calls_into "$app_src" "$lower_module" routines '' '' ||
    ! super_chains_into "$app_src" "$lower_module" routines; then
    echo "orbit-core ${lower_module} must not name routines or call its inherent methods; use lower-owned contracts or resident::ResidentRoutineStatusPort"
    fail=1
  fi
done

# [ORB-22287] The forecast has no ports, so nothing below it may call its
# inherent methods (names derived); `task` and `job` are covered above, through
# their upper-module lists.
for lower_module in automation review distributed pull_drain admission workspace_auto; do
  if ! inherent_calls_into "$app_src" "$lower_module" backlog_forecast '' ''; then
    echo "orbit-core ${lower_module} must not call backlog_forecast's inherent methods"
    fail=1
  fi
done

# Selective-review retirement removed review's risk-model freeze and its
# codebase dependency. Keep that direction closed: review has no codebase
# exception, including calls indexed from codebase's inherent runtime methods.
if ! super_chains_into "$app_src" review codebase application-root ||
  ! inherent_calls_into "$app_src" review codebase '' ''; then
  echo "orbit-core review must not name codebase or call its inherent methods"
  fail=1
fi

# [ORB-22287] `codebase` sits outside the order. The forecast's one authorized
# edge into it is `runtime.task_riskiness_model()` in `assembly.rs`, whose
# returned model answers `forecast_regression_prior`. Any other path or inherent
# call into `codebase`, or any `codebase` path or call back into the forecast,
# is a new edge.
forecast_codebase_allowed='^backlog_forecast/assembly\.rs:[0-9]+:\s+let risk_model = runtime\.task_riskiness_model\(\)\?;$'
if dependency_matches -n 'crate::application::codebase\b' \
  "$app_src/backlog_forecast" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bcodebase\b' \
    "$app_src/backlog_forecast" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" backlog_forecast codebase "$forecast_codebase_allowed" '' ||
  ! super_chains_into "$app_src" backlog_forecast codebase; then
  echo "orbit-core backlog_forecast may reach codebase only through runtime.task_riskiness_model() in assembly.rs"
  fail=1
fi
if dependency_matches -n 'crate::application::backlog_forecast\b' \
  "$app_src/codebase" -g '*.rs' -g '!**/tests/**' ||
  dependency_matches -n -U 'crate::application::\{[^}]*\bbacklog_forecast\b' \
    "$app_src/codebase" -g '*.rs' -g '!**/tests/**' ||
  ! inherent_calls_into "$app_src" codebase backlog_forecast '' '' ||
  ! super_chains_into "$app_src" codebase backlog_forecast; then
  echo "orbit-core codebase must not name backlog_forecast or call its inherent methods"
  fail=1
fi

shared_contracts='DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED|PULL_DRAIN_JOB|DrainClaimedLeaf|ClaimedLeafStage|HostProviderHold|ESCALATION_TRIAGE_JOB|FINDING_VERIFICATION_JOB|DrainEntryPoint|PullSettlementEntry|PullSettlementOutcome|SUPERSEDED_BY_SOURCE|PilotFingerprints|review_layer_json|CreationCrewAssignment|CrewCandidate|InspectionPool|PURPOSE_SOURCE_PREFIX|PROVIDER_LIMIT_REASSIGNMENT_PREFIX|auto_task_minted|automatic_crew_source|RetryKey|LOCAL_ROUTE_JOB|CI_FAILURE_TAG|DEPENDABOT_TAG|CODE_SCANNING_TAG|RECONCILIATION_FOLLOW_UP_TAG_PREFIX|PublishedDelivery|PrecedentAssistState|PrecedentAssistOutcomeSource|PrecedentAssistOutcome|PrecedentAssistDemotion|PrecedentAssistEnd|PrecedentAssistEnablement|ClockSettings|DEFAULT_CLOCK_CADENCE_SECONDS|LAUNCHD_LABEL|SYSTEMD_UNIT|load_clock_settings|save_clock_settings|ScheduleDisplayState'
if dependency_matches -n "\\b(const|enum|struct|fn) (${shared_contracts})\\b" \
  "$repo_root/crates/orbit-core/src" -g '*.rs' -g "!**/application/contracts/**" ||
  dependency_matches -n "\\b(${app_modules})(::[a-z_]+)*::(${shared_contracts})\\b" \
    "$repo_root/crates" -g '*.rs' ||
  dependency_matches -n -U "\\b(${app_modules})(::[a-z_]+)*::\\{[^}]*\\b(${shared_contracts})\\b" \
    "$repo_root/crates" -g '*.rs'; then
  echo "shared application contracts are defined and imported only at application::contracts"
  fail=1
fi

if dependency_matches -n 'ResolvedConfig::load|ConfigRoots::' \
  "$repo_root/crates/orbit-runtime/src" \
  -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-runtime must consume resolved config supplied by composition"
  fail=1
fi

# orbit-store is intentionally one crate, but its internal persistence graph
# is directional: contracts and shared fs mechanics are leaves; each driver is
# isolated; only repositories, workflows, and composition may join drivers.
store_src="$repo_root/crates/orbit-store/src"
if dependency_matches -n 'crate::(driver|repository|workflow)|\brusqlite\b' \
  "$store_src/contracts" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store contracts must not import persistence implementations"
  fail=1
fi

if dependency_matches -n 'crate::(driver::sqlite|repository|workflow)|use crate::(Store|StoreTx)' \
  "$store_src/driver/file" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store file driver must not import SQLite, repositories, or workflows"
  fail=1
fi

if dependency_matches -n 'crate::(driver::file|repository|workflow)' \
  "$store_src/driver/sqlite" -g '*.rs' -g '!**/tests/**'; then
  echo "orbit-store SQLite driver must not import file drivers, repositories, or workflows"
  fail=1
fi

if dependency_matches -n 'crate::(driver|repository|workflow)' \
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
if dependency_matches -n 'automation_commit\(|Sha256' \
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
if dependency_matches -n 'automation_commit\(|Sha256|fn evaluate\b|fn exclusion\b|fn certificate_acceptable\b|fn task_meaning_digest\b|definition_epoch' \
  "$repo_root/crates/orbit-core/src/application/review" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "Core review must use orbit-automation's shared coverage rules and Store persistence"
  fail=1
fi

if dependency_matches -n 'ReviewStoreBackend|review_certificate_record|review_reserve' \
  "$repo_root/crates/orbit-automation/src" \
  --glob '*.rs' --glob '!**/tests/**'; then
  echo "orbit-automation must not persist review evidence; Store owns ledgers and certificates"
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "dependency direction guard passed"
