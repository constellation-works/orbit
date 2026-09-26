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

A plugin lives once per machine under `~/.orbit/plugins/<ns>/<version>/`.
Enable state, grants, install paths and manifest digests are host-local and are
never copied into the repository. Sync treats the pin's `enabled:` field as an
instruction for that shared host state. A repository commits only the pin file:

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
orbit plugin validate ./my-plugin --render  # inspect the manifest and effective backend profile
orbit plugin add ./my-plugin             # install for this machine (disabled)
orbit plugin upgrade my-plugin           # update and review permission changes
orbit plugin enable my-plugin            # put its tools on the surface
orbit plugin list                        # what is installed or pinned here
orbit plugin show my-plugin              # tools, panels, and requested versus granted
```

Run `orbit plugin test` before `add`. It applies ordinary template paths on its
own. When the manifest asks for an unconfined backend, an absolute write root,
`network: any`, or `env_pass`, it stops and prints the requested grants instead
of running them. See [Certifying a plugin](#certifying-a-plugin-for-this-orbit).

`add` also accepts `git+<url>#<ref>` and a `.tar.gz` archive. `--enable`
installs and enables in one step. The tool registry is built when an Orbit
command starts, so an enable takes effect on the next command — restart a
long-lived `orbit mcp serve` to pick it up in that session.

`add --grant …` requires `--enable`. For an installed namespace, `plugin
upgrade <ns> [source]` uses the recorded source by default and prints the old
and new permission requests. If filesystem roots, network mode, environment
names, Orbit-tool callbacks or the sandbox widened, Orbit disables the plugin,
clears its grants and prints the `plugin enable --grant …` re-consent command.
An unchanged or narrower request keeps the existing state. Supplying
`upgrade --grant …` is explicit re-consent and enables the new manifest.

On a machine that is joining a repository someone else configured:

```bash
orbit plugin sync --dry-run              # what this machine is missing
orbit plugin sync                        # install it
orbit plugin sync --grant fs,network     # consent to requested grants while enabling
```

Sync also applies each pin's `enabled:` state and reconciles an enabled
plugin's seeded definitions into the current workspace. Enable state is shared
by every workspace on the host, so a later workspace sync can change it. A pin
whose manifest requests grants remains disabled unless this invocation supplies
the complete reviewed set with `--grant`; repository content is never consent.

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
| `boolean` | `--kebab-case`, or `--kebab-case=false` to set it explicitly |
| `array` of scalars | `--kebab-case <VALUE>`, repeated |
| `object` or an array of objects | `--kebab-case-json '<JSON>'` |
| named in the manifest's `cli.positional` | a positional argument, in that order, alongside its ordinary `--kebab-case` flag |

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
under `Plugins:`.

## The dashboard's Plugins tab

`orbit web serve` grows a Plugins tab listing what is installed here, each plugin's state
and diagnostic, and the panels its manifest declares. A panel is one `read_only` tool's
JSON drawn by a generic renderer — `kv` (label/value rows), `table` (an array of objects),
`markdown` (sanitised) or `json` — plus plain link tiles to plugin-hosted UIs (`http://` or
`https://` only). No plugin ships JavaScript, and a manifest that declares a panel over a
*mutating* tool is refused by `orbit plugin validate`, naming the panel. Installing, enabling and granting stay on the
CLI: the dashboard reads.

Panel reads are cached and single-flighted by the web server for 30 seconds by default, so
several tabs produce one audited backend call in that window. A panel may set `refresh_ms`
between 1000 and 3600000 in `spec.web.panels[]`; failed reads are retried rather than cached.
Serialized output above 256 KiB is returned as a bounded prefix with a truncation diagnostic.
Plugin lifecycle changes made by the CLI appear on the next dashboard request without
restarting `orbit web serve`.

## Certifying a plugin for this Orbit

```bash
orbit plugin scaffold demo            # creates ./demo with backend, tool, panel, skill, goldens
orbit plugin scaffold demo --dir /path/to/demo # choose an explicit output directory
orbit plugin validate ./demo --render # manifest plus effective profile/env
orbit plugin test ./demo              # run its goldens through the real protocol
orbit plugin test ./demo --case status_reports_ready
orbit plugin test ./demo --update-goldens
```

