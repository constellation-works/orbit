---
type: design
summary: "Scope: a plugin standard and contract for extending Orbit with tools, CLI groups, dashboard panels, routines, auto-tasks, activities, jobs and skills from one manifest"
tags: [plugins, tools, routines, auto-tasks, dashboard, cli]
last_updated: 2026-09-24
last_validated: 2026-09-22
---

# Scope: Orbit plugin standard

Status: phases 1-4 landed; phase 5 remains proposal.
Bearing: [Operations as data, not inherent methods](../orbit-core/4_decisions.md).
Precedents: `*.orbit-tool.yaml` sidecar manifests (orbit-graph still ships three; its migration
to `plugin.yaml` is pending), and orbit-research's version/capability allowlist against the
Orbit binary.

## Problem

Before this standard a "plugin" was three unrelated things: an **external tool** (executable
plus sidecar, registered by `orbit tool add`, unsandboxed, absent from MCP —
`orbit-tools/src/external.rs`); the **Claude Code plugin mirror** under `plugin/`; and a
**separate product** (orbit-research, orbit-graph) shelling out to `orbit tool run`. Nothing
let one artifact declare tools, CLI, schedules, panels and skills together, or record what it
is *allowed* to do; a first-party surface cost up to nine hand edits [ORB-12724].

## Goal

One manifest (`plugin.yaml`, `schemaVersion: 2`, superseding the v1 tool sidecar) declaring
every contribution (tools, CLI, dashboard panels, routines, auto-tasks, activities, jobs,
skills, config); one lifecycle (`orbit plugin …`); one execution protocol; one set of
invariants enforced at load, enable and call time. A plugin is **data plus one backend
executable**: no Rust, no dashboard JavaScript.

### Non-goals for v1

- New Rust **deterministic activity actions** (`DeterministicAction` stays closed); plugins
  expose a tool and call it through the generic `plugin.tool_call` action (§4.5).
- New **agent providers / executor types**; opening `ProviderRegistry` is Phase 5.
- **Arbitrary dashboard code or built-in feature toggles**; plugins get declarative panels
  and links only.
- Marketplace/discovery. Sources are a path, a git URL or an archive.

## 1. Contribution model

| Contribution | Manifest key | Where it lands | Surfaces derived |
|---|---|---|---|
| Tools | `spec.tools[]` | `ToolRegistry` as `<ns>.<verb>`; MCP as `<ns>_<verb>` | tool host, MCP `tools/list`, `orbit tool run`, `orbit <ns> <verb>`, panel source |
| CLI group | derived from tools (`cli:` override) | `orbit <ns> …` | clap subcommand built from the input schema |
| Activities / jobs | `spec.definitions.activities`, `.jobs` | catalog layer `plugin:<ns>` (§4.5) | `job:<name>` routine targets, `orbit run job` |
| Routines | `spec.definitions.routines` | seeded to `.orbit/routines/<ns>-<name>.yaml`, `enabled: false` | clock tick |
| Auto-tasks | `spec.definitions.auto_tasks` | seeded to `.orbit/auto_tasks/<ns>-<name>.yaml`, `enabled: false` | clock tick, `orbit auto-task` |
| Skills | `spec.skills[]` | linked into `skill_link_roots` as `<ns>-<directory-name>` | Claude/Codex skill discovery |
| Config | `spec.config` | `[plugins.<ns>]` in `config.toml`, validated by the plugin's JSON Schema | `orbit config`, Config tab provenance |
| Dashboard | `spec.web.panels[]`, `.links[]` | generic panel renderer; link tiles | `/api/plugins/<ns>/…`, one `plugins` tab group |

The **namespace** `<ns>` is `metadata.name`. It owns tool names `<ns>.*`, the CLI group
`orbit <ns>`, config `[plugins.<ns>]`, the provenance tag `plugin:<ns>` on seeded definitions
and minted tasks, and the catalog layer. Every built-in `Commands` variant is reserved; a
collision fails the load of that plugin only.

Skill IDs are namespaced too (`skills/graph` in `acme` links as `acme-graph`), reported by
`orbit plugin validate`. Linking may repair an older target from the same plugin but never
replaces a link owned by shipped skills, another plugin, or the user.

**`orbit.<ns>.*` is reserved for Orbit-originated plugins.** A manifest with
`metadata.publisher: constellation-works` and `metadata.origin: orbit` claims tool names
`orbit.<ns>.<verb>` (MCP `orbit_<ns>_<verb>`) and keeps `orbit <ns>` as its CLI group; every
other publisher gets bare `<ns>.*`. `origin: orbit` is honoured only when Orbit fetched the
source from a `git+` URL under `github.com/constellation-works`, or its manifest digest is in
the bundled first-party list; a local directory's Git remotes are never evidence. Any other
claim is refused at load. `orbit plugin migrate` emits no `publisher`/`origin`, so migrated
orbit-graph sidecars register bare `graph.*` until the author adds both fields and installs
from the verified `git+` source (or a release bundles the digest).

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
    host_api: 1                     # protocol major (§4.8)
    platforms: [linux, macos]
    programs: [git]                 # host programs the backend spawns; resolved at enable (§4.3)

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

  secrets:                                           # names only; values are set by the operator (§3)
    - name: refresh_token
      description: OAuth refresh token for the graph service.
      rotatable: true                                # the backend may replace it (not yet delivered)
