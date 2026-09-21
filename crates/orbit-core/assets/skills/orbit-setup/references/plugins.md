# Installing plugins

A plugin is one directory holding a `plugin.yaml` (`schemaVersion: 2`,
`kind: Plugin`) that declares a namespace and what it contributes. Installing
one adds `<ns>.<verb>` tools to `orbit tool list`, makes them runnable with
`orbit tool run`, and — for a tool whose manifest gives it an `mcp_scope` —
advertises them to MCP clients as `<ns>_<verb>`. A plugin may also contribute
activities and jobs, scheduled chores, skills and its own configuration
section: see [What enabling installs](#what-enabling-installs).

Nothing here is required for task tracking or agent execution. Add a plugin
only when the user asks for the capability it provides.

## Two halves: the machine installs, the repository pins

A plugin lives once per machine under `~/.orbit/plugins/<ns>/<version>/`. Enable
state, grants, install paths and manifest digests are host-local and are never
synced. A repository commits only the pin file:

```yaml
# .orbit/plugins.yaml — committed to the repository
schemaVersion: 1
plugins:
  - name: graph
    version: "^0.4.1"
    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1
    enabled: true
```

Cloning a repository therefore does not make its plugins available. Plugin
trees are never vendored into a checkout, and `orbit plugin add` refuses a
source inside the current repository for exactly that reason.

## Install

```bash
orbit plugin validate ./my-plugin        # check the manifest before installing
orbit plugin add ./my-plugin             # install for this machine (disabled)
orbit plugin enable my-plugin            # put its tools on the surface
orbit plugin list                        # what is installed or pinned here
orbit plugin show my-plugin              # tools, and requested versus granted
```

`add` also accepts `git+<url>#<ref>` and a `.tar.gz` archive. `--enable`
installs and enables in one step. The tool registry is built when an Orbit
command starts, so an enable takes effect on the next command — restart a
long-lived `orbit mcp serve` to pick it up in that session.

On a machine that is joining a repository someone else configured:

```bash
orbit plugin sync --dry-run              # what this machine is missing
orbit plugin sync                        # install it
```

## What enabling installs

Beyond tools, `orbit plugin enable <ns>` applies everything else the manifest
declares:

| Contribution | Where it lands |
|---|---|
| `spec.definitions.activities`, `.jobs` | A `plugin:<ns>` catalog layer. A workspace file of the same name shadows the plugin's, and a shipped default is never displaced. `orbit run show` names the layer that resolved each `job:` / `activity:` reference and what it shadowed. |
| `spec.definitions.routines`, `.auto_tasks` | Seeded as `.orbit/routines/<ns>-<name>.yaml` and `.orbit/auto_tasks/<ns>-<name>.yaml` with `enabled: false` and a `# provenance: plugin:<ns>@<version>` header. |
| `spec.skills` | Linked from the install directory into `~/.agents/skills` and `~/.claude/skills`; unlinked on disable. |
| `spec.config` | The `[plugins.<ns>]` config section, validated by the plugin's own JSON Schema. |

Seeded schedules are inert until a human reviews one and sets `enabled: true`
— a plugin may not ship a schedule that is already on, and a routine it ships
may target only a job the same plugin ships or a shipped default. Say this
plainly when proposing a plugin: enabling it does not start anything.

An upgrade re-seeds a file that still matches what the plugin wrote. A file
edited since is preserved with a warning until `orbit plugin enable <ns>
--force` takes the plugin's version. Disabling or removing the plugin leaves
the files where they are and skips them with a reason naming the plugin, which
`orbit routine list` and `orbit auto-task list` show; tasks minted from a
plugin auto-task carry `plugin:<ns>`.

Configure a plugin the ordinary way — `orbit config set plugins.<ns>.<key>
<value>` accepts only keys the plugin declares, and `orbit config show` prints
the value with its workspace/global provenance. A value the plugin's schema
rejects refuses *that plugin* at load, naming the key.

## When a plugin is not serving its tools

```bash
orbit plugin doctor
```

One row per plugin, naming the step that would make it active, plus a row for
any skill link whose target has gone. The four states:

| Status | What it means |
|---|---|
| `active` | Installed, enabled, and serving its tools. |
| `disabled` | Installed but not enabled — run `orbit plugin enable <ns>`. |
| `missing` | Pinned by the repository, not installed here — run `orbit plugin sync`. |
| `inactive` | Enabled but refused at load: the manifest no longer loads, its `requires.orbit`/`requires.host_api` does not hold on this machine, its namespace collides, a definition it ships breaks the rules above, or its `[plugins.<ns>]` config fails its own schema. `orbit plugin show <ns>` names the reason. |

A plugin fails closed on its own: one broken plugin never takes down the
built-in tools, the runtime, or another plugin.

## Permissions and grants

The manifest's `spec.permissions` block is a **request**, never a grant. The
`--grant` flags at `orbit plugin enable` are the only source of authority, and
`orbit plugin show` prints requested and granted side by side. Record only the
grants the user authorizes.

| Grant | What the manifest asks for | What granting it does |
|---|---|---|
| `fs` | `permissions.fs.read` / `.write` | Opens exactly those paths to the sandboxed backend. |
| `network` | `permissions.network: loopback\|any` | Lets the backend reach the network; without it, TCP is refused. |
| `env_pass` | `permissions.env_pass` | Copies those variables from Orbit's environment into the child. |
| `orbit_tools` | `permissions.orbit_tools` | Lets the backend call those Orbit tools back through `orbit tool run`, and nothing else. |
| `unsandboxed` | `backend.sandbox: none` | Runs the backend with no confinement at all. |

A plugin that requests a grant the host has not given registers its tools
**inactive**: a call is refused with a diagnostic naming the grant and the
`orbit plugin enable --grant …` that would fix it. Granting is per host and
never synced.

Backends run confined: on Linux under a Landlock ruleset built from the
granted paths, on macOS under `sandbox-exec`. A write outside the granted
profile fails; so does a TCP connection without the `network` grant. A
`sandbox: none` plugin needs `unsandboxed` and is reported by
`orbit plugin doctor` for as long as it stays enabled.

Callbacks are the only way back into Orbit: the child carries
`ORBIT_ALLOWED_TOOLS` with the granted intersection, and `orbit tool run`
refuses anything outside it — the plugin's own good behaviour is not the
boundary.

Who may *call* a plugin tool is decided by its `execution_kind`, not by the
manifest: a `read_only` tool is callable by any caller Orbit can identify, and
a `mutating` tool by an operator or a sanctioned run — an agent reaches one
only when the task's `required_tools` or the activity's allowlist names it. A
scripted call with no identity at all is refused; `ORBIT_OPERATOR=1` is the
documented escape hatch, and its use is audited.

Treat an installed plugin as code the user has chosen to run on their machine,
and say so when proposing one: the sandbox bounds what a backend can reach, it
does not vouch for what the backend does inside those bounds.

## Backends

| `backend.type` | How Orbit runs it |
|---|---|
| `exec` | One process per call. Orbit writes `{"schema_version":1,"tool":…,"input":…,"context":{…}}` on stdin and reads `{"ok":true,"output":…}` or `{"ok":false,"error":{…}}` on stdout. A non-zero exit, non-JSON stdout, or output failing the tool's `output_schema` is a tool error — never a partial result. |
| `mcp` | The plugin ships a stdio MCP server. Orbit spawns it once per runtime process, checks its `tools/list` against the manifest (a disagreement refuses startup, naming the tool), and proxies each `<ns>.<verb>` call as `tools/call`. A crashed or unresponsive server is a tool error within `backend.timeout_ms`, and the next call respawns it. |

## Migrating an existing external tool

The older form — one executable plus one `*.orbit-tool.yaml` sidecar per tool,
registered with `orbit tool add` — keeps working unchanged. To move a set of
sidecars onto one manifest:

```bash
orbit plugin migrate ./bin/my-tool --version 0.1.0 --out-dir ./my-plugin
orbit plugin validate ./my-plugin
```

Review the generated manifest before installing it: migration cannot know
which tools are read-only, so every migrated tool is marked `mutating`.