`orbit plugin test` runs each `spec.tests` golden — `{name, tool, input, secrets, expect}` —
in a temp workspace and compares `expect.output` exactly, or matches the backend code in
`expect.error: {code: ...}`. String values in `input` and `expect.output` substitute
`{{workspace}}` and `{{plugin_root}}` against that hermetic run. A case's optional
`secrets: {<name>: <value>}` supplies fixture values for declared secrets, delivered like
stored ones at version `fixture`; a declared secret the case leaves out is unset, and the
host's real secrets are never read. It exits non-zero when a
case fails, naming it. `--case <name>` runs one case without certifying the full suite;
`--update-goldens` replaces mismatched output expectations with actual output. Manifest
template paths (`{{workspace}}`, `{{plugin_root}}`, `{{plugin_state}}`,
`{{config.<key>}}`), `network: loopback`, and `orbit_tools` are applied as the manifest
requests them, inside that temp workspace, so those paths do not touch the operator's
Orbit state.

The command refuses, and prints the requested grant set, when the manifest asks for an
unconfined backend (`backend.sandbox: none`), an absolute `fs.write` root that is not a
template, `network: any`, or any `env_pass` variable. Re-run with `--accept-requested` to
test under that requested profile, or with `--grant` using the same names as `orbit plugin
enable --grant` (`fs`, `network`, `env_pass`, `orbit_tools`, `unsandboxed`). The `--grant`
list has to name each of those requests. Either flag applies only to this run and does not
record a host grant; `orbit plugin enable --grant` is still what authorizes the installed
plugin.

A passing run records this Orbit's version on the installed plugin, and `orbit plugin show <ns>` prints it as `Certified for: <version>`.
The record is only written when the directory tested is the installed one (same manifest
digest); reinstalling a changed manifest drops the claim.

`orbit plugin scaffold <namespace>` creates `./<namespace>` in the current directory;
`--dir <path>` creates the plugin at that path instead. Plugin installation refuses
sources inside a workspace repository, so move the scaffold outside the repository
or give `--dir` an external path before installing it.

`orbit plugin validate <dir> --render` uses the same profile and child-environment builders
as a real call, but starts no backend. In a registered workspace it renders for that
workspace. From an unregistered directory it uses host-level config and does not
initialize a workspace there. Pass the global `--workspace <selector>` option when
rendering needs workspace config or to inspect another registered checkout.

`orbit tool scaffold` still writes the older executable-plus-sidecar form for one more
release and prints a deprecation pointing here.

## When a plugin is not serving its tools

```bash
orbit plugin doctor
```