```

Rules:

- `deny_unknown_fields` everywhere; `$ref` resolves only inside the plugin root.
- Templates are limited to `{{workspace}}`, `{{plugin_root}}`, `{{plugin_state}}` and
  `{{config.<key>}}`. Filesystem templates render against the effective `[plugins.<ns>]`
  values (workspace config over global config over manifest defaults), stringifying non-string
  scalars.
- Every path is canonicalised and must stay inside the plugin root or the granted fs profile.
  A rendered relative fs root is relative to the plugin root everywhere (validation,
  registration, conformance, call-time sandboxing), never to Orbit's working directory.

## 3. Lifecycle and state

```
orbit plugin add <path|git+url#ref|archive>   →  installed   (~/.orbit/plugins/<ns>/<version>/)
orbit plugin upgrade <ns> [source] [--grant …]
                                              →  upgraded    (permission diff printed; widening requires re-consent)
orbit plugin enable <ns> [--grant fs,network,orbit_tools,unsandboxed] [--workspace]
                                              →  active      (tools Active; definitions seeded; skills linked)
orbit plugin disable <ns>                     →  installed   (tools Inactive; seeded definitions skipped with a warning)
orbit plugin remove <ns> --yes                →  gone        (prints retained ~/.orbit/state/plugins/<ns> path)
orbit plugin remove <ns> --yes --purge-state  →  gone        (also deletes this plugin's Orbit-owned state)
orbit plugin remove <ns> --yes --record-only  →  gone        (record only; every installed file and secret is left in place)
orbit plugin secret set <ns> <name>           →  secret set  (value from stdin or a no-echo prompt, never argv)
orbit plugin secret list <ns> | rm <ns> <name>
orbit plugin list | show <ns> | doctor | validate <dir> | test <dir> | scaffold <ns> | sync | migrate
```

**Skill links.** Enable creates only the namespaced links `plugin validate` reports, in
discovery roots that are siblings of the active global root (`~/.orbit` →
`~/.agents/skills`, `~/.claude/skills`; `--root /path/to/root` → `/path/to/.agents/skills`,
`/path/to/.claude/skills`). Disable removes only links whose targets are inside the global
root's `plugins/<ns>/`.

**One version directory per namespace, swapped whole.** The `plugins` row's `install_path` is
the only authority for where a plugin lives: the loader reads it, every lifecycle verb verifies
it, and the sandbox profile is built from it. There is no `current` link; `add` prunes one left
by an older Orbit.

- `add` copies the source into a staging directory inside `~/.orbit/plugins/<ns>/` and
  publishes it with a single `rename`, so a concurrent reader (clock tick, MCP server,
  dashboard) never loads a half-written tree. Replacing a tree renames the old one aside
  first; in that window a reader gets "not installed", not half a plugin.
- The swap rolls back if anything fails before the row is written, so no tree is left
  without a row.
- Once the row names the new tree, everything else under `~/.orbit/plugins/<ns>/` (the
  replaced version, a stale `current`, crash scratch) is pruned: old trees are readable to
  every backend (§4.3). `remove` deletes the whole namespace directory after the same
  install-path check; `--record-only` leaves every file.

**Plugin state.** By default, `remove` leaves `{{plugin_state}}` under
`<global_root>/state/plugins/<ns>/` and prints its path. `--purge-state` deletes only
that namespace's state tree after verifying the install path and refusing symlinks in
the state path; it cannot be combined with `--record-only`. Data the plugin wrote
outside `{{plugin_state}}` (such as `.orbit-graph/`) is retained in either mode.

**Plugin secrets.** A plugin that calls an authenticated service declares the credentials it
needs in `spec.secrets` (`name`, `description`, `rotatable`) instead of keeping them in
`{{plugin_state}}`. Only names live in the manifest. A name starts with a lowercase letter, then
uses lowercase letters, digits, `_` or `-` (at most 64 characters); it is declared once and
namespaced to the plugin. The
operator supplies values; a value never enters argv, the environment, logs, audit rows,
`plugin show`, `/api/plugins`, the dashboard or an MCP response.

- **CLI (operator only).** `orbit plugin secret set <ns> <name>` reads the value from stdin or,
  on a terminal, a prompt with echo off; one trailing newline is dropped. Anything after the
  name is refused without being echoed, and a name the installed manifest does not declare is
  refused. `list <ns>` prints each declared name as `set`/`unset` with its last-updated time,
  plus any stored name the manifest no longer declares (`undeclared`); `rm <ns> <name>` deletes
  one. No `secret` verb is a callback entry point, so a plugin backend running any of them is
  refused `policy_denied` before stdin or the store is touched (§4.3).
- **Storage.** One JSON file per plugin at `<global_root>/state/plugin-secrets/<ns>.json`, mode
  `0600` in a `0700` directory, replaced by rename and written under a per-plugin lock file. Each
  write stamps a fresh random `version`; the store offers a versioned get and a compare-and-swap
  put (applied only against the expected version, or only when unset) for per-call delivery
  and backend rotation, which are not wired yet. There is no OS keychain backend: one file
  format keeps the semantics identical on every host and lets compare-and-swap sit under a
  single lock.
- **Unreadable to plugins.** `state/plugin-secrets/` is on the plugin sandbox's unreadable list
  with nothing granted back, not even a plugin's own file (§4.3); the host reads a value and
  hands it over. It is *not* denied to agent sandboxes: a nested `orbit` inside an agent
  sandbox must still be able to deliver a secret to its backend, so until the host-side broker
  lands an agent sandbox can read the global root, this tree included [ORB-13038].
- **Lifecycle.** `enable` (and `add --enable`) warns once per declared secret that has no value.
  `upgrade` (and any reinstall) keeps each secret the new manifest still declares, value and
  version intact, and deletes the rest. `remove` deletes the plugin's secrets unless
  `--record-only` is passed. `doctor` reports every declared-but-unset secret.

Secrets are never shared across plugins and cannot appear in `{{config.*}}` templates or panel
links.

**Every verb that touches the recorded tree checks it first.** `enable`, `disable` and
`remove` apply the loader's install-path check (below) before seeding from, unlinking by or
deleting a tree, and on refusal leave the row intact. Recovery is `orbit plugin add` or
`orbit plugin remove <ns> --yes --record-only`.

**Workspace declares, host installs.** A committed `.orbit/plugins.yaml` pins what a workspace
uses:

```yaml
schemaVersion: 1
plugins:
  - name: graph
    version: "0.4.x"
    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1
    enabled: true
```

- Install is global only (once per host under `~/.orbit/plugins/`, shared by every
  workspace). Plugin trees are never vendored; a `source:` inside the repository is refused.
- Git sources accept only `git+https://…`, `git+ssh://…` and SCP-style `git+git@host:path`.
  Other transports, URL-shaped options and refs beginning with `-` are refused before Git is
  spawned. Clones disable user-selected protocols and terminal prompts, permit only HTTPS and
  SSH, and end option parsing before the URL.
- A source tree containing a symbolic link is refused before copying, naming the entry;
  `load_plugin_dir` repeats the walk so a hand-edited install cannot become active.
- `orbit plugin sync` converges this host and workspace from the pin file: installs missing
  plugins, applies `enabled: false` by disabling the host row, and applies `enabled: true`
  only after permission review. A committed pin is never grant consent; pass the complete
  reviewed set with `--grant`. For an already-enabled plugin, sync seeds or refreshes its
  routines and auto-tasks in the current workspace.
- Before installing a pin, sync checks that the source manifest declares the pinned namespace
  and satisfies any version requirement. A mismatch is reported unsatisfied with nothing
  written, enabled, linked or seeded; sync continues with other pins.

Grants, install paths, digests and enable state are **host-local** (SQLite `plugin_store`,
next to `tool_store`). The pin's `enabled:` is a convergence instruction, so syncing another
workspace may change that shared host toggle; grants still require explicit consent. A pinned
plugin that is not installed or lacks requested grants gets its tools registered via
`register_inactive` and one deduped diagnostic naming the missing step.

**The recorded grant set is tamper-evident.** `orbit plugin enable` and an upgrading `--grant`
write an integrity value over the authorized set to `~/.orbit/plugins/.grants/<ns>.json`. The
loader refuses a row whose grants do not match it: no tools, a `doctor` finding, and a `denied`
audit row per load pass. The value is
`sha256("orbit.plugin.grants.v1\n<ns>\n<enabled|disabled>\n<sorted grants>")` — a plain
digest, not a MAC, because the child can read the whole global root. What bounds an attacker
is the write boundary: `orbit_tools` makes `orbit.db` writable but never `plugins/`, and an
`fs` grant may write beneath the global root only inside the plugin's `{{plugin_state}}`
(§4.3) [ORB-12778].

- A row with non-empty grants and no witness is refused. Witnesses are not back-filled from
  existing rows, so hosts upgrading past this change re-run
  `orbit plugin enable <ns> --grant …` once per granted plugin.
- When a manifest's request widens without re-consent, the installer first writes a disabled,
  empty-grant witness, so the old witness cannot be replayed against the new manifest.
- The witness also records the path each `requires.programs` entry resolved to at that
  consent (§4.3). Those paths are not in the digest: they live only in the host-owned witness,
  which is itself the consent.

**The row's install path is held to the install root.** The witness covers only `name`,
`enabled` and grants (binding version or digest would refuse every upgrade), so a backend could
repoint `install_path` at a tree it wrote, with a matching digest. Every enabled row's
`install_path` must therefore resolve physically and strictly beneath `~/.orbit/plugins/<ns>/`
before anything is read from it (else a `denied` audit row and one diagnostic), checked at load
and on every `orbit_tools` callback [ORB-12785].

**Seeding follows the managed-asset rule.** Routines, auto-tasks and skills are written once
with `provenance: plugin:<ns>@<version>` and a digest in `.orbit-managed-plugin-assets.json`,
kept separate from the shipped catalog's `.orbit-managed-assets.json` so plugin entries never
retire a shipped default. An upgrade re-seeds only files whose digest still matches the
previously shipped version; a customised file gets a warning and a `--force` path. Activities
and jobs are not copied: they form a catalog layer (§4.5).

## 4. Contracts Orbit enforces

### 4.1 Placement, never permission

The manifest says *where* a tool appears (`mcp_scope`, `execution_kind`, CLI shape), never
*who* may call it. Plugin tools resolve to one of two generic governed rows
(`PLUGIN_TOOL_READ_ONLY`, `PLUGIN_TOOL_MUTATING` in
`orbit-common/src/governance/authorization.rs`): `read_only` tools are callable by `Agent |
Operator | Runner`; `mutating` tools by `Operator | Runner`, and by `Agent` only when the
task's `required_tools` or the activity allowlist names them.

`permissions:` is a *request*. `--grant` on an enabling add, enable, upgrade or sync is the
only source of authority, and `orbit plugin show` prints requested vs granted. A stored grant
set that fails its witness (§3) grants nothing on every surface.

**Grant list semantics.**

- An explicit `--grant` list is the complete authorized set and replaces the recorded one
  (`add --enable`, `enable`, grant-consenting `sync`). Omitting `--grant` on `enable`
  preserves recorded grants, so disable/re-enable needs no restatement.
- A narrower list revokes down to it; `--grant none` records an explicit empty set.
- `--grant all` (every grant this build knows) and `--grant requested` (exactly what
  `permissions`/`backend.sandbox` ask for) are the other reserved spellings. None of the three
  mixes with a literal list; each replaces the witness like a literal list does.
- Grants are validated identically whether fresh or read back from storage: an unrecognized
  name (retired, renamed, or from a newer Orbit) refuses the row rather than being dropped.
- `orbit plugin add --grant …` without `--enable` is rejected.

**Upgrade and widening.** On a manifest digest change, `add` and `upgrade` compare filesystem
roots, network mode, env names, Orbit-tool allowlist and sandbox mode. Unchanged or narrower
keeps the enable/grant state. Any widening disables the plugin, clears its grants and witness,
prints the widened requests, and names the full `orbit plugin enable <ns> --grant …` command.
`plugin upgrade <ns> [source]` defaults to the recorded source and always prints the diff; its
own `--grant …` is explicit re-consent.

**Manifest digest binding.** Every load hashes the on-disk `plugin.yaml` and compares it to
the row's install-time `manifest_digest`. A mismatch registers the plugin inactive with a
diagnostic naming both digests; recovery is `orbit plugin add <source> --force` then
`orbit plugin enable <ns> --grant …`. The profile compiled for a call is always the granted
one. The comparison is meaningful only because `install_path` is first held to
`~/.orbit/plugins/<ns>/` (§3) [ORB-12785].

**Write-root admission.** Every rendered `permissions.fs.write` root is normalized
physically (kernel-resolved longest existing prefix plus the missing names), so a symlink
planted in `{{plugin_state}}` cannot pass a root into the install namespace [ORB-12799].
Refused:

- a root that contains the plugin install tree or Orbit's global root;
- any root beneath the global root outside the plugin's own `{{plugin_state}}`;
- a root that contains or lies inside the workspace's `.orbit` or `.git` (compared by path
  component, so `{{workspace}}/.orbit-graph` is fine).

