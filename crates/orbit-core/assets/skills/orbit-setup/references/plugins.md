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
source inside the current repository for exactly that reason. A source that
contains a symbolic link is refused, naming the entry, because the install
copy would follow it and place the target's bytes inside the plugin root.

## Install

```bash
orbit plugin scaffold demo               # start a new plugin from a working example
orbit plugin validate ./my-plugin        # check the manifest before installing
orbit plugin add ./my-plugin             # install for this machine (disabled)
orbit plugin enable my-plugin            # put its tools on the surface
orbit plugin list                        # what is installed or pinned here
orbit plugin show my-plugin              # tools, panels, and requested versus granted
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
| `spec.skills` | Linked from the install directory into `~/.agents/skills` and `~/.claude/skills` as `<plugin-namespace>-<skill-directory>`; `plugin validate` reports the ID before install, and disable removes only that plugin's links. |
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

## The `orbit <ns>` command group

Enabling a plugin also gives it a command group: `orbit <ns> <verb>` is built at startup
from each tool's `input_schema`, and is the same audited operation as
`orbit tool run <ns>.<verb>` — same dispatch, same dry-run, same audit row. The flags come
from the top level of the schema and nowhere deeper:

| Property shape | Flag |
|---|---|
| `string` | `--kebab-case <VALUE>`; a schema `enum` is offered as the allowed values |
| `integer` / `number` | `--kebab-case <N>` |
| `boolean` | `--kebab-case`, or `--kebab-case false` |
| `array` of scalars | `--kebab-case <VALUE>`, repeated |
| `object` or an array of objects | `--kebab-case-json '<JSON>'` |
| named in the manifest's `cli.positional` | a positional argument, in that order |

`--input '<json>'`, `--input-file` and `--dry-run` are always accepted, and `--input`
overrides every flag — that is the escape hatch for a shape no flag expresses. A property
whose flag would collide with one Orbit owns keeps its place in the schema and is reached
through `--input`.

Only an enabled, loading plugin has a group. `orbit <ns>` for a disabled one is an unknown
command, not a silent no-op, and `orbit --help` lists the groups a machine actually has
under `Plugins:`.

## The dashboard's Plugins tab

`orbit web serve` grows a Plugins tab listing what is installed here, each plugin's state
and diagnostic, and the panels its manifest declares. A panel is one `read_only` tool's
JSON drawn by a generic renderer — `kv` (label/value rows), `table` (an array of objects),
`markdown` (sanitised) or `json` — plus plain link tiles to plugin-hosted UIs (`http://` or
`https://` only). No plugin ships JavaScript, and a manifest that declares a panel over a
*mutating* tool is refused by `orbit plugin validate`, naming the panel. Installing, enabling and granting stay on the
CLI: the dashboard reads.

## Certifying a plugin for this Orbit

```bash
orbit plugin scaffold demo            # a starter plugin: backend, tool, panel, skill, goldens
orbit plugin validate ./demo          # manifest, paths, namespace, panels
orbit plugin test ./demo              # run its goldens through the real protocol
```

`orbit plugin test` runs each `spec.tests` golden — `{name, tool, input, expect.output}` —
in a temp workspace with the grants the manifest requests, compares the output exactly, and
exits non-zero when a case fails, naming it. A passing run records this Orbit's version on
the installed plugin, and `orbit plugin show <ns>` prints it as `Certified for: <version>`.
The record is only written when the directory tested is the installed one (same manifest
digest); reinstalling a changed manifest drops the claim.

`orbit tool scaffold` still writes the older executable-plus-sidecar form for one more
release and prints a deprecation pointing here.

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
| `env_pass` | `permissions.env_pass` | Copies those variables from Orbit's environment into the child. `ORBIT_*` names are reserved for Orbit's own envelope: `validate_structure` refuses a manifest that names one, and the privilege-bearing ones (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) can never reach the child even so. |
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

Callbacks are the only way back into Orbit: the child carries `ORBIT_PLUGIN`
and an informational `ORBIT_ALLOWED_TOOLS` copy of the granted intersection.
`orbit tool run` looks up that plugin's recorded grants and refuses anything
outside them — unsetting or rewriting the variable cannot expand the set, and
the plugin's own good behaviour is not the boundary.

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
