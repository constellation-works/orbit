---
type: design
summary: "Scope: a plugin standard and contract for extending Orbit with tools, CLI groups, dashboard panels, routines, auto-tasks, activities, jobs and skills from one manifest"
tags: [plugins, tools, routines, auto-tasks, dashboard, cli]
last_validated: 2026-09-22
---

# Scope: Orbit plugin standard

Status: phases 1-4 landed; phase 5 remains proposal.
Namespace, dashboard-feature, install and mirror questions resolved 2026-09-20.
Bearing: [Operations as data, not inherent methods](../orbit-core/4_decisions.md) (orbit-core ADR).
Precedents: `*.orbit-tool.yaml` sidecar manifests (orbit-graph still ships three; its migration
to `plugin.yaml` remains pending); the shelved
docs + search pluginization proposal; orbit-research's
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
| Skills | `spec.skills[]` | install directory → `skill_link_roots` as `<ns>-<directory-name>` | Claude/Codex skill discovery |
| Config | `spec.config` | `[plugins.<ns>]` in `config.toml`, validated by the plugin's JSON Schema | `orbit config`, Config tab provenance |
| Dashboard | `spec.web.panels[]`, `.links[]` | generic panel renderer; link tiles | `/api/plugins/<ns>/…`, one `plugins` tab group |

The **namespace** `<ns>` is `metadata.name`. It owns: tool names `<ns>.*`, the CLI group
`orbit <ns>`, config `[plugins.<ns>]`, the provenance tag `plugin:<ns>` on seeded
definitions and minted tasks, and the catalog layer. Every built-in `Commands` variant is
reserved; a collision fails the load of that plugin only.

Plugin skill discovery IDs follow the same ownership rule: a declared `skills/graph` directory
from namespace `acme` is linked as `acme-graph`, never as `graph`. `orbit plugin validate`
reports every derived discovery ID before installation. Linking may repair an older-version
target from the same plugin install family, but refuses to replace a same-named path or link
owned by shipped skills, another plugin, or the user.