The rule runs at `orbit plugin validate`, registration (against a synthetic workspace) and
call time (against the real one). An `fs` grant therefore cannot reopen `bin/`,
`plugins/.grants`, another plugin's tree, workspace control files or Git hooks.

### 4.2 Execution protocol

`exec` backend — one process per call, current dir = caller cwd, env cleared to the
allowlisted child env plus:

```
ORBIT_HOST_API=1  ORBIT_VERSION=0.24.0  ORBIT_PLUGIN=graph  ORBIT_PLUGIN_VERSION=…
ORBIT_PLUGIN_ROOT=…  ORBIT_PLUGIN_STATE=…  ORBIT_PLUGIN_CALLBACK=<host-issued token>
ORBIT_TOOL_NAME=graph.recommend  ORBIT_TOOL_CWD=…  ORBIT_WORKSPACE_ROOT=…
ORBIT_ALLOWED_TOOLS=orbit.task.show,orbit.search  ORBIT_PROC_ALLOWED_PROGRAMS=git
```

stdin:  `{"schema_version":1,"tool":"graph.recommend","input":{…},"context":{"workspace_root":…,"agent":…,"model":…}}`
stdout: `{"ok":true,"output":{…}}` or `{"ok":false,"error":{"code":"…","message":"…","retryable":false}}`

