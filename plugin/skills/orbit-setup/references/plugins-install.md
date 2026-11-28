# Plugins: installing

## Contents

- [Two halves: the machine installs, the checkout pins](#two-halves-the-machine-installs-the-checkout-pins)
- [Install](#install)
- [Switching a plugin off in one workspace](#switching-a-plugin-off-in-one-workspace)
- [What enabling installs](#what-enabling-installs)
- [The `orbit <ns>` command group](#the-orbit-ns-command-group)
- [The dashboard's Plugins tab](#the-dashboards-plugins-tab)

A plugin is one directory holding a `plugin.yaml` (`schemaVersion: 2`,
`kind: Plugin`) that declares a namespace and what it contributes. In a plugin
source that directory is `.orbit-plugin/`, the plugin root: `orbit plugin add`,
`upgrade`, `sync`, `validate` and `test` resolve a checkout, `git+` clone or
archive to its `.orbit-plugin/`, and only that directory is installed.

A source
whose only manifest is a top-level `plugin.yaml` is refused, naming the expected
`.orbit-plugin/plugin.yaml` path. Installing
one adds `<ns>.<verb>` tools to `orbit tool list`, makes them runnable with
`orbit tool run`, and — for a tool whose manifest gives it an `mcp_scope` —
advertises them to MCP clients as `<ns>_<verb>`. A plugin may also contribute
activities and jobs, scheduled chores, skills and its own configuration
section: see [What enabling installs](#what-enabling-installs).

Nothing here is required for task tracking or agent execution. Add a plugin
only when the user asks for the capability it provides.

For a login service acting on explicit approvals, use [scoped application
authority](applications.md). It binds host-issued credentials to an exact
workspace, tool inputs, manifest digest and completion callbacks without
granting the service Operator/Runner or making approval tools agent-callable.

## Two halves: the machine installs, the checkout pins

A plugin lives once per machine under `~/.orbit/plugins/<ns>/<version>/`.
Enable state, grants, install paths and manifest digests are host-local and are
never copied into the repository. A workspace can switch a host-enabled plugin
off for itself (see [Switching a plugin off in one workspace](#switching-a-plugin-off-in-one-workspace)).
A checkout keeps only a pin file.

Like the rest of `.orbit/`, it is per-user
checkout state that git ignores, so it is not shared by committing or cloning;
copy it to another checkout deliberately:

```yaml
# .orbit/plugins.yaml — per-checkout, gitignored
schemaVersion: 1
plugins:
  - name: graph
    version: "^0.4.1"
    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1
    enabled: true
```

Cloning a repository therefore brings neither pins nor plugins. Plugin
trees are never vendored into a checkout, and `orbit plugin add` refuses a
source inside the current repository for exactly that reason. A plugin root
that contains a symbolic link is refused, naming the entry, because the install
copy would follow it and place the target's bytes inside the plugin root. Links
elsewhere in the source repository are not walked and do not block an install.

## Install

```bash
orbit plugin scaffold demo               # start a new plugin from a working example
orbit plugin validate ./my-plugin --render  # inspect the manifest and effective backend profile
orbit plugin add ./my-plugin             # install for this machine (disabled)
orbit plugin upgrade my-plugin ./my-plugin  # explicit source; review permission changes
orbit plugin enable my-plugin            # put its tools on the surface
orbit plugin list                        # what is installed or pinned here
orbit plugin show my-plugin              # tools, panels, and requested versus granted
```

For machine-readable inspection, use `orbit plugin list --format json` or
`orbit plugin show my-plugin --format json`. Plugin records in these commands
and add/enable/disable/upgrade receipts include `build`, `programs` (recorded
paths, grants and resolution problems), `tools[].agent_callable` and
`panels[].refresh_ms`.

The HTTP plugin records expose the same data, plus
`enabled` and `host_orbit_version`; their `source` and `install_path` abbreviate
the home directory to `~`, while CLI paths remain literal. The HTTP list also
carries session-specific enable/disable `capabilities`.

Run `orbit plugin test` before `add`. It applies ordinary template paths on its
own. When the manifest asks for an unconfined backend, an absolute write root,
`network: any`, or `env_pass`, it stops and prints the requested grants instead
of running them. See [Certifying a plugin](plugins-certify.md#certifying-a-plugin-for-this-orbit).

`add` also accepts `git+<url>#<ref>` and a `.tar.gz` archive. A
`git+<url>#<full commit id>` source whose manifest declares `spec.build` builds
its backend at install time and needs `--allow-build` (see
[Backends built from source](plugins-backends.md#backends-built-from-source)). `--enable`
installs and enables in one step.

The tool registry is built when an Orbit
command starts, so an enable takes effect on the next command — restart a
long-lived `orbit mcp serve` to pick it up in that session.

`add --grant …` requires `--enable`. For an installed namespace, `plugin
upgrade <ns> <source>` requires an explicit, non-empty source and prints the old
and new permission requests. Omitting the source refuses before fetching or
installing anything. The recorded database source is informational and can be
rewritten by a plugin with `orbit_tools`; it is not bound by the host-owned
grant witness.

Review and supply the intended source on every upgrade instead
of copying the recorded value without checking it.

If filesystem roots, network mode, environment names, Orbit-tool callbacks or
the sandbox widened, Orbit disables the plugin,
clears its grants and prints the `plugin enable --grant …` re-consent command.
An unchanged or narrower request keeps the existing state. Supplying
`upgrade --grant …` is explicit re-consent and enables the new manifest.

On a checkout whose pin file names plugins this machine has not installed (for
example, a pin file copied from another checkout):

```bash
orbit plugin sync --dry-run              # what this machine is missing
orbit plugin sync                        # install it
orbit plugin sync --grant fs,network     # consent to requested grants while enabling
```

Sync also applies each pin's `enabled:` state and reconciles an enabled
plugin's seeded definitions into the current workspace. `enabled: false` switches
the plugin off in this workspace only; it never disables the host row, so
syncing one workspace never changes another.

`enabled: true` turns a workspace
toggle back on, and enables a host-disabled plugin only with permission review:
a pin whose manifest requests grants remains disabled unless this invocation
supplies the complete reviewed set with `--grant`; pin file content is never
consent.

## Switching a plugin off in one workspace

```bash
orbit plugin disable graph --scope workspace   # off here; other workspaces keep it
orbit plugin enable graph --scope workspace    # back on here
```

The host row is the ceiling and the workspace toggle can only narrow it. The
toggle is `[plugin_enablement] graph = false` in this workspace's
`.orbit/config.toml`; an unset entry inherits the host state. Switched off, the
plugin's tools, job and activity catalog entries, seeded routines and
auto-tasks, and dashboard panels are gone in this workspace, and a call refuses
with `plugin_disabled_in_workspace`.

`enable --scope workspace` while the host
row is disabled refuses with `plugin_disabled_on_host` and changes nothing:
enable it on the host first, with its `--grant` review. Skill links stay,
because they are host-level. A running MCP server or dashboard picks the change
up on its next call. `[plugin_enablement]` is refused in the global
`config.toml`.

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
--force` takes the plugin's version. Disabling or removing the plugin (on the
host, or `--scope workspace`) leaves the files where they are and skips them.

`orbit routine list`, `orbit auto-task list`, MCP `orbit.auto_task` action `list`
(opt-in `--toolset automation` in the operator profile) and
the dashboard hide them; `--all` (MCP `include_inactive_plugins`, the
dashboard's "Show hidden") lists them as inactive with a reason naming the
plugin, and `show <name>` still resolves them. Re-enabling brings them back
with no re-seed. Tasks minted from a plugin auto-task carry `plugin:<ns>`.

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
| `boolean` | `--kebab-case`, or `--kebab-case=false` to set it explicitly |
| `array` of `string`, `integer` or `number` | `--kebab-case <VALUE>`, repeated |
| `object`, any other `array` (objects, booleans, untyped items), or untyped | `--kebab-case-json '<JSON>'` (so `flags: {type: array, items: {type: boolean}}` uses `--flags-json '[true,false]'`) |
| named in the manifest's `cli.positional` | a positional argument, in that order, alongside its ordinary `--kebab-case` flag; a JSON-shaped property keeps only its `--<name>-json` flag and has no positional form |

`--input '<json>'`, `--input-file` and `--dry-run` are always accepted, and `--input`
overrides every flag — that is the escape hatch for a shape no flag expresses. A property
whose flag would collide with one Orbit owns keeps its place in the schema and is reached
through `--input`. Add `--explain` to print the equivalent `orbit tool run <ns>.<verb>
--input '<json>'` command without executing the backend.

A boolean's explicit value must use `=`: `--kebab-case value` never treats `value` as the
flag's own, so a boolean flag can sit directly in front of a positional argument without
swallowing it.

Only an enabled, loading plugin has a group. `orbit <ns>` for a disabled one is an unknown
command, not a silent no-op, and `orbit --help` lists the groups a machine actually has
under `Plugins:`. A plugin switched off in the current workspace keeps its group, and a
call refuses with `plugin_disabled_in_workspace`.

## The dashboard's Plugins tab

`orbit web serve` grows a Plugins tab listing the plugins installed on this host, each
plugin's state and diagnostic, and the panels its manifest declares. The **Installed**
view shows those plugin cards. A panel is one `read_only` tool's JSON drawn by a generic
renderer — `kv` (label/value rows), `table` (an array of objects), `markdown` (sanitised)
or `json`.

Plugins can also declare local pages. When a plugin is active, each declared page appears
as a tab beside **Installed**; its page tile on the Installed card opens the same view.
An active page loads from the installed plugin bundle in an iframe sandbox that allows
scripts but does not grant same-origin access. These plugin-authored pages are separate
from generic panels, which render tool data. Link tiles remain separate too: they open
plugin-hosted UIs at `http://` or `https://` addresses.

Panels do not execute plugin JavaScript. A manifest that declares a panel over a
*mutating* tool is refused by `orbit plugin validate`, naming the panel. Installing,
enabling and granting stay on the CLI: the dashboard reads.

Panel reads are cached and single-flighted by the web server for 30 seconds by default, so
several tabs produce one audited backend call in that window. A panel may set `refresh_ms`
between 1000 and 3600000 in `spec.web.panels[]`; failed reads are retried rather than cached.
Serialized output above 256 KiB is returned as a bounded prefix with a truncation diagnostic.
Plugin lifecycle changes made by the CLI appear on the next dashboard request without
restarting `orbit web serve`.
