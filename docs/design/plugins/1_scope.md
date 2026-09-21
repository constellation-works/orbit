---
type: design
summary: "Scope: a plugin standard and contract for extending Orbit with tools, CLI groups, dashboard panels, routines, auto-tasks, activities, jobs and skills from one manifest"
tags: [plugins, tools, routines, auto-tasks, dashboard, cli]
last_validated: 2026-09-21
---

# Scope: Orbit plugin standard

Status: phases 1-3 landed (2026-09-21); phases 4-5 remain proposal.
Namespace, dashboard-feature, install and mirror questions resolved 2026-09-20.
Bearing: [Operations as data, not inherent methods](../orbit-core/4_decisions.md) (orbit-core ADR).
Precedents: `*.orbit-tool.yaml` sidecar manifests (orbit-graph ships three); the shelved
[docs + search pluginization](../orbit-docs-plugin/1_scope.md); orbit-research's
version/capability allowlist against the Orbit binary.

## Problem

Orbit has six data-driven definition kinds that all live as `.orbit/<kind>/*.yaml` with a
`schemaVersion` (routines, auto-tasks, activities, jobs, executors, policies), plus skills and
external tools. Yet a "plugin" today is three unrelated things:

- an **external tool**: one executable + one sidecar manifest per tool, registered into SQLite
  by `orbit tool add`, invoked unsandboxed, never advertised over MCP
  (`orbit-tools/src/external.rs`, `orbit-mcp/src/remote/surface.rs`);
- the **Claude Code plugin mirror** under `plugin/` (skills + hooks + `mcp.json`), synced one
  way from `crates/orbit-core/assets`;
- a **separate product** (orbit-research, orbit-graph) that talks to Orbit by shelling out to
  `orbit tool run` and pinning a compatibility table of Orbit versions.

Nothing lets one artifact say "I add these tools, this CLI group, these scheduled chores, this
dashboard panel and this skill", and nothing records what a plugin is *allowed* to do. Adding a
first-party surface still costs up to nine hand edits (the Config tab, ORB-12724).

## Goal

One manifest (`plugin.yaml`, `schemaVersion: 2`, superseding the v1 tool sidecar) that
declares every contribution a plugin makes (tools, CLI, dashboard panels, routines,
auto-tasks, activities, jobs, skills, config), one lifecycle (`orbit plugin add|enable|disable|
remove|list|show|doctor|validate|scaffold|sync`), one execution protocol, and one set of
invariants that Orbit enforces at load, enable and call time. Everything a plugin contributes
is **data plus one backend executable**: no Rust, no dashboard JavaScript.

### Non-goals for v1

- New **deterministic activity actions** in Rust (`DeterministicAction` stays closed). A
  plugin gets deterministic behaviour by exposing a tool and calling it from a new generic
  `plugin.tool_call` action (see §4.5). Agent-loop activities are already data.
- New **agent providers / executor types**. `ProviderRegistry` is `pub(crate)` with four
  parallel vocabularies; opening it is Phase 5, gated on a second concrete need.
- **Arbitrary dashboard code, or switching built-in dashboard features.** Dashboard assets are
  compiled in and served under the operator-session model; plugins get declarative panels and
  links, not scripts and not feature toggles.
- Marketplace/discovery. Sources are a path, a git URL or an archive.

## 1. Contribution model

| Contribution | Manifest key | Where it lands | Surfaces derived |
|---|---|---|---|
| Tools | `spec.tools[]` | `ToolRegistry` as `<ns>.<verb>`; MCP as `<ns>_<verb>` | tool host, MCP `tools/list`, `orbit tool run`, `orbit <ns> <verb>`, dashboard panel source |
| CLI group | derived from tools (`cli:` override) | `orbit <ns> …` | clap subcommand built from the tool's input schema |
| Activities / jobs | `spec.definitions.activities`, `.jobs` | catalog layer `plugin:<ns>` (below workspace, above shipped) | `job:<name>` routine targets, `orbit run job` |
| Routines | `spec.definitions.routines` | seeded to `.orbit/routines/<ns>-<name>.yaml`, `enabled: false` | clock tick |
| Auto-tasks | `spec.definitions.auto_tasks` | seeded to `.orbit/auto_tasks/<ns>-<name>.yaml`, `enabled: false` | clock tick, `orbit auto-task` |
| Skills | `spec.skills[]` | managed asset → `skill_link_roots` | Claude/Codex skill discovery |
| Config | `spec.config` | `[plugins.<ns>]` in `config.toml`, validated by the plugin's JSON Schema | `orbit config`, Config tab provenance |
| Dashboard | `spec.web.panels[]`, `.links[]` | generic panel renderer; link tiles | `/api/plugins/<ns>/…`, one `plugins` tab group |