Non-zero exit, non-JSON stdout, or output failing `output_schema` is a tool error; there is no
partial success. Timeout is `backend.timeout_ms`, capped by a host ceiling.

`mcp` backend — Orbit spawns the plugin's stdio MCP server once per *caller context* per
runtime, keeps it alive, proxies each `<ns>.<verb>` as `tools/call`, and refuses to start if
the server's `tools/list` disagrees with the manifest's `tools:` (names and schemas). Orbit is
the only client; the plugin never listens on a socket.

- **Caller context = workspace × allowed-tools intersection.** A child is bound to a workspace
  by its cwd, `ORBIT_WORKSPACE_ROOT` and its `{{workspace}}` write roots, and one runtime
  (`orbit clock tick`, `orbit mcp serve`) serves several workspaces. Both halves key the
  session, so no call is proxied to a child confined to another workspace or allowlist
  [ORB-12820].
- **Per-call context travels on the request** under `params._meta.orbit`, because a shared
  child serves every tool and every call. `ORBIT_TOOL_NAME` is absent for an `mcp` child.

  ```
  {"name":"recommend","arguments":{…},
   "_meta":{"orbit":{"workspace_root":…,"agent":…,"model":…,"tool":"graph.recommend"}}}
  ```

- **Orbit answers server-initiated requests**: `ping` with `{}`, anything else with JSON-RPC
  `-32601` (Orbit declares no client capabilities). A server waiting on an unanswered request
  would otherwise be killed at the deadline [ORB-12820].
- **One lock per session.** The backend lock covers only the session map, so a long call in
  one workspace does not block another workspace's call or liveness check.

**Advertised input schema.** MCP `tools/list` advertises a tool's declared `input_schema` as
written — `enum`, `const`, `default`, bounds, `minLength`, `description`, `required`,
`additionalProperties`, `$defs`/`$ref` and combinators included — not the flat parameter list
`orbit tool show` prints. For a `workspace`-scoped tool Orbit adds only its `workspace` selector:
a property (which `additionalProperties: false` then admits), required in an unbound session,
and stripped before the call reaches the plugin. A tool that declares no `input_schema` is
advertised as an empty open object. The keywords not carried as declared:

| Keyword | Advertised as | Why |
|---|---|---|
| root `type` | `"object"` | MCP's `inputSchema` must be an object schema and `tools/call` arguments are always an object |

A root keyword that constrains property names or counts (`propertyNames`, `maxProperties`) is
carried too, so it also constrains the injected selector; a plugin using one must admit
`workspace`.