One row per plugin, naming the step that would make it active, plus a row for
any skill link whose target has gone and one for each declared secret that is
not set (see [Secrets](#secrets)). The four states:

| Status | What it means |
|---|---|
| `active` | Installed, enabled, and serving its tools. |
| `disabled` | Installed but not enabled — run `orbit plugin enable <ns>`. |
| `missing` | Pinned by the repository, not installed here — run `orbit plugin sync`. |
| `inactive` | Enabled but refused at load: the manifest no longer loads, its `requires.orbit`/`requires.host_api` does not hold on this machine, its namespace collides, a definition it ships breaks the rules above, or its `[plugins.<ns>]` config fails its own schema. `orbit plugin show <ns>` names the reason. |

A plugin fails closed on its own: one broken plugin never takes down the
built-in tools, the runtime, or another plugin.

## Permissions and grants

The manifest's `spec.permissions` block is a **request**, never a grant.
`--grant` on an enabling add, enable, upgrade or sync is the only source of authority, and
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

Callbacks are the only way back into Orbit. The host issues a per-call session
(token plus the child's pid and start time) when it spawns the backend;
`ORBIT_PLUGIN` and `ORBIT_ALLOWED_TOOLS` are information for the child, not the
gate. `orbit tool run` and MCP `tools/call` look up that session — by the token
or by process ancestry if the child unsets its environment — and refuse anything
outside the recorded grants. Unsetting or rewriting the variables cannot expand
the set, and the plugin's own good behaviour is not the boundary.

Who may *call* a plugin tool is decided by its `execution_kind`, not by the
manifest: a `read_only` tool is callable by any caller Orbit can identify, and
a `mutating` tool by an operator or a sanctioned run — an agent reaches one
only when the task's `required_tools` or the activity's allowlist names it. A
scripted call with no identity at all is refused; `ORBIT_OPERATOR=1` is the
documented escape hatch, and its use is audited.

Treat an installed plugin as code the user has chosen to run on their machine,
and say so when proposing one: the sandbox bounds what a backend can reach, it
does not vouch for what the backend does inside those bounds.

## Secrets

A plugin that talks to an authenticated service declares the credentials it
needs by name in its manifest, and the user sets the values on this machine.
Never put a credential in `[plugins.<ns>]` config or ask for it in a prompt.

```yaml
spec:
  secrets:
    - name: refresh_token          # lowercase letter, then a-z 0-9 _ - (max 64)
      description: OAuth refresh token.
      rotatable: true              # the backend may replace it
```

```bash
orbit plugin secret set <ns> refresh_token < token.txt   # or run it in a terminal to be prompted
orbit plugin secret list <ns>                            # names, set/unset, updated-at — never values
orbit plugin secret rm <ns> refresh_token
```

- The value comes from stdin or a prompt that does not echo. A value typed on
  the command line is refused: argv is readable by other processes and lands
  in shell history. Only names the installed manifest declares can be set.
- Values live in `~/.orbit/state/plugin-secrets/<ns>.json` (mode `0600`). No
  plugin backend can read that directory, its own file included; Orbit reads a
  value and hands it over. A secret value never appears in CLI output,
  `plugin show`, the dashboard, logs or audit rows.
- Each call carries the plugin's own declared secrets that are set, in the
  request itself: an `exec` backend reads `context.secrets` on stdin, an `mcp`
  backend `params._meta.orbit.secrets` on `tools/call`. Each entry is
  `{"value": …, "version": …}`, where `version` is opaque and changes on every
  write. An unset secret is simply absent, so a backend must handle that
  (`secrets` itself is present exactly when the manifest declares secrets).
  Values are read per call, so a `secret set` reaches a running `mcp` server's
  next call. Nothing goes into the backend's environment or argv, no grant is
  needed, and a plugin never receives another plugin's secrets. The call's
  audit row records which names were delivered, never values.
- `orbit plugin enable` names every declared secret that is still unset, and
  `orbit plugin doctor` keeps reporting them. `upgrade` keeps the secrets the
  new manifest still declares; `remove` deletes the plugin's secrets unless
  `--record-only` is passed.
- These are operator commands: a plugin backend calling them is refused
  `policy_denied`.
- A backend rotates a secret it declares `rotatable: true` by answering with
  `secret_updates` — beside `ok`/`output` on `exec` stdout, as
  `result._meta.orbit.secret_updates` on an `mcp` result:
  `{"<name>": {"value": "<new>", "expected_version": "<version it was delivered>"}}`
  (`null` writes only while the secret is unset). Orbit applies it with
  compare-and-swap, so of two calls rotating from one version exactly one
  wins. An undeclared or non-rotatable name, a malformed entry or a stale
  version is refused with a warning that names the secret but never a value,
  and the call still returns its output (or its error). Updates are applied
  even when the call reports `ok: false`, so a token refreshed before a failed
  request is kept. The next call's `secrets` shows what is stored, and the
  audit row records each name as `applied` or `refused`. `orbit plugin test`
  refuses every update.

  OAuth refresh with X, which invalidates the old refresh token on every
  refresh:

  ```json
  {"ok": true, "output": {"posted": "…"},
   "secret_updates": {"refresh_token": {"value": "<new refresh token>",
                                        "expected_version": "<context.secrets.refresh_token.version>"}}}
  ```

## Backends

| `backend.type` | How Orbit runs it |
|---|---|
| `exec` | One process per call. Orbit writes `{"schema_version":1,"tool":…,"input":…,"context":{…}}` on stdin and reads `{"ok":true,"output":…}` or `{"ok":false,"error":{…}}` on stdout. A non-zero exit, non-JSON stdout, or output failing the tool's `output_schema` is a tool error — never a partial result. |
| `mcp` | The plugin ships a stdio MCP server. Orbit spawns one per caller context per runtime process — a caller in another workspace, or with a different allowed-tools intersection, gets its own child rather than one confined to the first caller's workspace — checks its `tools/list` against the manifest (a disagreement refuses startup, naming the tool), and proxies each `<ns>.<verb>` call as `tools/call`. A crashed or unresponsive server is a tool error within `backend.timeout_ms`, and the next call respawns it. |

## Python backends with dependencies

The scaffold's backend uses only the standard library. A Python backend that
needs third-party packages ships as a uv project, and uv builds the environment
under the plugin's state directory on first call. Do not commit a virtualenv:
it symlinks its interpreter, and `plugin add` refuses a tree that contains a
symbolic link.

```text
my-plugin/
  plugin.yaml
  pyproject.toml     # [project] dependencies; [tool.uv] package = false
  uv.lock            # committed: pins every dependency
  .python-version    # optional: an interpreter uv may download into plugin state
  backend/main.py
  bin/my-plugin      # the shim below, executable
```

```yaml
spec:
  requires:
    programs: [uv]
  backend:
    type: exec
    command: bin/my-plugin
    timeout_ms: 60000        # the first call installs the environment
  permissions:
    fs:
      write: ["{{plugin_state}}"]
    network: any             # the first sync downloads the locked wheels
```

```sh
#!/bin/sh
set -eu
state="${ORBIT_PLUGIN_STATE:?ORBIT_PLUGIN_STATE is not set}"
export UV_PROJECT_ENVIRONMENT="$state/venv"
export UV_CACHE_DIR="$state/uv-cache"
export UV_PYTHON_INSTALL_DIR="$state/uv-python"
export UV_PYTHON_BIN_DIR="$state/uv-python/bin"
unset PYTHONPYCACHEPREFIX
export PYTHONDONTWRITEBYTECODE=1     # the plugin root is read-only
export TMPDIR="$state/tmp"
mkdir -p "$TMPDIR"
exec uv run --frozen --exact --no-dev --quiet --project "$ORBIT_PLUGIN_ROOT" \
  python "$ORBIT_PLUGIN_ROOT/backend/main.py"
```

Enable it with `orbit plugin enable my-plugin --grant fs,network`, and certify
it with `orbit plugin test ./my-plugin --grant network`. `uv` is resolved from
`PATH` when the plugin is enabled and granted at that path. `orbit plugin show`
lists it under Programs, and `orbit plugin doctor` reports it when it has gone.
No other grant or sandbox exception is involved. uv reads the plugin root, its
own binary, and the system runtime directories the sandbox already allows.
Every write lands under `{{plugin_state}}`. On Linux, Landlock refuses a few
reads uv can do without, such as its user configuration under `$HOME` and some
`/proc/self` files; uv continues. Do not set `UV_NO_CONFIG=1`, because it also
makes uv ignore `.python-version`. Under Landlock the interpreter uv picks must
also be readable. A system Python qualifies, and so does one uv downloads into
plugin state. A Python installed under `$HOME`, for example by pyenv, does not.
On such a host, add `export UV_PYTHON_PREFERENCE=only-managed` to the shim so uv
always uses an interpreter it downloaded into plugin state.

**Network.** The first call downloads the locked wheels, and the interpreter
when `.python-version` names one the host lacks. The first call after a
lockfile change downloads whatever that change added. Both need `network: any`
and the `network` grant; without them a cold call fails with a connection error
from uv. Other calls use the cache and open no connection. A plugin that ships
its wheels in its own tree, locked through `[tool.uv.sources]` path entries,
needs no network at all.

**Cost.** On one Linux host with Orbit 0.24.0, `orbit <ns> <verb>` took these
times:

| Call | Time |
|---|---|
| Cold, 11 wheels (19 MB) into an empty state directory | about 0.45 s |
| Cold, also downloading a CPython | about 2 s |
| Warm | 0.14–0.15 s |
| The same environment's `python` without uv | 0.11–0.12 s |

`--offline` and `--no-sync` add nothing measurable, and `--offline` breaks the
cold call. Concurrent cold calls are safe because uv locks its cache and the
environment.

**Upgrades.** The environment is not part of the install. Every call runs
`uv run --frozen --exact`, which compares the state-directory environment with
the installed `uv.lock`. After `orbit plugin upgrade` installs a changed lock,
the next call adds, upgrades and removes packages to match it. Disable Python
bytecode caching as in the shim above: a shared `PYTHONPYCACHEPREFIX` can reuse
old bytecode after an upgrade when the old and new source have the same size
and second-resolution timestamp, even though uv installed the new wheel. The
manifest digest covers `plugin.yaml` only. An upgrade that changes just the
lockfile therefore reports the requested permissions as unchanged and keeps
the plugin's grants. Review the lock diff
before upgrading, because it is new code even though it is not a new
permission. `orbit plugin remove --purge-state` deletes the environment and
cache with the rest of the plugin's state.

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
Migration also cannot prove first-party provenance. An old
`orbit.graph.recommend` sidecar therefore produces a manifest with namespace
`graph`, no `publisher` or `origin`, and a `graph.recommend` tool. A local
directory's own Git remote is never proof of origin. The author can use the
reserved `orbit.graph.*` namespace only by adding
`publisher: constellation-works` and `origin: orbit` and then distributing the
plugin through a verified `git+https://github.com/constellation-works/...`
source; a manifest digest bundled into an Orbit release is the other trusted
path.