The **namespace** `<ns>` is `metadata.name`. It owns: tool names `<ns>.*`, the CLI group
`orbit <ns>`, config `[plugins.<ns>]`, the provenance tag `plugin:<ns>` on seeded
definitions and minted tasks, and the catalog layer. Every built-in `Commands` variant is
reserved; a collision fails the load of that plugin only.

**`orbit.<ns>.*` is reserved for Orbit-originated plugins.** A manifest with
`metadata.publisher: constellation-works` and `metadata.origin: orbit` claims tool names
`orbit.<ns>.<verb>` (MCP `orbit_<ns>_<verb>`) while keeping `orbit <ns>` as its CLI group;
every other publisher gets bare `<ns>.*`. `origin: orbit` is only honoured for plugins whose
source resolves to a constellation-works repository or whose manifest digest is in the
bundled first-party list; any other manifest claiming it is refused at load. orbit-graph and
orbit-research are first-party, so `orbit.graph.*` keeps its current names with no alias table.

## 2. Manifest

```yaml
# plugin.yaml — at the plugin root
schemaVersion: 2
kind: Plugin
metadata:
  name: graph                     # namespace: graph.recommend, `orbit graph recommend`, [plugins.graph]
  version: 0.4.1
  description: Leakage-safe file/symbol recommendations from verified change history.
  publisher: constellation-works
  homepage: https://github.com/constellation-works/orbit-graph
spec:
  requires:
    orbit: ">=0.24.0 <1.0.0"        # semver range on the host binary
    host_api: 1                     # protocol major; mismatch refuses enable
    platforms: [linux, macos]
    programs: [git]                 # host programs the backend spawns (proc allowlist)

  backend:
    type: exec                      # exec | mcp
    command: bin/orbit-graph        # relative to plugin root, or absolute
    args: [orbit-tool]              # exec: tool name arrives in the request envelope
    timeout_ms: 30000
    sandbox: default                # default | none  ('none' needs the `unsandboxed` grant)

  permissions:                      # REQUESTED. Granted by the operator at `orbit plugin enable`.
    fs:
      read:  ["{{workspace}}"]
      write: ["{{workspace}}/.orbit-graph", "{{plugin_state}}"]
    network: none                   # none | loopback | any
    env_pass: []                    # names copied from the parent env (never values)
    orbit_tools: [orbit.task.show, orbit.search]   # callbacks the backend may make

  tools:
    - name: recommend               # canonical graph.recommend, MCP graph_recommend
      description: Recommend files or symbols for a task or free-text query.
      execution_kind: read_only     # read_only | mutating
      mcp_scope: workspace          # workspace | global | none
      input_schema:  { $ref: schemas/recommend.request.json }
      output_schema: { $ref: schemas/recommend.response.json }
      cli: { verb: recommend, positional: [query] }   # optional override of the derived clap shape
    - name: status
      execution_kind: read_only
      mcp_scope: workspace
      input_schema: { type: object, properties: { repository: { type: string } } }
    - name: maintain
      execution_kind: mutating
      mcp_scope: workspace
      input_schema: { $ref: schemas/maintain.request.json }

  definitions:
    activities: [definitions/activities/*.yaml]     # schemaVersion 2, kind: Activity (agent_loop or plugin.tool_call)
    jobs:       [definitions/jobs/*.yaml]
    routines:   [definitions/routines/*.yaml]       # seeded enabled:false
    auto_tasks: [definitions/auto_tasks/*.yaml]     # seeded enabled:false

  skills: [skills/graph]                            # dirs containing SKILL.md

  config:
    schema: schemas/config.json                     # validates [plugins.graph]
    defaults: { index_dir: ".orbit-graph" }

  web:
    panels:
      - id: status
        title: Graph index
        source: tool:status                          # read_only tools only
        render: kv                                   # kv | table | markdown | json
        group: diagnostics                           # diagnostics | operations | config
    links:
      - title: Graph explorer
        url: "http://127.0.0.1:{{config.explorer_port}}"

  tests: [tests/conformance/*.yaml]                  # request/response goldens run by `orbit plugin test`
```