**Callbacks.** A backend reaches Orbit only via `orbit tool run` or MCP `tools/call`, for
tools in `permissions.orbit_tools` ∩ granted ∩ reachable by the spawning caller
(`ORBIT_ALLOWED_TOOLS` echoes this, as information only). Identity is a host-issued session: at
spawn the host writes a record under `{global_root}/state/plugin-callbacks/` (plugin, pid,
start time, tool ceiling) and stamps `ORBIT_PLUGIN_CALLBACK`; both entry points resolve it and
enforce the recorded install. `ORBIT_PLUGIN` is not the gate.

**The session carries the caller's ceiling.** One plugin is reachable from callers with
different allowlists, so a name-only gate would admit the whole manifest list [ORB-12801].
Every callback is decided by **recorded allowlist ∩ session ceiling**, both consulted per
call, neither read from the child's environment:

- The *recorded* half (row `enabled`, grants, `permissions.orbit_tools` from the install tree)
  is re-read on every callback, so revoking a grant, disabling or narrowing the manifest stops
  a running backend at its next callback.
- The *session* half is fixed at mint and never rewritten, so nothing done to the row
  afterwards (re-grant, widening upgrade, a backend rewriting its tree) widens a live child.

**Session record schema.** Records state the ceiling from `schema_version: 2`. A record with no
ceiling or an unreadable schema is refused and counted stale: `orbit plugin doctor` reports it
and the next session mint's sweep removes it [ORB-12879]. Incomplete JSON is left alone, since
a record is briefly empty between open and its single write.

**Every other CLI command is refused.** The CLI resolves the callback session once, before it
pins a generation or opens a runtime. A recognized plugin child may run only `orbit tool run
<tool>`, its `orbit <ns> <verb>` spelling (§4.6), and `orbit mcp serve` (whose `tools/call`
hits the same allowlist). Everything else, including `orbit update`, is `policy_denied` before
any part runs; refusal is the default a new command inherits [ORB-12876]. A backend that needs
a read a refused command offered requests the serving tool instead:

| read | tool | entry point | also needs |
| --- | --- | --- | --- |
| `orbit workspace list` | `orbit.workspace.list` | MCP `tools/call` only — workspace discovery is owned by the MCP server ([federated-mcp](../federated-mcp/1_overview.md)) | — |
| `orbit run show <id>` | `orbit.workflow.run.show` | `orbit tool run` or MCP | the `operator` capability |

Reaching another machine through a federated destination still needs a `network` grant and
`ssh` in `requires.programs`.

**Credential binding.** A token is accepted only when its record's pid is the caller, its
parent, or its process group (backends spawn with `process_group(0)`). A token from another
record or carried past `setsid` is a mismatch; one matching no session is missing. Both are
refused, never resolved to another plugin or to an ordinary caller. Start time is checked only
when a *scan* proposes a record (rejecting pid reuse): a confined child cannot read another
process's `/proc`.

**A confined child with no credential is refused, not promoted.** Backends cannot list
`state/plugin-callbacks/` (§4.3), so a caller that cannot is inside a plugin sandbox and, with
no token, is refused on both entry points. A backend that scrubs its child env (`env -i`)
denies itself callbacks. Ancestry alone identifies a backend only where that directory is
readable (unsandboxed backends, in-process dispatch). Identity never reads another process's
`environ`.

### 4.3 Sandboxing

Plugin backends run under Landlock (Linux) / `sandbox-exec` (macOS) with the **granted** `fs`
profile, `network` mode and `programs`; a plugin missing a required grant never gets here
(§4.1). A host that can enforce neither refuses to run the plugin. `backend.sandbox: none`
needs the `unsandboxed` grant and is a finding in `orbit plugin doctor` and the dashboard
reliability view.

| Manifest | Grant | Linux (`spawn_under_linux_landlock_boundary`) | macOS (`compile_macos_sandbox_profile` + `append_macos_network_access`) |
|---|---|---|---|
| (always) | — | Plugin root and its own `{{plugin_state}}` readable (the plugin root also executable); host runtime grants (`/usr`, loader, resolver files, `PATH` dirs, tool state) from the same table as activity-scoped `proc.spawn`; the unreadable trees below get no grant | The compiler's read allow plus its credential denies; the unreadable trees below as `(deny file-read* (subpath …))`, then the child's own state re-allowed as a `subpath` and its own record and witness as `literal`s (last match wins) |
| `permissions.fs.read` | `fs` | Each rendered path as a read tree or file | `(allow file-read* (subpath …))` |
| `permissions.fs.write` | `fs` | Each rendered path as a write tree, after §4.1 write-root admission; paths without a write grant are read-only | `(allow file-write* (subpath …))`, same admission |
| `network: none` (default) | — | TCP bind/connect handled with no rule, refusing every endpoint (needs Landlock ABI 4; older kernels fail closed) | `(deny network*)` |
| `network: loopback` | `network` | TCP left open (Landlock has no address filter) | `(deny network*)` then loopback re-allows |
| `network: any` | `network` | TCP left open | `(allow network*)` stands |
| `permissions.orbit_tools` | `orbit_tools` | See the inventory below | Same inventory: write dirs as `(subpath …)`, named files literally; the unreadable trees stay denied as in the first row |
| `permissions.env_pass` | `env_pass` | Named vars copied into the allowlisted child env via `allowlisted_child_env`; `ORBIT_*` names are refused by `validate_structure`, so `ORBIT_OPERATOR` or `ORBIT_WORKSPACE_CLAIM_TOKEN` never reach a child | same |
| `requires.programs` | — (resolved at enable) | Each program's recorded path as a read-and-execute file, whatever the caller's `PATH`; also checked against a restricted caller's `proc.spawn` allowlist and stamped into `ORBIT_PROC_ALLOWED_PROGRAMS` | The same path as a read `subpath` (the compiler already allows `process*`) |
| `backend.sandbox: none` | `unsandboxed` | No ruleset | No `sandbox-exec` wrapper |

