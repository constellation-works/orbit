# Installing plugins

A plugin is one directory holding a `plugin.yaml` (`schemaVersion: 2`,
`kind: Plugin`) that declares a namespace and a set of tools. Installing one
adds `<ns>.<verb>` tools to `orbit tool list`, makes them runnable with
`orbit tool run`, and — for a tool whose manifest gives it an `mcp_scope` —
advertises them to MCP clients as `<ns>_<verb>`.

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

## When a plugin is not serving its tools

```bash
orbit plugin doctor
```

One row per plugin, naming the step that would make it active. The four states:

| Status | What it means |
|---|---|
| `active` | Installed, enabled, and serving its tools. |
| `disabled` | Installed but not enabled — run `orbit plugin enable <ns>`. |
| `missing` | Pinned by the repository, not installed here — run `orbit plugin sync`. |
| `inactive` | Enabled but refused at load: the manifest no longer loads, its `requires.orbit`/`requires.host_api` does not hold on this machine, or its namespace collides. `orbit plugin show <ns>` names the reason. |

A plugin fails closed on its own: one broken plugin never takes down the
built-in tools, the runtime, or another plugin.

## Permissions

The manifest's `spec.permissions` block is a **request**, never a grant. The
`--grant` flags at `orbit plugin enable` are the only source of authority, and
`orbit plugin show` prints requested and granted side by side. Record only the
grants the user authorizes.

Who may *call* a plugin tool is decided by its `execution_kind`, not by the
manifest: a `read_only` tool is callable by any caller Orbit can identify, and
a `mutating` tool by an operator or a sanctioned run — an agent reaches one
only when the task's `required_tools` or the activity's allowlist names it. A
scripted call with no identity at all is refused; `ORBIT_OPERATOR=1` is the
documented escape hatch, and its use is audited.

In this release, grants are recorded but not enforced and plugin backends are
not sandboxed. Treat an installed plugin as code the user has chosen to run on
their machine, and say so when proposing one.

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