Rules: `deny_unknown_fields` everywhere (same posture as `RoutineDefinition`); `$ref` resolves
only inside the plugin root; templates may use `{{workspace}}`, `{{plugin_root}}`,
`{{plugin_state}}`, `{{config.<key>}}` and nothing else; every path is canonicalised and must
stay inside the plugin root or the granted fs profile.

## 3. Lifecycle and state

```
orbit plugin add <path|git+url#ref|archive>   →  installed   (~/.orbit/plugins/<ns>/<version>/, `current` link)
orbit plugin enable <ns> [--grant fs,network,orbit_tools,unsandboxed] [--workspace]
                                              →  active      (tools Active; definitions seeded; skills linked)
orbit plugin disable <ns>                     →  installed   (tools Inactive; seeded definitions skipped with a warning)
orbit plugin remove <ns>                      →  gone        (derived data such as .orbit-graph/ is retained)
orbit plugin list | show <ns> | doctor | validate <dir> | test <dir> | scaffold <ns> | sync | migrate
```

**Workspace declares, host installs.** A committed `.orbit/plugins.yaml` pins what a
workspace uses:

```yaml
schemaVersion: 1
plugins:
  - name: graph
    version: "0.4.x"
    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1
    enabled: true
```

Install is global only: the plugin lives once per host under `~/.orbit/plugins/` and every
workspace on that host shares it. The repository commits only the pin file; plugin trees are
never vendored under `.orbit/`, so a `source:` pointing inside the repository is refused.
Cloning a repo therefore does not make its plugins available; `orbit plugin sync` reads the
pin file and installs or reports whatever the host is missing.

Grants, install paths, digests and enable state are **host-local** (SQLite `plugin_store`,
next to `tool_store`), never synced — the same split routines already make between the
versioned `enabled:` and host-local pauses. A workspace that pins a plugin the host has not
installed, or has installed without the requested grants, gets the plugin's tools registered
via `register_inactive` and one deduped diagnostic naming the missing step. Nothing else
degrades.

**Seeding follows the managed-asset rule.** Routines, auto-tasks and skills are written once
with `provenance: plugin:<ns>@<version>` and a digest in `.orbit-managed-assets.json`. A
plugin upgrade re-seeds only files whose digest still matches the previously shipped
version; a customised file gets a warning and a `--force` path, never a silent overwrite.
Activities and jobs are not copied at all: they form a catalog layer resolved
`workspace > plugin:<ns> > shipped`, so a workspace can shadow a plugin activity by name.

## 4. Contracts Orbit enforces

### 4.1 Placement, never permission

The manifest may say *where* a tool appears (`mcp_scope`, `execution_kind`, CLI shape). It
can never say *who* may call it. Plugin tools enter `GOVERNED_OPERATIONS` through a single
generic row per execution kind: `read_only` plugin tools are callable by `Agent | Operator |
Runner`; `mutating` plugin tools by `Operator | Runner` and by `Agent` only when the task's
`required_tools` or the activity's allowlist names them. The `permissions:` block is a
*request*; the `--grant` flags at enable time are the only source of authority, and
`orbit plugin show` prints requested vs granted side by side.

### 4.2 Execution protocol

`exec` backend — one process per call, current dir = caller cwd, env cleared to the
allowlisted child env plus:

```
ORBIT_HOST_API=1  ORBIT_VERSION=0.24.0  ORBIT_PLUGIN=graph  ORBIT_PLUGIN_ROOT=…  ORBIT_PLUGIN_STATE=…
ORBIT_TOOL_NAME=graph.recommend  ORBIT_WORKSPACE_ROOT=…  ORBIT_ALLOWED_TOOLS=orbit.task.show,orbit.search
```

stdin:  `{"schema_version":1,"tool":"graph.recommend","input":{…},"context":{"workspace_root":…,"agent":…,"model":…}}`
stdout: `{"ok":true,"output":{…}}` or `{"ok":false,"error":{"code":"…","message":"…","retryable":false}}`