**Declared programs.** Landlock executes only out of the caller's `PATH` directories and
granted roots, so a program off that `PATH` (a `uv` in `~/.local/bin` spawned by a systemd
unit) would get `Permission denied`. Every enabling command (`enable`, `add --enable`, an
upgrading `--grant`, a consenting `sync`) therefore resolves `requires.programs` once, against
the consenting operator's `PATH`:

- A bare name is searched on `PATH` (absolute entries only, executable regular files only); an
  absolute path is taken as written; a relative path is refused. Either way the **canonical**
  path is what gets recorded, in the grant witness (§3).
- The profile grants each recorded path read and execute, whichever caller (systemd unit, MCP
  server, ssh shell) spawns the backend. Nothing is resolved at call time.
- A recorded path is granted only while it still names that executable: it exists, is an
  executable regular file, is still its own canonical path (a link retargeted after consent
  stops matching), and lies outside the unreadable trees below. A path for a name the manifest
  no longer declares grants nothing.
- A changed resolution is a re-consent event: re-running `orbit plugin enable <ns>` records the
  new path and warns with the old and new one. A program that did not resolve warns at enable.
- `orbit plugin show` lists each program with its recorded path and whether it is granted.
  `orbit plugin doctor` reports every program of an active plugin that will not be granted,
  including those with no recorded path (unresolved at enable, or enabled by an Orbit that did
  not record them).

**The `orbit_tools` inventory.** A callback *is* `orbit tool run`, which needs `config.toml`,
the recorded install and `workspaces.json`, so the global root and the workspace `.orbit/`
become **readable**. Writable is a named inventory, never the roots:

- directories `state/logs`, `state/audit`, `tasks` under the global root, and `tasks`,
  `frictions`, `state/audit`, `state/logs`, `state/job-runs` under the workspace `.orbit/`;
- the `orbit.db` and `state/semantic.db` WAL file sets and the two executable-generation
  locks, as individual files.

`bin/orbit` (run unconfined by the scheduler and workers), `plugins/`, `config.toml`,
`mcp-callers.toml`, `clock.toml`, and the workspace's `plugins.yaml`, `routines/` and
`auto_tasks/` stay read-only.

**Unreadable trees.** `state/plugin-callbacks/`, `plugins/.grants/`, `state/plugins/` and
`state/plugin-secrets/` are **unreadable** to every plugin child, whatever it was granted. Each
child gets back exactly its own session record and own grant witness (single files), and its
own `state/plugins/<ns>` (`{{plugin_state}}`, a whole tree); nothing in the secret store is
granted back (§3). So another plugin's token, witness or state is
unreachable: a confined child loading another plugin's row registers it inactive, and a
credential a plugin keeps in `{{plugin_state}}` is readable by that plugin alone. A manifest
`fs.read` root that resolves inside one of these trees, outside the plugin's own state, is
dropped from the profile with a warning; no grant re-allows it. Writing `{{plugin_state}}`
still needs an `fs.write` root there and the `fs` grant (§4.1 admission is unchanged). This is the inventory the agent sandbox grants a nested Orbit
(`append_linux_runtime_write_roots` in `orbit-core`); widening it is a security decision
[ORB-12777] [ORB-12789] [ORB-12798] [ORB-12801]. The witness under `plugins/` stays read-only
only because both grant paths compose: the inventory omits `plugins/`, and `fs.write`
admission refuses global-root descendants outside `{{plugin_state}}` [ORB-12778].

**Host-materialized write roots.** A rule binds to an inode, so a grant naming a missing
directory would grant nothing. Granted write directories are created before spawn only at or
beneath exactly three prefixes:

| Host-materialized prefix | Present when |
| --- | --- |
| the selected workspace root (`{{workspace}}`) | a workspace is selected |
| the plugin's own state tree (`{{plugin_state}}`) | always |
| the `orbit_tools` write directories above | `orbit_tools` is granted |

The third row *is* the `orbit_tools` inventory, so one list decides both what is writable and
what may be created [ORB-12872]; `<global_root>/state` itself is not a prefix.

- Creation never follows links: an escaping `..` root stays absent, a symlinked prefix is
  refused, and a missing tail is created one component at a time, refusing a link or identity
  change in between, so the admitted root and the compiled rule name one directory
  [ORB-12799].
- A granted root outside those prefixes is never created, even with consent; it must already
  exist or the call is refused with a diagnostic telling the operator to create it
  (`the_host_materializes_its_own_write_roots_and_never_a_manifest_path_outside_them`).
- Named write *files* are never created (they belong to SQLite and the generation protocol);
  an absent file yields no grant. The spawning host has already opened the store.

**Landlock carve-outs.** Landlock has no deny rule, so the unreadable trees are compiled by
granting no ancestor of a denied path and granting each allowed sibling in its own right
(an ancestor granted list-only would still let the child enumerate credentials). Consequences:

- A confined backend can read `{global_root}/config.toml` and recorded installs but cannot
  list `{global_root}`, `{global_root}/state/`, `{global_root}/state/plugins/` or
  `{global_root}/plugins/`.
- A name created directly inside a carved-out directory after spawn is not readable. An
  unreadable tree that does not exist yet at spawn is carved out the same way (its ancestors
  get no grant), so a `state/plugins/<ns>` another plugin creates while a long-lived backend
  runs stays out of reach.
- The session record is granted as one inode, so the host rewrites it in place when binding
  the backend pid; a rename would leave the grant on an unlinked inode.

### 4.4 Audit and provenance

- A plugin refused for mismatched grants is audited at load as `plugin.load` / `denied` with
  the claimed grant set (§3).