**`orbit.<ns>.*` is reserved for Orbit-originated plugins.** A manifest with
`metadata.publisher: constellation-works` and `metadata.origin: orbit` claims tool names
`orbit.<ns>.<verb>` (MCP `orbit_<ns>_<verb>`) while keeping `orbit <ns>` as its CLI group;
every other publisher gets bare `<ns>.*`. `origin: orbit` is only honoured when Orbit fetched
the source from a `git+` URL under `github.com/constellation-works`, or when its manifest digest
is in the bundled first-party list; a local directory's Git remotes are never provenance
evidence. Any other manifest claiming it is refused at load. orbit-graph and orbit-research are
first-party when installed from a verified source (or when a release bundles their manifest
digest), so they may use `orbit.graph.*` and `orbit.research.*` without an alias table. Migrating
local orbit-graph sidecars does not establish that provenance: migration emits no `publisher` or
`origin`, and the generated manifest therefore registers the bare `graph.*` namespace. To reclaim
`orbit.graph.*`, its author must add `publisher: constellation-works` and `origin: orbit`, publish
the tree under that GitHub organisation, and install it through the corresponding `git+` URL (or
ship a manifest digest in Orbit's bundled first-party list).

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
        refresh_ms: 30000                            # optional; 1000..3600000
    links:
      - title: Graph explorer
        url: "http://127.0.0.1:{{config.explorer_port}}"

  tests: [tests/conformance/*.yaml]                  # request/response goldens run by `orbit plugin test`
```

Rules: `deny_unknown_fields` everywhere (same posture as `RoutineDefinition`); `$ref` resolves
only inside the plugin root; templates may use `{{workspace}}`, `{{plugin_root}}`,
`{{plugin_state}}`, `{{config.<key>}}` and nothing else; every path is canonicalised and must
stay inside the plugin root or the granted fs profile. Filesystem permission templates render
against the effective `[plugins.<ns>]` values (workspace configuration over global configuration
over manifest defaults), stringifying non-string JSON scalars. A rendered relative filesystem
root is relative to the plugin root in validation, registration, conformance and call-time
sandboxing; it never inherits Orbit's process working directory.

## 3. Lifecycle and state

```
orbit plugin add <path|git+url#ref|archive>   →  installed   (~/.orbit/plugins/<ns>/<version>/)
orbit plugin upgrade <ns> [source] [--grant …]
                                              →  upgraded    (permission diff printed; widening requires re-consent)
orbit plugin enable <ns> [--grant fs,network,orbit_tools,unsandboxed] [--workspace]
                                              →  active      (tools Active; definitions seeded; skills linked)
orbit plugin disable <ns>                     →  installed   (tools Inactive; seeded definitions skipped with a warning)
orbit plugin remove <ns> --yes                →  gone        (derived data such as .orbit-graph/ is retained)
orbit plugin remove <ns> --yes --record-only  →  gone        (record only; every installed file is left in place)
orbit plugin list | show <ns> | doctor | validate <dir> | test <dir> | scaffold <ns> | sync | migrate
```

Enable creates only the namespaced skill links reported by `plugin validate`. Discovery roots
are siblings of the active global root: `~/.orbit` uses `~/.agents/skills` and
`~/.claude/skills`, while `--root /path/to/root` uses `/path/to/.agents/skills` and
`/path/to/.claude/skills`. Disable removes only discovery links whose targets are inside the
active global root's `plugins/<ns>/`; shipped, user-owned and other plugins' links remain
untouched.

**One version directory per namespace, swapped whole.** The host row in `plugins` is the only
authority for where a plugin lives: its `install_path` is what the loader reads, what every
lifecycle verb verifies, and what the sandbox profile is built from. There is deliberately no
`current` link beside the version directories — nothing read it, and a second name for the
install is a second thing that can drift from the row. An earlier Orbit wrote one beside the
version directories; the next `add` prunes it, and `remove` takes it with the namespace.

`add` copies the source into a staging directory inside `~/.orbit/plugins/<ns>/` and makes it
visible with a single `rename`, so a concurrent `orbit` — a clock tick, an MCP server, a
dashboard panel — never loads a `plugin.yaml` from a tree whose backend is still being
written. Replacing a tree renames the old one aside first, because `rename` cannot replace a
non-empty directory: for that moment `<version>/` does not exist, and a reader there gets a
plain "not installed" error rather than half a plugin. The swap is rolled back if anything
before the row is written fails, so a failed install never leaves a tree with no row — which
the next `add` would refuse without `--force`. Once the row names the new tree, everything
else under `~/.orbit/plugins/<ns>/` is unreferenced and is pruned: the version the upgrade
replaced, a stale `current`, and scratch a crashed install left. Old version trees are
readable to every plugin backend (§4.3), so keeping them would leave code on the host that no
row admits to. `remove` deletes the whole `~/.orbit/plugins/<ns>/` family for the same reason,
after the same install-path verification; `--record-only` still leaves every file in place.

**Every verb that touches the recorded tree checks it first.** The `install_path` in the
`plugins` row is writable by any backend holding `orbit_tools`, so `enable`, `disable` and
`remove` refuse a row that does not resolve beneath `~/.orbit/plugins/<ns>/` — the same check
the loader applies — before they seed from, unlink by, or delete it. The refusal names the
recorded and the expected path and leaves the row intact, because the operator's recovery is
`orbit plugin add` (reinstall) or `orbit plugin remove <ns> --yes --record-only`, which drops
this host's record and never touches the recorded path.

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
Git sources accept only `git+https://…`, `git+ssh://…`, and SCP-style
`git+git@host:path` repository URLs. Other Git transports, URL-shaped options, and refs
beginning with `-` are refused with the complete source entry before Git is spawned. Clones
disable user-selected protocols and terminal prompts, permit only HTTPS and SSH transports,
and terminate Git option parsing before the repository URL.
A directory, `git+` clone, or archive that contains a symbolic link is refused before the
tree is copied into the install root, naming the entry; `load_plugin_dir` applies the same
walk so a hand-edited install cannot become active. Following a link at copy time would
materialise the target's bytes inside the install root, which every backend may read.
Cloning a repo therefore does not make its plugins available; `orbit plugin sync` reads the
pin file and converges this host and workspace. It installs missing plugins, applies
`enabled: false` by disabling an enabled host row, and applies `enabled: true` only after
permission review when the manifest requests grants. Pass the complete reviewed set with
`--grant`; a committed pin is never grant consent. For an already-enabled host plugin, sync
also seeds or refreshes that plugin's routines and auto-tasks in the current workspace.
Before installing a missing pin, sync validates that the resolved source manifest declares the
pinned namespace and, when the pin includes a version requirement, that its version satisfies
the requirement. A mismatch is reported as unsatisfied without writing an install tree or host
row, enabling the plugin, linking its skills, or seeding its definitions; sync still continues
with unrelated pins.

Grants, install paths, digests and enable state are **host-local** (SQLite `plugin_store`,
next to `tool_store`), never copied into the repository. The pin's versioned `enabled:` value
is a convergence instruction, so syncing a different workspace may change that shared host
toggle; grants still require explicit host consent. A workspace that pins a plugin the host
has not installed, or has installed without the requested grants, gets the plugin's tools
registered via `register_inactive` and one deduped diagnostic naming the missing step.
Nothing else degrades.

**The recorded grant set is tamper-evident.** `orbit plugin enable` and an upgrading
`--grant` also write an integrity value over the set they authorized to
`~/.orbit/plugins/.grants/<ns>.json`, and the loader
refuses a `plugins` row whose grants do not match it — no tools, a `doctor` finding, and a
`denied` audit row per load pass. The two halves sit on opposite sides of boundaries the
sandbox enforces: a backend holding `orbit_tools` can write `orbit.db`, because `orbit tool
run` cannot start without it, but that grant never opens `plugins/` for writing, and an `fs`
grant may write beneath the global root only inside that plugin's `{{plugin_state}}` tree
(§4.3). The value
is `sha256("orbit.plugin.grants.v1\n<ns>\n<enabled|disabled>\n<sorted grants>")` — a plain
digest, not a MAC: a keyed value would need a secret the child cannot read, and that child
reads the whole global root. What bounds an attacker is the write boundary, not a secret
[ORB-12778]. A row whose grants are non-empty with no such record is refused, so a host
upgrading past this change re-runs `orbit plugin enable <ns> --grant …` once per granted
plugin; the records are deliberately not back-filled from existing rows, which would
authorize a row that may already have been written by a plugin.
When a manifest's request widens without re-consent, the installer overwrites this with a
disabled, empty-grant witness before replacing the row, so the old enabled witness cannot be
replayed against the new manifest through a database rewrite.

**The row's install path is held to the install root.** The witness covers `name`,
`enabled` and the grant set, and nothing else on the row — binding the version or the
manifest digest would refuse every legitimate upgrade. That leaves `install_path`: a row
that keeps its authorized grant names and repoints them at a tree the backend wrote under
one of its own write roots passes the witness unchanged, and the `manifest_digest` check
then compares the attacker's tree against the attacker's digest. So every enabled row's
`install_path` must resolve strictly beneath `~/.orbit/plugins/<ns>/` before anything is
read from it — a `denied` audit row and one diagnostic naming the recorded and the expected
path otherwise, and the same check on every `orbit_tools` callback, because a backend
already running can rewrite its row without a reload [ORB-12785]. Resolution is physical
where the path exists, so a `..` or a symlink cannot name a tree it does not live in.

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
*request*; `--grant` on an enabling add, enable, upgrade or sync is the only source of authority, and
`orbit plugin show` prints requested vs granted side by side. A stored grant set the host cannot
verify against its authorization record is not authority either: the plugin is refused and
every surface reports it as granting nothing (§3).

An explicit grant list is the complete set the operator authorizes, not an addition to the
stored row: `orbit plugin add --enable --grant …`, `orbit plugin enable --grant …` and a
grant-consenting `orbit plugin sync --grant …` replace the recorded set. Omitting `--grant` on
`plugin enable` preserves the recorded grants,
so disable followed by an ordinary re-enable does not require restating them. Supplying a
narrower list is the supported way to revoke grants down to what remains, and `--grant none` is
the way to revoke all of them: it records an explicit empty set rather than being read as
"nothing supplied, preserve what's there". `--grant all` and `--grant requested` are the other
two reserved spellings `orbit plugin enable` accepts in place of a literal list: every grant this
build knows, or exactly what the manifest's `permissions`/`backend.sandbox` ask for. Any of the
three replaces the authorization witness the same way a literal list does, and none can be mixed
into one.

A `plugins` row's grants are validated once, whether they arrive as a fresh `--grant` list or are
read back out of storage at load: a name no current grant recognizes — retired, renamed, or
written by a newer Orbit — refuses the row rather than being dropped and running the plugin under
a narrower set than what was authorized.

`orbit plugin add --grant …` is rejected unless `--enable` is also present. On a manifest
digest change, `add` and `upgrade` compare the old and new filesystem roots, network mode,
environment names, Orbit-tool allowlist and sandbox mode. An unchanged or narrower request
keeps the existing enable/grant state. Any addition or stronger network/sandbox request disables
the plugin, clears its grants and its old authorization witness, prints the widened requests,
and names the complete `orbit plugin enable <ns> --grant …` re-consent command. `plugin upgrade
<ns> [source]` defaults to the recorded source and always prints the permission diff; supplying
its own `--grant …` is explicit re-consent and enables the upgraded manifest.

Those grants are bound to the install-time `plugin.yaml` digest stored on the `plugins` row.
Every load hashes the bytes on disk and compares them to that `manifest_digest`. A mismatch
registers the plugin inactive with a diagnostic that names both digests and the re-consent
commands. Recovery first reinstalls the original source with `orbit plugin add <source> --force`,
then records consent with `orbit plugin enable <ns> --grant …`; `--force` alone cannot recover a
tree when that source is no longer available. The profile compiled for a call is the one the
operator granted, never a later rewrite of the requested paths, env names, tools, or backend.
That comparison means something only because the row's `install_path` is
checked against `~/.orbit/plugins/<ns>/` first (§3): both sides of the digest comparison come
off a row a backend can write, so what makes the stored digest evidence is that the bytes it
is compared against sit at a path the backend cannot populate [ORB-12785]. Independently of
that digest check, every rendered `permissions.fs.write` root is normalized before admission.
Normalization is physical: the longest existing prefix is resolved by the kernel and the names
that do not exist yet are appended to it, so a root whose tail is absent is still judged where
its existing ancestors live. A backend with a writable `{{plugin_state}}` can plant a symbolic
link there between two calls, and reading such a root by name would let
`{{plugin_state}}/alias/9.0.0` pass as plugin state while it materialises a new version tree
inside the plugin's own protected install namespace — which the `install_path` check above then
accepts, because the row would point at a tree beneath `~/.orbit/plugins/<ns>/` [ORB-12799].
A root that contains the plugin install tree or Orbit's global root is refused, and any root
*beneath* the global root is also refused unless it is inside that plugin's own
`{{plugin_state}}` tree. The same rule runs at `orbit plugin validate`, registration and call
time. A root that contains or lies inside the selected workspace's `.orbit` or `.git`
metadata is refused too; the comparison is by path component, so a sibling such as
`{{workspace}}/.orbit-graph` remains valid. Validation and registration enforce that
workspace-relative rule against a synthetic root, then call time repeats it against the real
workspace. Thus an `fs` grant cannot reopen `bin/`, `plugins/.grants`, another plugin's install
tree, workspace control files, Git hooks or another protected global path.

### 4.2 Execution protocol

`exec` backend — one process per call, current dir = caller cwd, env cleared to the
allowlisted child env plus:

```
ORBIT_HOST_API=1  ORBIT_VERSION=0.24.0  ORBIT_PLUGIN=graph  ORBIT_PLUGIN_ROOT=…  ORBIT_PLUGIN_STATE=…
ORBIT_PLUGIN_CALLBACK=<host-issued token>  ORBIT_TOOL_NAME=graph.recommend  ORBIT_WORKSPACE_ROOT=…
ORBIT_ALLOWED_TOOLS=orbit.task.show,orbit.search
```

stdin:  `{"schema_version":1,"tool":"graph.recommend","input":{…},"context":{"workspace_root":…,"agent":…,"model":…}}`
stdout: `{"ok":true,"output":{…}}` or `{"ok":false,"error":{"code":"…","message":"…","retryable":false}}`

Non-zero exit, non-JSON stdout, or output failing `output_schema` is a tool error; there is no
partial success. Timeout is `backend.timeout_ms`, capped by a host ceiling.

`mcp` backend — Orbit spawns the plugin's stdio MCP server once per *caller context* per
runtime, keeps it alive, proxies each `<ns>.<verb>` call as `tools/call`, and refuses to
start if the server's `tools/list` disagrees with the manifest's `tools:` (names and
schemas). Orbit is the only client; the plugin never listens on a socket. This is how
orbit-research plugs in without a rewrite.

**The caller context is the workspace *and* the allowed-tools intersection.** A child is
bound to a workspace three times over — its working directory, its `ORBIT_WORKSPACE_ROOT`,
and the write roots its sandbox profile renders from `{{workspace}}` (§4.3) — and all three
come from whichever caller spawned it. A runtime process serves several workspaces (`orbit
clock tick`, `orbit mcp serve`), so a session keyed by the intersection alone proxies
workspace B's call to a child confined to A: the plugin mutates A's state for a request
about B, or fails `EACCES` [ORB-12820]. Both halves of the context are therefore part of the
key, and a caller that differs in either gets its own child rather than inheriting another
caller's workspace or `ORBIT_ALLOWED_TOOLS`.

**Per-call context travels on the request.** A shared child cannot be told in its
environment which caller each call is for, so `tools/call` carries what the `exec`
envelope's `context` carries, under `params._meta.orbit`:

```
{"name":"recommend","arguments":{…},
 "_meta":{"orbit":{"workspace_root":…,"agent":…,"model":…,"tool":"graph.recommend"}}}
```

`tool` is there because one `mcp` child serves every tool of its plugin and so has no
`ORBIT_TOOL_NAME`; `workspace_root` is there because the child's own variable names the
workspace its *session* is bound to, which the backend should not have to infer is also
this call's.

**Orbit answers the server's own requests.** MCP is bidirectional: a server may send `ping`
or `roots/list` with an id of its own, including before it answers the `tools/call` it is
working on. A client that skips every message that is not its own response never replies,
and a server waiting on that reply is read as unresponsive — killed at the deadline and
respawned on the next call [ORB-12820]. Orbit answers `ping` with `{}` and every other
server-initiated request with JSON-RPC `-32601`, which is honest: it declares no
capabilities in `initialize`. An answer, including a refusal, is what keeps the call moving.

**One lock per session, not one per plugin.** The backend's own lock covers only the map of
sessions; each session has its own. A 300 s call from one workspace therefore does not hold
up another workspace's call, or a liveness check, on the same plugin.

Callbacks: the backend reaches Orbit only through `orbit tool run` or MCP `tools/call`, and
only for tools listed in `permissions.orbit_tools` *and* granted *and* reachable by the
caller that spawned it. The same intersection is
stamped into `ORBIT_ALLOWED_TOOLS` as information for the backend. No socket, no shared store
handle. Identity is a host-issued session: when the host spawns the backend it writes a
per-call record under `{global_root}/state/plugin-callbacks/` (plugin, pid, start time, and
the effective tool ceiling below) and
stamps `ORBIT_PLUGIN_CALLBACK` into the child. `orbit tool run` and MCP dispatch resolve that
session and enforce the recorded install. `ORBIT_PLUGIN` names the plugin for the backend; it
is not the gate, and neither is the inherited `ORBIT_ALLOWED_TOOLS` value. The backend's
restraint is not the boundary.

**The session carries the caller's ceiling, not just the plugin's name.** Knowing *which*
plugin is calling back does not decide the call, because the same plugin is reachable from
callers with different allowlists: a restricted caller's `permissions.orbit_tools` ∩ grant ∩
`allowed_tools` intersection is narrower than the manifest list, and the manifest list is
what a name-only gate would admit. So the child could accept a narrow intersection, clear the
`ORBIT_ACTIVITY_TOOLS` it inherited, and spend its valid token on any tool the manifest
requested [ORB-12801]. Spawning a separate `mcp` child per intersection (§4.2) does not close
that: it keeps the *advertised* allowlists apart, and the callback gate never read them.
The host therefore writes that intersection into the session record it owns — the one tree
the child is denied write access to (§4.3) — and every callback is decided by **the recorded
allowlist ∩ the session's ceiling**. Both halves are consulted on every call, and neither is
ever read out of the child's environment.

**How revocation and live sessions interact.** The two halves answer different questions and
change on different clocks, which is what makes the intersection monotone downwards for the
life of a session:

- The *recorded* half — the row's `enabled` flag, its grants, and `permissions.orbit_tools`
  read back from the install tree — is re-read on every callback. Revoking a grant, disabling
  the plugin or narrowing the manifest therefore stops a backend that is already running, at
  its next callback. There is no session to kill and no cache to invalidate, and a running
  child is not grandfathered past an operator's revocation.
- The *session* half is fixed when the host mints the record and never rewritten. So nothing
  that happens to the row afterwards can widen a live child: not an operator re-granting,
  not an upgrade whose manifest asks for more, and not the backend rewriting its own install
  tree — which it can do, since `{{plugin_state}}` and the install path checks bound *where*
  it may write, not whether the row can be made to name a wider allowlist.

The record states the ceiling from `schema_version: 2` on. A record that states none is
refused rather than read as unbounded: these files live only as long as the child they
identify, so the only way to meet one is a leftover from an older host, and a leftover is not
consent. Refusing it is only half the answer, though — a record this host's schema cannot
read is also *counted stale*: `orbit plugin doctor` reports it and the sweep that runs when
the next backend mints a session removes it. Skipping it in the janitor's view instead would
strand a mode-0600 file under `state/plugin-callbacks/` that no surface names and no sweep
reaches [ORB-12879]. Only bytes that are not a complete JSON value are left alone, because
both the mint and the pid binding leave the file empty between opening it and the single
write that fills it, and reaping what a concurrent scan finds in that window would destroy a
live session — along with the Landlock grant its child reads the record through.

**Every other CLI command is refused.** "Only through `orbit tool run` or MCP `tools/call`"
is a rule about the whole CLI, not about those two commands: `orbit workspace list --format
json` and `orbit run show <id>` read governed data and never consult
`permissions.orbit_tools`, so a plugin granted one tool could read whatever the CLI exposes
[ORB-12876]. The CLI therefore resolves the callback session once, before it pins a
generation or opens a runtime, and a recognized plugin child may run only the commands that
*are* a tool call: `orbit tool run <tool>`, its `orbit <ns> <verb>` spelling (§4.6), and
`orbit mcp serve`, whose every `tools/call` lands on the same allowlist. Everything else —
including `orbit update`, which would replace the host binary — is `policy_denied` before any
part of the command runs. Refusal is the default a new command inherits, rather than a
command-to-tool mapping that would leave each unmapped command open.

**Asking for a read instead.** A backend that needs what a refused command showed requests
the tool that serves it under `permissions.orbit_tools`, and reaches it on whichever entry
point that tool lives on. Two the first plugins want:

| read | tool | entry point | also needs |
| --- | --- | --- | --- |
| `orbit workspace list` | `orbit.workspace.list` | MCP `tools/call` only — workspace discovery is owned by the MCP server ([federated-mcp](../federated-mcp/1_overview.md)), not the generic tool registry | — |
| `orbit run show <id>` | `orbit.workflow.run.show` | `orbit tool run` or MCP | the `operator` capability, which that operation has always required |

The last column is the point of the repair, not a gap in it: `orbit.workflow.run.show` is a
governed operator surface while `orbit run show` was an ungated read, so the CLI spelling
*was* the way around its own governed-operation row. Closing the CLI surface means a plugin
that wants workflow observation holds `operator` like every other caller of it. Reaching
another *machine* through a federated destination is unchanged and still costs a `network`
grant and `ssh` in `requires.programs`, neither of which the default sandbox gives.

**What the credential is bound to.** The token names a record, and that record names a
process. A presented token is accepted only when the recorded pid is the calling process, its
parent, or its process group — the three the kernel still answers for a confined child, and
the reason the backend is spawned with `process_group(0)` so ordinary descendants carry the
backend pid as their PGID. Anything else is a mismatch: a token read out of another plugin's
record, or a token carried into a process that called `setsid`. A token that matches no
session at all is a missing credential. Every one of those is refused; none of them resolves
to a different plugin's allowlist, and none resolves to "ordinary caller". The recorded start
time is checked when a *scan* proposes a record — which is how a reused pid is rejected — but
not when a token names one: a confined child cannot read `/proc` of another process, so
demanding proof that its own parent is alive would refuse every legitimate callback. The
token is what makes the record this caller's; the host minted it for exactly this call.

**A confined child with no credential is refused, not promoted.** The sandbox does not grant
a plugin backend the session directory (§4.3), so a process that cannot even list
`{global_root}/state/plugin-callbacks/` is by construction inside a plugin sandbox. With no
token, that is an unidentified backend descendant and the call is refused on both entry
points. This is the boundary `setsid` cannot cross: a new session or process group sheds
ancestry, not confinement. The consequence to design against is the other direction — the
credential has to reach every process that calls back, so a backend that scrubs its child
environment (`env -i`) denies *itself* the callback rather than escaping its allowlist.
Ancestry alone still identifies a backend where the session directory is readable, which is
the unsandboxed case (`backend.sandbox: none` plus the `unsandboxed` grant) and the host's own
in-process dispatch. Landlock does not grant `/proc/<pid>` of another process, so identity
never reads another process's `environ`.

As implemented, the child also carries `ORBIT_PLUGIN_VERSION`, `ORBIT_TOOL_CWD` and
`ORBIT_PROC_ALLOWED_PROGRAMS` (`requires.programs`). `ORBIT_TOOL_NAME` is absent for an `mcp`
child, which serves every tool of its plugin; each call names its tool in `_meta.orbit`
instead.

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
| `permissions.fs.write` | `fs` | Each rendered path as a write tree; before a rule is compiled, normalized paths at or beneath Orbit's global root are refused except paths inside this plugin's `{{plugin_state}}` tree, and paths containing or inside workspace `.orbit` / `.git` are refused. The ruleset handles every write-side right, so a path without a write grant is read-only to the child | `(allow file-write* (subpath …))` via the profile's `modify` rules, after the same global-root and workspace-metadata refusal |
| `permissions.network: none` (default) | — | `ACCESS_NET_BIND_TCP \| ACCESS_NET_CONNECT_TCP` handled with no rule, which refuses every TCP endpoint (needs Landlock ABI 4; an older kernel fails closed) | `(deny network*)` appended after the compiler's broad allow |
| `permissions.network: loopback` | `network` | TCP left open — Landlock has no address filter, and the design's confinement claim is the filesystem | `(deny network*)` then loopback re-allows |
| `permissions.network: any` | `network` | TCP left open | the compiler's `(allow network*)` stands |
| `permissions.orbit_tools` | `orbit_tools` | Orbit's own global root and the workspace's `.orbit/` become **readable**, because a callback *is* `orbit tool run` and that command cannot start without `config.toml`, the recorded install and `workspaces.json`. Writable is a named inventory, never the roots: `state/logs`, `state/audit` and `tasks` under the global root, `tasks`, `frictions`, `state/audit`, `state/logs` and `state/job-runs` under the workspace's `.orbit/`, plus the `orbit.db` and `state/semantic.db` WAL file sets and the two executable-generation locks as individual files. `bin/orbit` — run unconfined by the scheduler and every worker — `plugins/`, `config.toml`, `mcp-callers.toml`, `clock.toml` and the workspace's `plugins.yaml`, `routines/` and `auto_tasks/` are read-only to the child. Two trees under the global root are **not readable at all**: `state/plugin-callbacks/` (the live callback credentials) and `plugins/.grants/` (the grant witnesses). The child is granted two single files inside them and nothing else: its *own* session record, which its `orbit tool run` reads back to identify itself, and its *own* grant witness, which that nested Orbit verifies before it registers the plugin row. Both directories stay unlistable, and no other plugin's token or witness is reachable — a confined child that loads another plugin's row therefore registers it inactive. What the callback itself may do is decided by the plugin's recorded `orbit_tools` grant and `permissions.orbit_tools` **intersected with the tool ceiling the host wrote into that session** — the spawning caller's own allowlist, which the recorded manifest list alone would ignore — looked up from the host-issued session on both `orbit tool run` and MCP `tools/call`, together with the ordinary governed-operation rows — not by the sandbox, not by `ORBIT_PLUGIN`, and not by the inherited `ORBIT_ALLOWED_TOOLS` value [ORB-12777] [ORB-12789] [ORB-12798] [ORB-12801] | same boundary, from the same inventory: write directories become `(subpath …)` roots and the named files are emitted literally, so a store file never widens into the root that holds it; the two unreadable trees are `(deny file-read* (subpath …))` clauses appended after the compiler's broad read allow, with the child's own session record and its own witness re-allowed after them (SBPL is last-match-wins) |
| `permissions.env_pass` | `env_pass` | Those names are copied from Orbit's environment into the otherwise allowlisted child environment, composed through the same `allowlisted_child_env` admission path as the baseline — `ORBIT_*` names are reserved for Orbit's own envelope and refused by `validate_structure`, so a privilege-bearing name (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) can never reach a plugin child even if requested | same |
| `requires.programs` | — | Not a sandbox rule: the declared programs are checked against a restricted caller's own `proc.spawn` allowlist and stamped into `ORBIT_PROC_ALLOWED_PROGRAMS` | same |
| `backend.sandbox: none` | `unsandboxed` | No ruleset at all | No `sandbox-exec` wrapper at all |

Granted write *directories* are created before the child starts only when their normalized path
is at or beneath a **host-materialized prefix**. There are exactly three kinds, and this is the
whole list:

| Host-materialized prefix | Present when |
| --- | --- |
| the selected workspace root (`{{workspace}}`) | a workspace is selected for the call |
| the plugin's own state tree (`{{plugin_state}}`) | always |
| `<global_root>/state/logs`, `<global_root>/state/audit`, `<global_root>/tasks`, and `<workspace>/.orbit/` `tasks`, `frictions`, `state/audit`, `state/logs`, `state/job-runs` | `orbit_tools` is granted |

The third group is not a separate list: it *is* the `orbit_tools` write inventory named in the
table above, the directories the host itself adds to the profile. One list decides both what is
granted and what may be created, so an entry added to that inventory is writable and creatable
in the same edit — the drift between the two that reddened CI four times cannot recur
[ORB-12872]. Note what the third group is *not*: `<global_root>/state` is not a prefix, so
making the two named stores under it materializable does not give the child — or the host — a
creatable tree over all of `state/`.

Creation checks every existing component without following symbolic links; an escaping `..` root
is left absent and a symlinked prefix is refused. A granted root outside those host-materialized
prefixes is never created by Orbit, even after explicit operator consent: it must already exist
as a directory or the call is refused with a diagnostic naming the root and telling the operator
to create the consented directory first. Consent authorizes the profile; it does not make a
manifest path a host-materialized prefix. Both halves are one test
(`the_host_materializes_its_own_write_roots_and_never_a_manifest_path_outside_them`): the
host-owned prefixes are created on the plugin's behalf, and an absent manifest-named path
outside them stays absent. A Landlock rule binds to an inode, so a safe grant naming a directory
that does not exist yet would otherwise grant nothing. Compiling the boundary resolves a root exactly
as the refusal above did — the same existing-prefix resolution — and creates a missing tail one
component at a time, refusing a link that appears between the two and refusing a created path
that no longer resolves to the identity that was checked. Validation and the compiled rule
therefore always name one directory: a root the check refuses is precisely the root a rule would
have carried, so call-time path construction cannot widen what was granted [ORB-12799].
Named write *files* are the exception — they belong to SQLite and to the generation protocol,
and a host that materialised one would break the store rather than confine it, so an absent
file simply yields no grant. The host process spawning the backend has already opened the
store and pinned its generation, so the file set is there for the call. A host that can
enforce neither backend refuses to run the plugin rather than running it unconfined;
`unsandboxed` is the only opt-out and it is a `doctor` finding.

Landlock has no deny rule, so the two unreadable trees are compiled by *not* granting them.
A rule's rights apply to everything beneath the path it names, so granting an ancestor
list-only would still let the child enumerate the denied directory — which for a directory of
credentials is most of the disclosure. The carve-out therefore grants no ancestor of a denied
path at all and grants each allowed sibling in its own right. Three consequences follow, all
from a rule binding an inode:

- The ancestors themselves stop being listable. A confined backend can read
  `{global_root}/config.toml` and the recorded installs, but cannot list `{global_root}`,
  `{global_root}/state/` or `{global_root}/plugins/`.
- A name created directly inside a carved-out directory after the child started has no
  granted ancestor either, so a plugin cannot rely on reading host state that appears at those
  levels mid-call.
- The child's own session record is granted as one inode, so the host rewrites that record in
  place when it binds the backend pid. Replacing it through a rename would leave the grant on
  an unlinked inode and the record unreadable to the very process it identifies.

The `orbit_tools` boundary is deliberately not "whatever the child needs": it is the same
inventory the agent sandbox grants a nested Orbit process (`append_linux_runtime_write_roots`
in `orbit-core`). Widening it is a security decision, because a plugin that can write
Orbit's global root can replace the binary the scheduler runs unconfined. Note that this
confines *filesystem* writes only — a plugin able to write `orbit.db` at all can still reach
its own `plugins` store row. What stops that row from becoming authority is the grant
authorization record under `plugins/`. It stays read-only only because the two grant paths
compose: the `orbit_tools` inventory does not add `plugins/`, and the independent `fs.write`
admission rule refuses every global-root descendant outside the plugin's own state tree. The
inventory alone is not that protection (§3) [ORB-12778].

### 4.4 Audit and provenance

A plugin refused because its stored grants do not match the set this host authorized is
audited at load as `plugin.load` / `denied`, with the claimed grant set on the row (§3).
Every call passes through the existing audited dispatch with `ToolEntryPoint` plus
`plugin: {name, version, manifest_digest}`. `manifest_digest` is the SHA-256 of the
`plugin.yaml` bytes actually loaded for that call, not a stale install-time value the
on-disk file no longer matches (§4.1). Tasks minted by a plugin auto-task carry
`plugin:<ns>` alongside the existing `auto-task:<name>` tag. Definitions seeded by a plugin
carry the provenance header. Removing a plugin never rewrites task history.

### 4.5 Definitions

- Routines target `job:<name>` only, as today; a plugin routine may only target a job the same
  plugin ships or a shipped default. Cross-plugin targets are a load error.
- Plugin jobs may reference shipped activities and their own activities. A reference to an
  activity supplied only by another plugin refuses the job's plugin at load; plugins do not gain
  an implicit dependency through catalog order.
- Activity names and job names are unique across active plugins. Host loading is deterministic:
  the first valid plugin keeps the name, while a later plugin with the same activity or job name
  is refused with a diagnostic naming both plugins. Workspace and shipped layers retain their
  documented precedence over the surviving plugin layer.
- Plugin activities are `agent_loop`, or `deterministic` with the one new action
  `plugin.tool_call { tool: <ns>.<verb>, input: {…} }`. This is the only addition to the
  closed action enums and it is what lets a routine drive a plugin without Rust.
- Seeded routines and auto-tasks are `enabled: false`. Turning one on is the same reviewed,
  versioned edit as for a shipped default. A plugin cannot enable its own schedule.
- Seeded filenames remain `<namespace>-<definition>.yaml`, but that spelling is not assumed to be
  injective when either component contains `-`. The managed-asset manifest's plugin owner is
  authoritative: if another namespace already owns the resulting filename, enabling is refused
  before any routine or auto-task is written and neither the file nor its manifest record changes.
  `--force` may replace an operator-customised file owned by the same plugin; it never transfers a
  file from one plugin owner to another.
- When a plugin is disabled, its seeded definitions are skipped through the retired-routine
  reconciliation path with a warning naming the plugin, not treated as load errors.

### 4.6 CLI

`orbit <ns> <verb> [--flag …]` is generated from each tool's `input_schema`. The mapping is
applied to the **top level** of the schema and nowhere deeper:

| Property shape | Surface |
|---|---|
| `string` | `--kebab-case <VALUE>`; an `enum` becomes clap's possible values, so an unknown one is refused with the list |
| `integer` / `number` | `--kebab-case <N>`, sent as a JSON number |
| `boolean` | `--kebab-case` for true, or `--kebab-case=<true\|false>` to set it explicitly |
| `array` of scalars | `--kebab-case <VALUE>`, repeated once per element |
| `object`, `array` of objects, or an untyped property | `--kebab-case-json '<JSON>'` |
| named in `cli.positional` | the same value as a positional argument, in manifest order, alongside its ordinary `--kebab-case` flag |

A boolean's optional value requires `=` (`--kebab-case=false`); a bare `--kebab-case value`
never consumes `value` as the flag's own, so a boolean flag can sit directly in front of a
positional argument without swallowing it.

`cli.verb` renames the subcommand: it is spelled like a tool verb, and two tools of one
plugin may not claim the same subcommand — `orbit <ns> <verb>` dispatches to exactly one
tool. Each `cli.positional` entry names a top-level `input_schema` property, once; an entry
that names nothing would be dropped without a word, so it refuses the plugin instead. Both
are checked against the schema the tool actually loads with, so a `{ $ref: <path> }` schema
is held to them too. Promoting a property to a positional argument does not remove its
ordinary flag — both spellings reach the same property, so a caller who already knows the
flag form is never forced to learn the positional one. Nothing is marked required at the
clap level: the tool's own `input_schema` is the authority on what a call must contain, and
a required flag would make `--input` alone unusable. A property whose flag would collide
with one the CLI owns (`--input`, `--input-file`, `--dry-run`, `--format`, `--root`,
`--workspace`) gets no flag and stays reachable through `--input`.

`--input '<json>'` and `--input-file` are always accepted and always win, so
`orbit graph recommend --query …` and `orbit tool run graph.recommend --input …` are the
same audited operation: the group declares the same `CommandOperation`, dispatches through
the same `ToolRunArgs`, and writes the same audit row. `--dry-run` is accepted too.

Only an **active** plugin contributes a group. There is no `git-foo` style passthrough: an
unknown `orbit <word>` — including a disabled plugin's namespace — is clap's ordinary
unknown-subcommand error, and plugin CLI never bypasses dispatch, dry-run or audit.
`orbit --help` lists the groups under a `Plugins:` heading, and `orbit <ns> --help` lists its
verbs with the manifest's descriptions. The tree is built at startup from the host's
installed manifests. Startup resolves the global config, opens SQLite read-only and queries the
`plugins` rows even when `~/.orbit/plugins/` does not exist; with no enabled row it reads no
manifest. Loaded manifests are cached for the
process and shared by CLI-tree discovery, runtime construction and host-global MCP discovery. An
unchanged `plugin.yaml` stamp reuses the resolved manifest immediately; if only the stamp changed,
its digest is checked before Orbit repeats the install-tree symlink walk and schema resolution.

### 4.7 Dashboard

The `plugins` tab lists every installed or pinned plugin from `GET /api/plugins` — enable
state, diagnostics, tools, panels and links — and draws each declared panel with one generic
renderer. The renderer contract is the whole frontend surface a plugin author needs:

| `render` | Input the panel's tool returns | Rendering |
|---|---|---|
| `kv` | an object | one label/value row per key; a nested value is shown as compact JSON |
| `table` | an array of objects (or `{rows: [...]}`) | one table whose columns are the union of the rows' keys, in first-seen order |
| `markdown` | a string, or an object with a `markdown` or `text` string | parsed and **sanitised** through the dashboard's existing `renderMarkdown` wrapper (raw HTML in the source is escaped, then DOMPurify runs) |
| `json` | anything | pretty-printed JSON |

Output that does not fit its declared mode falls back to `json` rather than rendering
nothing. `group` (`diagnostics` \| `operations` \| `config`) is a presentation hint on the
plugin's card.

`GET /api/plugins/<ns>/panels/<id>` executes the panel's source through the ordinary audited
tool dispatch with no caller-supplied input, and is served to any dashboard session. What
makes that safe is that a panel source is always a `read_only` tool: `validate_structure`
refuses a manifest whose panel names a mutating tool — naming the panel — so
`orbit plugin validate` reports it and no such plugin can be installed, and the read
re-checks the loaded manifest before executing. Writes (install, enable, grants) stay on the
CLI, where the operator answers the grant request.

The server single-flights each workspace/plugin/panel and reuses its successful response for
`refresh_ms` (30 seconds when omitted; accepted range 1–3600 seconds). Tabs therefore share one
audited backend execution per TTL window. Failures are not cached. Serialized tool output is
limited to 256 KiB; a larger value becomes a bounded JSON prefix with `truncated: true` and a
diagnostic stating the original size and limit, so a backend cannot make the dashboard retain
or ship an unbounded response.

Long-lived dashboard runtimes compare the current host plugin row set, including each row's
`updated_at`, with the exact rows used to build their tool surface. An install, enable, disable,
upgrade, grant, or certification change evicts and lazily rebuilds the affected workspace
runtime on its next request. `GET /api/plugins` and panel routes therefore reflect CLI lifecycle
changes without restarting `orbit web serve`.

`links` are plain tiles to plugin-hosted UIs (loopback by default), with `{{config.<key>}}`
rendered against the plugin's effective `[plugins.<ns>]` section. A dashboard summary has no
workspace or plugin-state invocation context, so a link containing `{{workspace}}` or
`{{plugin_state}}` remains wholly visible; an unknown `{{config.<key>}}` does too. No unresolved
reference is rendered as an empty string or a half-resolved URL. A tile's URL must be `http://`
or `https://` —
`validate_structure` refuses any other scheme, and the renderer draws no tile for one, so a
`javascript:` URL can never become an anchor in the operator's session. Plugins cannot introduce tabs or switch built-in
dashboard features on; that idea is deferred past v1. `router.js` gains one `plugins` entry
fed from `/api/plugins`, so new plugins need zero frontend edits and ship no JavaScript.

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
leaves every other plugin and all built-ins untouched. Every tool schema is compiled while
the plugin is read — naming the tool when it fails — so an invalid keyword or a `$ref` to
something the document does not contain is one refusal at load rather than a call that
fails every time it is made. A tool schema may only reference itself, with a `#/…` pointer;
the manifest's own `{ $ref: <path> }` form is the way to keep a schema in its own file.

## 5. Conformance

`orbit plugin validate <dir>` — manifest schema, path containment, namespace collisions,
schema self-consistency, definition cross-references, and the `spec.web` rules of §4.7.

`orbit plugin test <dir>` — runs `spec.tests` goldens through the real protocol. Each file is
`schemaVersion: 1`, `kind: PluginTest`, and a list of `{name, tool, input, expect.output}`
cases; `tool` is the manifest verb, so a golden travels with the plugin. A case whose tool
the manifest does not declare refuses the directory rather than being skipped. A temp
directory stands in for the global root and the workspace, so template paths do not touch
the operator's Orbit state. Template paths, `network: loopback`, and `orbit_tools` run
under the profile the manifest requests. An unconfined backend (`sandbox: none`), an
absolute non-template `fs.write` root, `network: any`, or any `env_pass` is refused, and
the refusal prints the requested grant set, unless the caller passes `--accept-requested`
or a `--grant` list naming each of those grants. With that consent the run uses the
requested profile. The consent is for this run only and does not record a host grant.
Output is compared as JSON
(exact, key order irrelevant); a failure names the test and prints expected beside actual,
and the command exits non-zero so a plugin's own CI can gate on it. A passing run records the
host's version on the installed plugin — `orbit plugin show` then prints "Certified for:
0.x" — but only when a plugin of that namespace is installed at the **same manifest digest**:
a directory that differs from the installed tree says nothing about the tree the host runs.
Installing a different manifest drops the claim.

`orbit plugin scaffold <ns>` — replaces `orbit tool scaffold`: a Python `exec` backend, one
`read_only` tool with input and output schemas, one `kv` panel over it, one disabled
auto-task, one skill stub, and two passing conformance goldens. `orbit tool scaffold` still
writes the v1 sidecar pair for one release and prints a deprecation naming its replacement.

## 6. What opens up in the codebase

| Today (closed) | Change |
|---|---|
| `register_builtins()` literal list; external tools loaded via plain `register()` | `PluginLoader` registers each manifest tool with `register_mcp(scope)` / `register_inactive` |
| `canonical_mcp_tool_definitions()` memoised in a `OnceLock`, external tools never in `tools/list` | MCP surface reads the registry; plugin tools advertised with their scope |
| `ExternalTool` unsandboxed, 15 s, no output validation | `PluginBackend::{Exec,Mcp}` with sandbox profile, schema validation, versioned envelope *(done)* |
| `Commands` enum only | one skipped `PluginGroup` variant; the clap subcommands are built from the loaded manifests before parsing *(done)* |
| `define_config_settings!` closed | dynamic `plugins.<ns>.<key>` admission validated by the plugin's JSON Schema, provenance-aware *(done)* |
| `GOVERNED_OPERATIONS` per-op rows | two generic plugin rows keyed on `execution_kind` |
| `DeterministicAction` closed | `plugin.tool_call` *(done)* |
| `RETIRED_ROUTINE_JOBS` only skip path | provenance-aware skip for disabled plugins *(done)* |
| `DEFAULT_*_FILES` + `skill_link_roots` | managed-asset reconciliation accepts a plugin layer with `plugin:<ns>@<version>` provenance *(done: plugin-seeded definitions are tracked in their own `.orbit-managed-plugin-assets.json` so a plugin's entries never retire a shipped default)* |
| `TABS` arrays + per-asset `include_str!` | one `plugins` group + one generic panel renderer *(done)* |

## 7. Phases (each its own PR into agent-main)

1. **Manifest + lifecycle over the existing tool path.** *Landed [ORB-12735].* `plugin.yaml`
   v2, `orbit plugin add|enable|disable|remove|list|show|validate|migrate`, `plugin_store`,
   tools registered with MCP scope and reaching `tools/list`. orbit-graph's migration from three
   sidecars to one manifest remains pending in that repository. Goldens for `orbit tool run`
   unchanged.
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
4. **Derived CLI + dashboard panels.** *Landed.* `orbit <ns> <verb>` built at startup from
   the installed manifests (§4.6), `GET /api/plugins` and
   `GET /api/plugins/<ns>/panels/<id>`, one generic renderer and the `plugins` tab (§4.7),
   `orbit plugin test` with the certification it records, and `orbit plugin scaffold` (§5).
   As implemented, a panel over a mutating tool is refused at `validate_structure` rather
   than at request time, so an installed plugin can never carry one; and the derived group
   declares the *same* `CommandOperation` as `orbit tool run`, which is what makes the two
   spellings one audited operation rather than two paths that agree today.
5. **Provider plugins** (deferred): open `ProviderRegistry` and collapse the four provider
   vocabularies, only when a second external provider exists.

## 8. Risks

- **Grant fatigue.** If `enable` demands a flag per permission nobody will read them. Default
  to a summary prompt with `--grant all` for interactive use and explicit flags for scripts.
- **Schema-derived CLI ergonomics.** Nested inputs make poor flags; `cli:` overrides and
  `--input` cover the long tail. Do not attempt full clap parity.
- **Catalog shadowing surprises.** Workspace-over-plugin precedence is right, but `orbit run
  show` must print which layer resolved each `activity:` ref.
- **`mcp` backend lifetime.** One long-lived child per caller context — workspace ×
  allowed-tools intersection — per runtime process means `orbit mcp serve`, `clock tick` and
  the CLI each spawn their own, and a process serving several workspaces spawns one per
  workspace. That multiplication is the price of not proxying one workspace's call to
  another's child (§4.2); acceptable in v1, pool later. Sessions are never reclaimed before
  the runtime ends, so an `orbit mcp serve` that visits many workspaces holds a child for
  each: idle-eviction is the first thing to add if that bites.
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