Non-zero exit, non-JSON stdout, or output failing `output_schema` is a tool error; there is no
partial success. Timeout is `backend.timeout_ms`, capped by a host ceiling.

`mcp` backend — Orbit spawns the plugin's stdio MCP server once per runtime, keeps it alive,
proxies each `<ns>.<verb>` call as `tools/call`, and refuses to start if the server's
`tools/list` disagrees with the manifest's `tools:` (names and schemas). Orbit is the only
client; the plugin never listens on a socket. This is how orbit-research plugs in without a
rewrite.

Callbacks: the backend reaches Orbit only through `orbit tool run`, and only for tools listed
in `permissions.orbit_tools` *and* granted; the same allowlist is stamped into
`ORBIT_ALLOWED_TOOLS`. No socket, no shared store handle. `ORBIT_ALLOWED_TOOLS` is written
even when it is empty, so a child process that carries `ORBIT_PLUGIN` is recognised as a
callback context and an unlisted tool is refused by the CLI before it runs — the backend's
restraint is not the boundary.

As implemented, the child also carries `ORBIT_PLUGIN_VERSION`, `ORBIT_TOOL_CWD` and
`ORBIT_PROC_ALLOWED_PROGRAMS` (`requires.programs`). `ORBIT_TOOL_NAME` is absent for an `mcp`
child, which serves every tool of its plugin.

### 4.3 Sandboxing

External tools run unsandboxed today. Plugin backends run under the existing Landlock /
`sandbox-exec` machinery with the granted `fs` profile, `network` mode and `programs` list.
`backend.sandbox: none` requires the `unsandboxed` grant and is reported as a finding by
`orbit plugin doctor` and the dashboard reliability view.

**The mapping, as implemented.** A grant is a *request* until `orbit plugin enable --grant`
records it; the profile compiled here is the granted one, never the requested one, and a
plugin missing a required grant never reaches this point (§4.1).

| Manifest | Grant | Linux (`spawn_under_linux_landlock_boundary`) | macOS (`compile_macos_sandbox_profile` + `append_macos_network_access`) |
|---|---|---|---|
| (always) | — | The plugin root is readable and executable; the host runtime grants (`/usr`, the loader, resolver files, `PATH` directories, tool state) come from the same table activity-scoped `proc.spawn` uses | The compiler's own read allow plus its credential denies |
| `permissions.fs.read` | `fs` | Each rendered path as a read tree (directory) or read file | `(allow file-read* (subpath …))` via the profile's `read` rules |
| `permissions.fs.write` | `fs` | Each rendered path as a write tree; the ruleset handles every write-side right, so a path without a write grant is read-only to the child | `(allow file-write* (subpath …))` via the profile's `modify` rules |
| `permissions.network: none` (default) | — | `ACCESS_NET_BIND_TCP \| ACCESS_NET_CONNECT_TCP` handled with no rule, which refuses every TCP endpoint (needs Landlock ABI 4; an older kernel fails closed) | `(deny network*)` appended after the compiler's broad allow |
| `permissions.network: loopback` | `network` | TCP left open — Landlock has no address filter, and the design's confinement claim is the filesystem | `(deny network*)` then loopback re-allows |
| `permissions.network: any` | `network` | TCP left open | the compiler's `(allow network*)` stands |
| `permissions.orbit_tools` | `orbit_tools` | Orbit's own global root and the workspace's `.orbit/` become writable, because a callback *is* `orbit tool run`; what that call may do is decided by `ORBIT_ALLOWED_TOOLS` and the ordinary governed-operation rows, not by the sandbox | same, through the profile's `modify` rules |
| `permissions.env_pass` | `env_pass` | Those names are copied from Orbit's environment into the otherwise allowlisted child environment | same |
| `requires.programs` | — | Not a sandbox rule: the declared programs are checked against a restricted caller's own `proc.spawn` allowlist and stamped into `ORBIT_PROC_ALLOWED_PROGRAMS` | same |
| `backend.sandbox: none` | `unsandboxed` | No ruleset at all | No `sandbox-exec` wrapper at all |

Granted write roots are created before the child starts: a Landlock rule binds to an inode,
so a grant naming a directory that does not exist yet would otherwise grant nothing. A host
that can enforce neither backend refuses to run the plugin rather than running it unconfined;
`unsandboxed` is the only opt-out and it is a `doctor` finding.

