---
title: Plugin manifest
description: "Author a v2 Orbit plugin: backend contracts, tools, permissions, conformance cases, dashboard panels, and migration from external-tool sidecars."
---

Use a plugin to bundle tools with their backend, permission requests, settings,
skills and dashboard panels. The source directory holds a `.orbit-plugin/`
directory containing `plugin.yaml` and everything the installed plugin needs.
Only that directory is installed. Keep build inputs elsewhere in the source
tree when they are not runtime assets.

Installs, enable state, grants and secrets belong to this machine. Checkout
pins and workspace toggles do not grant authority. See
[Install and grant plugins](../../how-to/plugins/) for installation and consent.

## Start with a working plugin

Run these commands in a directory where you want to create a source tree:

```bash
orbit plugin scaffold demo
orbit plugin validate ./demo
orbit plugin validate ./demo --render
orbit plugin test ./demo
orbit plugin test ./demo --case status_reports_ready
```

The scaffold creates `demo/.orbit-plugin/` with an executable Python backend,
a `status` tool, a dashboard panel, a skill, an auto-task template and conformance
cases. It requests no additional permissions. You need Python 3 to run its backend.
`validate` checks the source without installing or running the backend; `--render`
also shows the effective sandbox profile and child environment. Run it from the
registered workspace, or pass `--workspace <selector>`, to inspect that workspace's
configuration. Outside a workspace it uses host configuration.

`test` executes the backend through the real protocol in a temporary workspace
and reports the Orbit version. A filtered `--case` run does not certify the full
suite. Edit the generated backend and manifest together, then repeat validation
and conformance before installing the source.

## Manifest shape

This complete minimal manifest uses the scaffold's backend and its `status`
handler. Save it as `demo/.orbit-plugin/plugin.yaml` if you want to try a smaller
version of the scaffold:

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

`metadata.name` owns the namespace. Names and tool verbs start with a lowercase
letter and contain lowercase letters, digits or hyphens. Built-in command names
are reserved. `metadata.version` is a semantic version. Optional metadata includes
`publisher` and `homepage`; `origin: orbit` is honored only for a verified
first-party source. It does not let an arbitrary source claim the `orbit.*`
namespace.