- Every call goes through the audited dispatch with `ToolEntryPoint` plus
  `plugin: {name, version, manifest_digest}`, where `manifest_digest` is the SHA-256 of the
  `plugin.yaml` bytes loaded for that call.
- Tasks minted by a plugin auto-task carry `plugin:<ns>` beside `auto-task:<name>`; seeded
  definitions carry the provenance header. Removing a plugin never rewrites task history.

### 4.5 Definitions

- **Catalog layer.** Activities and jobs are not copied into the workspace. The plugin layer
  loads after the workspace and the shipped defaults, so a workspace file shadows a plugin's
  and a shipped default is never displaced (L-0060). `orbit run show` prints the resolving
  layer and what it shadowed (`orbit-core/src/application/job/catalog_layers.rs`).
- Routines target `job:<name>` only, and only a job the same plugin ships or a shipped default;
  cross-plugin targets are a load error.
- Plugin jobs may reference shipped activities and their own; a reference to another plugin's
  activity refuses the job's plugin at load.
- Activity and job names are unique across active plugins. Loading is deterministic: the first
  valid plugin keeps the name, a later one is refused with a diagnostic naming both.
- Plugin activities are `agent_loop`, or `deterministic` with the one new action
  `plugin.tool_call { tool: <ns>.<verb>, input: {…} }` — the only addition to the closed
  action enums.
- Seeded routines and auto-tasks are `enabled: false`; enabling one is the same reviewed edit
  as for a shipped default. Provenance is a `# provenance: plugin:<ns>@<version>` header
  comment, since `RoutineDefinition` and `AutoTaskDefinition` are `deny_unknown_fields`.
- Seeded filenames are `<namespace>-<definition>.yaml`, which is not injective when either part
  contains `-`. The plugin-asset manifest's owner is authoritative: if another namespace owns
  the filename, enabling is refused before anything is written. `--force` may replace an
  operator-customised file of the same plugin; it never transfers ownership.
- A disabled plugin's seeded definitions are skipped through the retired-routine
  reconciliation path with a warning naming the plugin, not load errors.
- A `[plugins.<ns>]` value the plugin's schema rejects refuses that plugin at load, naming the
  key (§4.9).

### 4.6 CLI

`orbit <ns> <verb>` is generated from the **top level** of each tool's `input_schema`:

| Property shape | Surface |
|---|---|
| `string` | `--kebab-case <VALUE>`; an `enum` becomes clap's possible values |
| `integer` / `number` | `--kebab-case <N>`, sent as a JSON number |
| `boolean` | `--kebab-case` for true, or `--kebab-case=<true\|false>` (the `=` is required, so a boolean never swallows a following positional) |
| `array` of scalars | `--kebab-case <VALUE>`, repeated |
| `object`, `array` of objects, or untyped | `--kebab-case-json '<JSON>'` |
| named in `cli.positional` | also a positional argument, in manifest order; its flag remains |

- `cli.verb` renames the subcommand; two tools of one plugin may not claim the same one.
  Each `cli.positional` entry must name a top-level property exactly once or the plugin is
  refused. Both are checked against the loaded schema, including `$ref` schemas.
- Nothing is required at the clap level; the `input_schema` is the authority.
- A property whose flag would collide with a CLI-owned flag (`--input`, `--input-file`,
  `--dry-run`, `--explain`, `--format`, `--root`, `--workspace`, `--help`, `--version`) gets no
  flag and stays reachable through `--input`.
- `--input '<json>'` and `--input-file` are always accepted and win. The group declares the
  same `CommandOperation` and dispatches through the same `ToolRunArgs` as `orbit tool run`, so
  both spellings are one audited operation. `--dry-run` is accepted.
- Only an **active** plugin contributes a group. There is no passthrough: an unknown
  `orbit <word>`, including a disabled plugin's namespace, is clap's unknown-subcommand error.
  `orbit --help` lists groups under `Plugins:`; `orbit <ns> --help` lists verbs.
- The tree is built at startup from the enabled `plugins` rows (SQLite opened read-only; no
  enabled row, no manifest read). Manifests are cached per process and shared by CLI, runtime
  and host-global MCP discovery; a changed `plugin.yaml` stamp re-checks the digest before the
  symlink walk and schema resolution are repeated.

### 4.7 Dashboard

The `plugins` tab lists every installed or pinned plugin from `GET /api/plugins` (enable
state, diagnostics, tools, panels, links) and draws each panel with one generic renderer:

| `render` | Input the panel's tool returns | Rendering |
|---|---|---|
| `kv` | an object | one label/value row per key; nested values as compact JSON |
| `table` | an array of objects (or `{rows: [...]}`) | columns are the union of row keys, first-seen order |
| `markdown` | a string, or an object with a `markdown` or `text` string | the dashboard's `renderMarkdown` wrapper (raw HTML escaped, then DOMPurify) |
| `json` | anything | pretty-printed JSON |

Mismatched output falls back to `json`; `group` is a presentation hint.

- `GET /api/plugins/<ns>/panels/<id>` runs the panel source through the audited tool dispatch
  with no caller input, for any dashboard session. This is safe because a panel source must be
  a `read_only` tool: `validate_structure` refuses a manifest whose panel names a mutating tool,
  and the read re-checks the loaded manifest. Writes (install, enable, grants) stay on the CLI.
- The server single-flights each workspace/plugin/panel and caches a success for `refresh_ms`
  (default 30 s; 1–3600 s). Failures are not cached. Serialized output is capped at 256 KiB
  (`PANEL_OUTPUT_LIMIT_BYTES`); a larger value becomes a bounded JSON prefix with
  `truncated: true` and a diagnostic naming size and limit.
- Dashboard runtimes compare host `plugins` rows (with `updated_at`) against those their tool
  surface was built from, and lazily rebuild on any change; no `orbit web serve` restart.