### 4.4 Audit and provenance

Every call passes through the existing audited dispatch with `ToolEntryPoint` plus
`plugin: {name, version, manifest_digest}`. Tasks minted by a plugin auto-task carry
`plugin:<ns>` alongside the existing `auto-task:<name>` tag. Definitions seeded by a plugin
carry the provenance header. Removing a plugin never rewrites task history.

### 4.5 Definitions

- Routines target `job:<name>` only, as today; a plugin routine may only target a job the same
  plugin ships or a shipped default. Cross-plugin targets are a load error.
- Plugin jobs may reference shipped activities and their own activities.
- Plugin activities are `agent_loop`, or `deterministic` with the one new action
  `plugin.tool_call { tool: <ns>.<verb>, input: {…} }`. This is the only addition to the
  closed action enums and it is what lets a routine drive a plugin without Rust.
- Seeded routines and auto-tasks are `enabled: false`. Turning one on is the same reviewed,
  versioned edit as for a shipped default. A plugin cannot enable its own schedule.
- When a plugin is disabled, its seeded definitions are skipped through the retired-routine
  reconciliation path with a warning naming the plugin, not treated as load errors.

### 4.6 CLI

`orbit <ns> <verb> [--flag …]` is generated from each tool's `input_schema` (top-level object
properties become `--kebab-case` flags; `cli.positional` promotes named properties; nested
objects take `--<name>-json`). `--input '<json>'` and `--input-file` are always accepted, so
`orbit graph recommend --query …` and `orbit tool run graph.recommend --input …` are the
same audited operation. There is no `git-foo` style passthrough: an unknown `orbit <word>`
still errors, and plugin CLI never bypasses dispatch, dry-run or audit.

### 4.7 Dashboard

Panels are rendered by one generic renderer from a `read_only` tool's JSON: `kv`, `table`
(array of objects), `markdown` (a string field), `json`. `/api/plugins/<ns>/panels/<id>` is
served to any session for `read_only` sources and never for mutating tools. `links` are
plain tiles to plugin-hosted UIs (loopback by default). Plugins cannot introduce tabs or
switch built-in dashboard features on; that idea is deferred past v1. The tab arrays in
`router.js` gain one `plugins` group fed from `/api/plugins`, so new plugins need zero
frontend edits.

### 4.8 Compatibility

- `requires.host_api` major must equal the host's; Orbit keeps the previous major for two
  minor releases and prints a deprecation in `doctor`.
- `requires.orbit` is checked on `enable`, and again at runtime build; a mismatch after an
  upgrade flips the plugin to Inactive with a diagnostic rather than failing the runtime. This
  honours the constellation rule that upgrades must not strand running clients.
- v1 `*.orbit-tool.yaml` sidecars and `orbit tool add` keep working unchanged;
  `orbit plugin migrate <binary>` writes a v2 manifest from a set of sidecars.

### 4.9 Fail closed, isolate blast radius

An invalid manifest, an unresolvable `$ref`, a schema that rejects its own `defaults`, a
missing binary or a collision refuses **that plugin** at load, records one diagnostic, and
leaves every other plugin and all built-ins untouched.

## 5. Conformance

`orbit plugin validate <dir>` — manifest schema, path containment, namespace collisions,
schema self-consistency, definition cross-references.
`orbit plugin test <dir>` — runs `spec.tests` goldens through the real protocol in a temp
workspace with the requested grants; a plugin is "certified for 0.x" when this passes on the
target Orbit and is recorded in `orbit plugin show`.
`orbit plugin scaffold <ns>` — replaces `orbit tool scaffold`: a Python `exec` backend, one
tool, one panel, one disabled auto-task, one skill stub, a passing conformance test.

## 6. What opens up in the codebase