| Field under spec | Purpose |
|---|---|
| requires | Optional Orbit version range (`orbit`), host API major (`host_api`), supported `platforms`, and host `programs` the backend spawns. |
| backend | Required `type` (`exec` or `mcp`), `command`, optional `args`, `timeout_ms` and `sandbox` (`default` or `none`). Relative commands resolve from the plugin root. The host caps calls at 300000 ms. |
| permissions | Requested filesystem roots, network mode, environment names and Orbit callbacks. See [Permission and callback boundaries](#permission-and-callback-boundaries). |
| tools | At least one tool, with a verb, `execution_kind`, schemas and optional MCP/CLI exposure. |
| definitions | Glob lists named `activities`, `jobs`, `routines` and `auto_tasks`. Activities/jobs enter the plugin catalog layer; routines and auto-tasks are seeded disabled on enable. |
| skills | Plugin-relative skill directories. |
| config | `schema`: a plugin-relative JSON Schema file; `defaults`: a settings object. Operators configure declared keys under `[plugins.demo]`; effective settings are validated before loading. |
| web | Declarative panels and links. See [Dashboard panels and links](#dashboard-panels-and-links). |
| tests | Plugin-relative conformance file globs. |
| secrets | Named credentials, with `name`, optional `description` and `rotatable` (default `false`). Declare names only; operators set values on the host. |
| build | Optional install-time build: declared `programs`, optional networked `fetch` argv, offline `command` argv, and `outputs` mapping `from` to `to`. Only declared outputs are installed. |

Asset paths stay inside the plugin root. Runtime path templates accept
`{{workspace}}`, `{{plugin_root}}`, `{{plugin_state}}` and `{{config.<key>}}`.
A missing workspace or configuration key is an error. Store durable plugin data
under `{{plugin_state}}`, rather than modifying the installed source.

Install-time builds are limited to `git+` sources pinned to a full commit ID and
require the operator's per-install `--allow-build` consent. Other source forms
must already contain their outputs. Build argv accepts only `{{build_dir}}` as a
template; `outputs.from` is relative to that build directory and `outputs.to` to
the plugin root. A manifest or checkout pin cannot consent to a build.

### Tool names, schemas and exposure

For the example above, the canonical tool is `demo.status`, its MCP name is
`demo_status`, and the derived CLI spelling is `orbit demo status`. After
installation and enablement, these calls supply the same tool input:

```bash
orbit tool run demo.status --input '{"subject":"reader"}'
orbit demo status reader
orbit demo status --subject reader
```

`input_schema` and `output_schema` are JSON Schema objects, or file references
such as `{ $ref: schemas/status-input.json }` within the plugin root. Output is
checked against the declared output schema. `cli.verb` can rename the derived
subcommand; `cli.positional` lists top-level input properties to promote to
positionals. Those properties remain available as flags.

Set `execution_kind` to `read_only` or `mutating` to describe the operation.
A mutating plugin tool is callable by an operator or a sanctioned run on both
the CLI and MCP, and an agent reaches it only when the task's `required_tools` or
the activity allowlist names it. This classification does not grant filesystem
access. `mcp_scope` defaults to `workspace`; `global` allows calls without a
workspace, while `none` keeps the tool available through `orbit tool run` but
excludes it from MCP advertisement. A global tool cannot assume a workspace
exists.

## Backend contracts

### Exec

`backend.type: exec` starts one process per call. Orbit writes a versioned JSON
request to stdin. This example shows an interactive call with no configured
settings or declared secrets:

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

Read the requested verb from `tool` and the arguments from `input`. Context
comes from the host: workspace, agent/model, effective plugin settings and,
for managed activity calls, activity binding. Return exactly one JSON response
object on stdout and exit zero, including for a structured backend error:

```json
{"ok":true,"output":{"plugin":"demo","status":"ready","subject":"reader"}}
```

```json
{"ok":false,"error":{"code":"not_ready","message":"Index is unavailable","retryable":true,"detail":{"state":"missing"}}}
```

Send diagnostics to stderr. A nonzero exit, malformed response or rejected
output schema fails the call; there is no partial success. The manifest uses
`schemaVersion: 2`, while the exec request uses `schema_version: 1`.

### MCP

`backend.type: mcp` starts a stdio MCP server and reuses it within a runtime for
the same workspace and caller tool allowance. Implement MCP initialization,
`tools/list` and `tools/call`. The server must advertise exactly the manifest's
verbs, with matching input schemas. For `demo.status`, advertise `status`,
not `demo.status` or `demo_status`.

Orbit sends the verb in `params.name`, input in `params.arguments`, and per-call
context in `params._meta.orbit`, including the canonical tool name. Read this
metadata for each call. Return `structuredContent` for JSON output. Otherwise
Orbit uses a single text content item, parsing it as JSON when possible, or
falls back to the content array. `isError: true` reports a tool failure; a
structured error can carry the same `code`, `message`, `retryable` and `detail`
fields used by the exec error above. Output schemas apply here too.

### Permission and callback boundaries

The default sandbox uses the host's confinement. `permissions.fs.read` and
`permissions.fs.write` request roots; `network` is `none`, `loopback` or `any`;
`env_pass` names environment variables; `orbit_tools` lists canonical Orbit
tools the backend needs to call. Orbit-reserved `ORBIT_*` variables cannot be
requested through `env_pass`. A request is never a grant. `sandbox: none`
requires explicit `unsandboxed` consent.

Callbacks use `orbit tool run <tool>` (including derived plugin spellings) or
MCP `tools/call` through `orbit mcp serve`. Other Orbit CLI commands are refused
for a plugin child. The effective callback allowance is bounded by the
manifest's list, the host's recorded grant and the spawning caller's allowance.
An inherited host-issued credential establishes identity; setting an environment
variable cannot confer callback authority. A grant cannot turn an agent into an
operator or widen a managed activity's policy.

Declared secret values travel only in the call's `context.secrets` (exec) or
`_meta.orbit.secrets` (MCP), keyed by name with `value` and opaque `version`.
Unset names are absent. They are not command arguments or environment variables.
Use [plugin secret commands](../../how-to/plugins/#set-secrets) for host values,
and fixture secrets for conformance. Keep ordinary settings separate from credentials.

## Conformance cases

Put this file at `demo/.orbit-plugin/tests/conformance/status.yaml`; the manifest's
`tests` glob includes it. `tool` is the manifest verb:

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

`expect.output` compares exact JSON values, ignoring object key order. String
values in `input` and `expect.output` can use `{{workspace}}` and
`{{plugin_root}}`. For a tool that returns a backend error, use
`expect: { error: { code: not_ready, retryable: true } }`; `retryable` and
`detail` are optional assertions. A case can supply `secrets: { api_token:
fixture-token }` for a manifest-declared secret. Tests never read the host's
secret store; fixture secrets arrive with version `fixture`.

```bash
orbit plugin test ./demo --case status_reports_ready
orbit plugin test ./demo --update-goldens
```

`--update-goldens` replaces mismatched expected outputs. Review those changes
before accepting them. Testing applies the requested temporary-workspace profile;
an unconfined backend, writes outside the temporary directory, `network: any`
or `env_pass` need explicit run consent with `--grant` or `--accept-requested`.
These options do not store a host grant. Review the manifest before consenting.

## Dashboard panels and links

Add this section under `spec` to expose the example's status:

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

Each panel's `source: tool:<verb>` must name one of this plugin's `read_only`
tools. Panels use generic renderers, with no plugin JavaScript:

| Renderer | Expected output |
|---|---|
| kv | Object rendered as label/value rows. |
| table | Array of objects; columns are the union of keys. |
| markdown | String, or an object's `markdown` or `text` field, rendered as sanitized Markdown. |
| json | Pretty-printed JSON; the default renderer. |

`group` is `diagnostics` (default), `operations` or `config`. `refresh_ms` controls
server-side reuse of the last successful response, defaults to 30000 and accepts
1000–3600000 ms. Links are plain tiles to a separately hosted UI and accept only
HTTP or HTTPS URLs; they may use manifest templates such as `{{config.port}}`.
A panel or link does not grant backend access or start a service.

## Legacy external tools

Existing executable tools and v1 sidecars remain supported. Prefer a v2 plugin
for new work that needs grants, schemas, bundled assets or conformance.
`orbit tool scaffold` is deprecated and still writes a v1 sidecar.

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

The executable receives raw JSON input on stdin and returns JSON on stdout.
Registration is enabled immediately. `--name` and `--description` override the
sidecar; without `--manifest`, discovery checks the executable's stem with
`.orbit-tool.yaml`, `.orbit-tool.yml`, then `.orbit-tool.json`. Without a
sidecar, the name comes from the filename and parameters are empty. Removing a
registration does not delete the executable.

Legacy registrations live in this machine's tool store and refer to the external
executable path. They do not install a v2 bundle or establish v2 permission grants.

To generate a v2 source tree instead:

```bash
orbit plugin migrate ./hello.py --sidecar ./hello.orbit-tool.yaml --name demo --out-dir ./demo-v2
orbit plugin validate ./demo-v2
```

Without `--out-dir`, migration prints the manifest. With it, migration writes
`.orbit-plugin/plugin.yaml` and copies the backend into `.orbit-plugin/bin/`;
it refuses to overwrite existing output files. Migration preserves the old
sidecars and registrations. It translates their declarations, not backend code,
and conservatively marks migrated tools `mutating`. Adapt the copied executable
to the v2 exec request/response envelope, review permissions and execution kinds,
add conformance cases, then run `orbit plugin test ./demo-v2` before installing
and enabling it. Retire the old registration when you are ready to switch callers.