- `links` are tiles to plugin-hosted UIs with `{{config.<key>}}` rendered from the effective
  `[plugins.<ns>]`. A link containing `{{workspace}}`, `{{plugin_state}}` or an unknown config
  key is left wholly unrendered, never half-resolved. URLs must be `http://` or `https://`:
  `validate_structure` refuses other schemes and the renderer draws no tile for one.
- Plugins cannot add tabs or toggle built-in features. `router.js` has one `plugins` entry, so
  new plugins need no frontend edits.

### 4.8 Compatibility

- `requires.host_api` must equal the host's `PLUGIN_HOST_API` or be one major behind it; one
  behind still registers, and `doctor` prints a deprecation. Anything else refuses
  (`unmet_requirement` in `orbit-core/src/runtime/plugin/requirements.rs`).
- `requires.orbit` is checked on `enable` and again at runtime build; a mismatch after an
  upgrade flips the plugin Inactive with a diagnostic rather than failing the runtime.
- v1 `*.orbit-tool.yaml` sidecars and `orbit tool add` keep working;
  `orbit plugin migrate <binary>` writes a v2 manifest from a set of sidecars.

### 4.9 Fail closed, isolate blast radius

An invalid manifest, unresolvable `$ref`, schema rejecting its own `defaults`, missing binary
or collision refuses **that plugin** at load with one diagnostic; nothing else is touched. Tool
schemas compile at read time (naming the tool on failure) and may reference only themselves
(`#/…`); a separate schema file uses the manifest's `{ $ref: <path> }` form.

## 5. Conformance

`orbit plugin validate <dir>` — manifest schema, path containment, namespace collisions,
schema self-consistency, definition cross-references, and the `spec.web` rules of §4.7.

`orbit plugin test <dir>` — runs `spec.tests` goldens through the real protocol.

- Each file is `schemaVersion: 1`, `kind: PluginTest`, a list of
  `{name, tool, input, expect.output}` cases; `tool` is the manifest verb. A case naming an
  undeclared tool refuses the directory.
- A temp directory stands in for the global root and workspace. Template paths,
  `network: loopback` and `orbit_tools` run under the requested profile. `sandbox: none`, an
  absolute non-template `fs.write` root, `network: any`, or any `env_pass` is refused (printing
  the requested grants) unless `--accept-requested` or a `--grant` list names each; that
  consent is for the run only.
- Output is compared as JSON (exact, key order irrelevant); a failure prints expected beside
  actual and exits non-zero.
- A passing run records "Certified for: 0.x" on the installed plugin (`orbit plugin show`) only
  when that namespace is installed at the **same manifest digest**; a different install drops
  the claim.

`orbit plugin scaffold <ns>` — a Python `exec` backend, one `read_only` tool with schemas, one
`kv` panel over it, one disabled auto-task, one skill stub, and two passing goldens.
`orbit tool scaffold` still writes the v1 sidecar pair and prints a deprecation naming its
replacement.

## 6. What opens up in the codebase

All landed. Extension points the standard opened: `PluginLoader` registers manifest tools with
`register_mcp(scope)` / `register_inactive` and the MCP surface advertises them;
`PluginBackend::{Exec,Mcp}` replaces the unsandboxed `ExternalTool` path for plugins; one
skipped `PluginGroup` `Commands` variant carries the derived clap tree; dynamic
`plugins.<ns>.<key>` config admission; two generic `GOVERNED_OPERATIONS` rows (§4.1);
`plugin.tool_call` in `DeterministicAction`; a provenance-aware disabled-plugin skip beside
`RETIRED_ROUTINE_JOBS`; `.orbit-managed-plugin-assets.json`; and one `plugins` dashboard group
with a generic renderer.

## 7. Phases (each its own PR into agent-main)

1. **Manifest + lifecycle** over the existing tool path — landed [ORB-12735]. orbit-graph's own
   migration is pending in that repository.
2. **Grants, sandboxed execution, `mcp` backend** (§4.1–§4.3) — landed [ORB-12736].
3. **Definitions, skills, config** (§4.5) — landed [ORB-12737].
4. **Derived CLI, dashboard panels, `plugin test`/`scaffold`** (§4.6, §4.7, §5) — landed.
5. **Provider plugins** — deferred: open `ProviderRegistry` and collapse the four provider
   vocabularies only when a second external provider exists.

## 8. Risks

- **Grant fatigue.** `--grant requested`/`all` for interactive use; explicit lists for scripts.
- **Schema-derived CLI ergonomics.** Nested inputs make poor flags; `cli:` overrides and
  `--input` cover the long tail. No full clap parity.
- **Catalog shadowing surprises.** A workspace file silently shadows a plugin definition of
  the same name; `orbit run show` printing the resolving layer is the mitigation.
- **`mcp` backend lifetime.** One child per caller context per runtime process, never reclaimed
  before the runtime ends, so an `orbit mcp serve` visiting many workspaces holds a child per
  workspace. Idle eviction is the first fix if that bites.
- **Compat table drift.** orbit-research's own Orbit allowlist should give way to
  `requires.orbit` plus the conformance run.

## Resolved questions (2026-09-20)

1. **Namespace.** `orbit.<ns>.*` is reserved for Orbit-originated plugins (§1); third parties
   get bare `<ns>.*`.
2. **Dashboard features.** No `web.features` in v1; panels and links only.
3. **Install model.** Global install per host with a committed `.orbit/plugins.yaml` pin; no
   vendoring (§3).
4. **Claude Code mirror.** `plugin/` stays the first-party mirror synced from
   `crates/orbit-core/assets`; it is not generated from installed plugins.