| Today (closed) | Change |
|---|---|
| `register_builtins()` literal list; external tools loaded via plain `register()` | `PluginLoader` registers each manifest tool with `register_mcp(scope)` / `register_inactive` |
| `canonical_mcp_tool_definitions()` memoised in a `OnceLock`, external tools never in `tools/list` | MCP surface reads the registry; plugin tools advertised with their scope |
| `ExternalTool` unsandboxed, 15 s, no output validation | `PluginBackend::{Exec,Mcp}` with sandbox profile, schema validation, versioned envelope *(done)* |
| `Commands` enum only | one `Plugin(PluginGroupArgs)` variant that builds clap subcommands from loaded manifests |
| `define_config_settings!` closed | dynamic `plugins.<ns>.<key>` admission validated by the plugin's JSON Schema, provenance-aware *(done)* |
| `GOVERNED_OPERATIONS` per-op rows | two generic plugin rows keyed on `execution_kind` |
| `DeterministicAction` closed | `plugin.tool_call` *(done)* |
| `RETIRED_ROUTINE_JOBS` only skip path | provenance-aware skip for disabled plugins *(done)* |
| `DEFAULT_*_FILES` + `skill_link_roots` | managed-asset reconciliation accepts a plugin layer with `plugin:<ns>@<version>` provenance *(done: plugin-seeded definitions are tracked in their own `.orbit-managed-plugin-assets.json` so a plugin's entries never retire a shipped default)* |
| `TABS` arrays + per-asset `include_str!` | one `plugins` group + one generic panel renderer |

## 7. Phases (each its own PR into agent-main)

1. **Manifest + lifecycle over the existing tool path.** *Landed [ORB-12735].* `plugin.yaml`
   v2, `orbit plugin add|enable|disable|remove|list|show|validate|migrate`, `plugin_store`,
   tools registered with MCP scope and reaching `tools/list`. orbit-graph migrates from three
   sidecars to one manifest. Goldens for `orbit tool run` unchanged.
2. **Grants + sandboxed execution + `mcp` backend.** *Landed [ORB-12736].* `permissions`/
   `--grant` enforced at load and at the call, envelope v1, output-schema validation,
   Landlock/sandbox-exec profile (§4.3), callback allowlist, MCP proxy. orbit-research can
   plug in without a rewrite.
3. **Definitions + skills + config.** *Landed [ORB-12737].* Catalog layer, seeding with
   provenance, `plugin.tool_call`, `[plugins.<ns>]` with schema validation and
   Config-tab provenance. As implemented, the activity/job layer loads after the
   workspace *and* the shipped defaults, so a workspace file shadows a plugin's and
   L-0060's rule that a shipped default is never displaced still holds;
   `orbit run show` prints the resolving layer and what it shadowed. The seeded
   provenance is a `# provenance: plugin:<ns>@<version>` header comment rather than a
   field, because `RoutineDefinition` and `AutoTaskDefinition` are
   `deny_unknown_fields`. A `[plugins.<ns>]` value the plugin's schema rejects refuses
   that plugin at load, naming the key, and leaves the runtime standing (§4.9).
4. **Derived CLI + dashboard panels.** `orbit <ns> <verb>`, `/api/plugins`, generic renderer,
   `plugins` tab group, `orbit plugin test` and `scaffold`.
5. **Provider plugins** (deferred): open `ProviderRegistry` and collapse the four provider
   vocabularies, only when a second external provider exists.

## 8. Risks

- **Grant fatigue.** If `enable` demands a flag per permission nobody will read them. Default
  to a summary prompt with `--grant all` for interactive use and explicit flags for scripts.
- **Schema-derived CLI ergonomics.** Nested inputs make poor flags; `cli:` overrides and
  `--input` cover the long tail. Do not attempt full clap parity.
- **Catalog shadowing surprises.** Workspace-over-plugin precedence is right, but `orbit run
  show` must print which layer resolved each `activity:` ref.
- **`mcp` backend lifetime.** One long-lived child per runtime process means `orbit mcp
  serve`, `clock tick` and the CLI each spawn their own; acceptable in v1, pool later.
- **Compat table drift.** orbit-research keeps its own Orbit allowlist today; after Phase 2 the
  single source is `requires.orbit` plus the conformance run, and its table should be retired.

## Resolved questions (2026-09-20)

1. **Namespace.** `orbit.<ns>.*` is reserved for Orbit-originated plugins (§1); third parties
   get bare `<ns>.*`.
2. **Dashboard features.** No `web.features` in v1; panels and links only.
3. **Install model.** Global install per host with a committed `.orbit/plugins.yaml` pin.
   No vendoring (§3).
4. **Claude Code mirror.** `plugin/` stays the first-party mirror synced from
   `crates/orbit-core/assets`; it is not generated from installed plugins.
