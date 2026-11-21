---
title: Plugin manifest
description: "Author an Orbit plugin: the manifest, backend contracts, permissions, conformance cases, dashboard panels and migration from external tools."
---

A plugin bundles tools with their backend, permission requests, settings, skills
and dashboard panels. Its source keeps `plugin.yaml` and every runtime asset in a
`.orbit-plugin/` directory, the only directory installed. To install one and
grant what it requests, see [Install and grant plugins](/how-to/plugins/).

## Start with a working plugin

```bash
orbit plugin scaffold demo
orbit plugin validate ./demo
orbit plugin validate ./demo --render
orbit plugin test ./demo
```

- `scaffold` creates `demo/.orbit-plugin/` with a Python 3 backend, a `status`
  tool, a dashboard panel, a skill, an auto-task template and conformance cases.
  It requests no permissions.
- `validate` checks the source without installing it or running the backend.
  `--render` also prints the effective sandbox profile and child environment,
  using the configuration of the registered workspace you are in or name with
  `--workspace <selector>`, or else the host's.
- `test` runs the [conformance cases](#conformance-cases) against the backend in
  a temporary workspace.

Validate and test again after each change, before you install the source.

## Manifest shape

A complete minimal manifest, using the scaffold's backend and `status` handler:

```yaml
schemaVersion: 2
kind: Plugin
metadata:
  name: demo
  version: 0.1.0
  description: Report the plugin's own status.
spec:
  requires:
    host_api: 1
    platforms: [linux, macos]
  backend:
    type: exec
    command: bin/backend.py
    timeout_ms: 30000
  permissions:
    network: none
  tools:
    - name: status
      description: Report this plugin's own status.
      execution_kind: read_only
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          subject: { type: string }
        additionalProperties: false
      output_schema:
        type: object
        required: [plugin, status, subject]
        properties:
          plugin: { type: string }
          status: { type: string }
          subject: { type: string }
      cli:
        positional: [subject]
  tests: [tests/conformance/*.yaml]
```

`metadata.name` is the namespace. Names and tool verbs start with a lowercase
letter and contain lowercase letters, digits or hyphens; built-in command names
are reserved. `metadata.version` is a semantic version. `publisher` and
`homepage` are optional. `origin: orbit` is honored only for a verified
first-party source: no other source can claim the `orbit.*` namespace.

| Field under spec | Purpose |
|---|---|
| requires | Optional: Orbit version range (`orbit`), host API major (`host_api`), supported `platforms`, host `programs` the backend spawns. |
| backend | Required `type` (`exec` or `mcp`) and `command`, relative to the plugin root; optional `args`, `timeout_ms` (capped at 300000) and `sandbox` (`default` or `none`). |
| permissions | What the backend requests. See [Permission and callback boundaries](#permission-and-callback-boundaries). |
| tools | At least one tool. See [Tools](#tools). |
| definitions | Glob lists: `activities`, `jobs`, `routines`, `auto_tasks`. Activities and jobs join the plugin's catalog layer; routines and auto-tasks are seeded disabled on enable. |
| skills | Plugin-relative skill directories. |
| config | `schema`, a plugin-relative JSON Schema file, and `defaults`, a settings object. Operators set declared keys under `[plugins.demo]`; the result is validated before loading. |
| web | See [Dashboard panels and links](#dashboard-panels-and-links). |
| tests | Plugin-relative globs of [conformance](#conformance-cases) files. |
| secrets | Credential names: `name`, optional `description`, `rotatable` (default `false`). Operators set the values on the host. |
| build | Install-time build: `programs`, optional networked `fetch` argv, offline `command` argv, `outputs` mapping `from` to `to`, optional per-phase `timeout_ms`. Only declared outputs are installed. |

Asset paths stay inside the plugin root. Runtime paths accept `{{workspace}}`,
`{{plugin_root}}`, `{{plugin_state}}` and `{{config.<key>}}`; a missing
workspace or key is an error. Keep durable data under `{{plugin_state}}`, not in
the installed source.

A build runs only for a `git+` source pinned to a full commit ID, and only when
the operator passes `--allow-build` on that install. Every other source must
already contain its outputs. Build argv accepts only `{{build_dir}}`.
`outputs.from` is relative to the build directory, `outputs.to` to the plugin
root.

## Tools

The tool above is `demo.status`, `demo_status` over MCP and `orbit demo status`
on the CLI. Once the plugin is installed and enabled, these calls are
equivalent:

```bash
orbit tool run demo.status --input '{"subject":"reader"}'
orbit demo status reader
orbit demo status --subject reader
```

| Tool field | Purpose |
|---|---|
| name, description | The verb and its description. |
| execution_kind | `read_only` admits agents, operators and runners. `mutating` needs operator or runner capability by default. |
| agent_callable | Requests agent access to a `mutating` tool. Default `false`. See below. |
| mcp_scope | `workspace` (default); `global`, callable without a workspace; or `none`, reachable through `orbit tool run` but not advertised over MCP. |
| input_schema, output_schema | A JSON Schema object, or a file in the plugin root such as `{ $ref: schemas/status-input.json }`. Output is checked against its schema. |
| cli.verb | Renames the derived subcommand. |
| cli.positional | Top-level input properties that also become positionals. A JSON-shaped property (an object, an array of anything but strings or numbers, or an untyped one) keeps only its `--<name>-json` flag. |

### Agent-callable mutations

Request agent access for an append-only or proposal-only tool that an
interactive agent may safely call:

```yaml
- name: capture
  description: Append an approved decision to the session log.
  execution_kind: mutating
  agent_callable: true
  mcp_scope: workspace
```

The operator must grant `agent_tools` when enabling the plugin; until then the
plugin is inactive. Orbit does not prove that a backend is append-only, so the
operator reviews it first. `orbit plugin show` displays declarations and grants.

With the grant, an agent session can call the tool through `orbit mcp serve`
without `--operator`, `orbit tool run`, or the derived command, and stays the
caller in the audit trail. Activity allowlists and denials still apply. The
grant gives no filesystem access, callback permission or operator capability.

An upgrade that adds another agent-callable tool disables the plugin and clears
its grants until the operator consents again.

## Backend contracts

### Exec

`backend.type: exec` starts one process per call and writes a JSON request to
stdin. An interactive call with no settings or secrets:

```json
{
  "schema_version": 1,
  "tool": "demo.status",
  "input": {"subject": "reader"},
  "context": {
    "workspace_root": "/work/project",
    "agent": null,
    "model": null,
    "config": {},
    "task_id": null,
    "job_run_id": null
  }
}
```

Read the verb from `tool` and the arguments from `input`. `context` carries the
workspace, agent and model, effective plugin settings and, for a managed
activity call, the task and run. The request's `schema_version` is 1, unlike the
manifest's `schemaVersion: 2`.

Return exactly one JSON object on stdout and exit zero, even for a backend
error:

```json
{"ok":true,"output":{"plugin":"demo","status":"ready","subject":"reader"}}
```

```json
{"ok":false,"error":{"code":"not_ready","message":"Index is unavailable","retryable":true,"detail":{"state":"missing"}}}
```

Send diagnostics to stderr. A nonzero exit, a malformed response or output that
fails its schema fails the whole call.

### MCP

`backend.type: mcp` starts a stdio MCP server and reuses it within a runtime for
the same workspace and caller tool allowance. Implement MCP initialization,
`tools/list` and `tools/call`. Advertise exactly the manifest's verbs with
matching input schemas: `status`, not `demo.status` or `demo_status`.

Orbit sends the verb in `params.name`, the input in `params.arguments` and the
per-call context, with the canonical tool name, in `params._meta.orbit`. Read
it on every call.

Return `structuredContent` for JSON output. Without it, Orbit uses a single text
content item, parsed as JSON when possible, or else the content array.
`isError: true` reports a failure; a structured error carries the same `code`,
`message`, `retryable` and `detail` as the exec error. Output schemas apply.

## Permission and callback boundaries

A request is never a grant: the operator grants each kind when enabling the
plugin.

| Field under permissions | Requests |
|---|---|
| fs.read, fs.write | Directory roots to read or write. |
| fs.write_files | Existing files to modify without access to their parent directory. |
| network | `none`, `loopback` or `any`. |
| env_pass | Environment variables to pass through. `ORBIT_*` names are reserved. |
| orbit_tools | Canonical Orbit tools the backend calls back into. |

The default sandbox uses the host's confinement. `backend.sandbox: none` needs
an explicit `unsandboxed` grant.

### Single-file writes

```yaml
permissions:
  fs:
    write_files: ["{{config.runtime_file}}"]
```

`write_files` uses the same path templates and `fs` consent as directory writes,
and grants no sibling file. Each entry must be:

- an existing regular file, not a directory: owned by the current user, with
  one hard link and no write access for others. Group write is accepted, so a
  checkout made under umask 0002 works; a group-writable ancestor must be owned
  by the user or sticky, and any other-writable ancestor must be sticky.
- free of `*` and `?` in any path component, including ancestors: macOS reads
  those characters as wildcards.
- reached without symbolic links, except for the workspace leaf below. Missing
  files, directories, and paths through an unsafe directory are refused.

The leaf alone may be a symbolic link when the call has a workspace: the link
must sit inside the workspace and resolve, through any further symbolic links,
to an existing regular file whose canonical path stays inside the same
workspace and outside `.git` and `.orbit` (for example `AGENTS.md` ->
`middle.md` -> `CLAUDE.md`). The canonical target must pass the same ownership,
hard-link, and other-write checks, and the sandbox grants that target, so
writes follow the link and the link stays. A link without a workspace, or
whose target is missing, leaves the workspace, or lands in `.git` or `.orbit`,
is refused.

A scoped `fs=<root>` grant must cover the file or an ancestor directory. A new
entry, or a templated entry that now points elsewhere, needs consent again.
To create a file or replace one by rename, request an `fs.write` directory.

### Callbacks

A backend calls back with `orbit tool run <tool>`, a derived plugin command, or
MCP `tools/call` through `orbit mcp serve`. Every other Orbit command is
refused for a plugin child.

A callback must pass three limits: the manifest's `orbit_tools` list, the grant
the host recorded, and what the caller that started the plugin may itself call.
The host issues the child's credential, so setting an environment variable
confers nothing. A grant never turns an agent into an operator or widens a
managed activity's policy.

### Secrets

Declared secrets arrive only inside the call, never as arguments or environment
variables: in `context.secrets` for exec and `_meta.orbit.secrets` for MCP,
keyed by name with a `value` and an opaque `version`. Unset names are absent.
Operators set them with the [plugin secret commands](/how-to/plugins/#set-secrets).

## Conformance cases

Put cases in a file the manifest's `tests` glob matches, for example
`demo/.orbit-plugin/tests/conformance/status.yaml`. `tool` is the manifest verb:

```yaml
schemaVersion: 1
kind: PluginTest
tests:
  - name: status_reports_ready
    tool: status
    input: {}
    expect:
      output:
        plugin: demo
        status: ready
        subject: Orbit
  - name: status_renders_workspace_template
    tool: status
    input:
      subject: "{{workspace}}"
    expect:
      output:
        plugin: demo
        status: ready
        subject: "{{workspace}}"
```

- `expect.output` compares exact JSON values, ignoring object key order.
- Strings in `input` and `expect.output` accept `{{workspace}}` and `{{plugin_root}}`.
- For a backend error, use `expect: { error: { code: not_ready, retryable: true } }`;
  `retryable` and `detail` are optional.
- `secrets: { api_token: fixture-token }` supplies a declared secret, with version
  `fixture`. Tests never read the host's secret store.

```bash
orbit plugin test ./demo --case status_reports_ready
orbit plugin test ./demo --update-goldens
```

| Flag | Effect |
|---|---|
| `--case <name>` | Runs one case. A filtered run does not certify the full suite. |
| `--update-goldens` | Replaces mismatched `expect.output` values with the actual output. Review the diff. |
| `--grant <grants>`, `--accept-requested` | Consent for this run only, never a host grant. One is required when the manifest asks for an unconfined backend, a write root outside the run's temporary directory, `network: any` or `env_pass`. |

## Dashboard panels and links

Add this under `spec` to show the example's status in the dashboard:

```yaml
web:
  panels:
    - id: status
      title: Status
      source: tool:status
      render: kv
      group: diagnostics
      refresh_ms: 30000
  links:
    - title: Local service
      url: "http://127.0.0.1:8080/"
```

A panel's `source: tool:<verb>` names one of this plugin's `read_only` tools.
`group` is `diagnostics` (default), `operations` or `config`. `refresh_ms` is
how long the server reuses the last successful response: 1000 to 3600000,
default 30000. Panels run no plugin JavaScript; `render` picks a renderer:

| Renderer | Expected output |
|---|---|
| kv | Object rendered as label/value rows. |
| table | Array of objects; columns are the union of keys. |
| markdown | String, or an object's `markdown` or `text` field, rendered as sanitized Markdown. |
| json | Pretty-printed JSON; the default renderer. |

A link is a plain tile to a separately hosted UI. Its `url` is HTTP or HTTPS
and may use templates such as `{{config.port}}`. Panels and links grant no
backend access and start no service.

## Legacy external tools

Executables registered as tools, with or without a v1 sidecar, still work. Use
a plugin for new work that needs grants, schemas, bundled assets or conformance.

A v1 sidecar describes one executable's tool, for example
`hello.orbit-tool.yaml` beside `hello.py`:

```yaml
schemaVersion: 1
name: demo.hello
description: Greet a named reader.
parameters:
  - name: name
    description: Reader to greet.
    param_type: string
    required: true
```

```bash
orbit tool add ./hello.py --manifest ./hello.orbit-tool.yaml
orbit tool show demo.hello
orbit tool run demo.hello --input '{"name":"reader"}'
orbit tool disable demo.hello
orbit tool enable demo.hello
orbit tool doctor
orbit tool remove demo.hello
```

The executable receives the raw JSON input on stdin and returns JSON on stdout.
A registration is enabled at once and lives in this machine's tool store,
pointing at the executable. Removing it does not delete the executable.

`--name` and `--description` override the sidecar. Without `--manifest`,
`orbit tool add` looks for the executable's stem plus `.orbit-tool.yaml`,
`.orbit-tool.yml`, then `.orbit-tool.json`. With no sidecar, the name comes
from the filename and the tool has no parameters.

### Migrate to a plugin

```bash
orbit plugin migrate ./hello.py --sidecar ./hello.orbit-tool.yaml --name demo --out-dir ./demo-v2
orbit plugin validate ./demo-v2
```

With `--out-dir`, migration writes `.orbit-plugin/plugin.yaml` and copies the
backend into `.orbit-plugin/bin/`, refusing to overwrite existing files. Without
it, the manifest is printed. Migration translates declarations, not backend
code, marks every tool `mutating`, and leaves the old sidecars and
registrations in place.

Before you install the result:

1. Adapt the copied executable to the [exec](#exec) request and response.
2. Review the permissions and each tool's `execution_kind`.
3. Add conformance cases and run `orbit plugin test ./demo-v2`.

Then remove the old registration when callers have switched.
