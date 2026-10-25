---
title: Executor YAML
description: "Overriding a shipped executor: file location, envelope, sandbox, host_os, inspection and auth probes."
sidebar:
  order: 6
---

An **executor** is the recipe Orbit uses to spawn a provider CLI: its command,
static arguments, output format and OS sandbox. This page covers overriding an
executor Orbit already ships. It does not cover adding a provider Orbit has no
executor for.

Use this page when you need to:

- pin an executor to the hosts it can actually run on (`host_os`);
- change its sandbox backend or its fallback behaviour;
- adjust the static `command` or `args` for a locally wrapped CLI;
- declare the arguments a pinned-source inspection run needs
  (`inspection_args`); or
- declare an authentication recovery probe (`auth_probe`).

For choosing a provider and writing a crew, see
[Agents and crews](../../concepts/agents/). A crew names a provider and model;
the executor is the lower-level definition underneath it.

## Where the file lives

Executors are **machine scope**, not workspace scope. `orbit init` seeds one
file per shipped executor under the global Orbit root:

```text
~/.orbit/resources/executors/<name>.yaml
```

For example, `~/.orbit/resources/executors/codex.yaml`. Editing that file
changes the executor for every workspace on this machine. There is no
workspace-local executor override: a file under a checkout's
`.orbit/resources/` does not replace the global executor definition.

Re-running plain `orbit init` does not overwrite a file you customized. It
only applies narrow migrations to a shipped default, such as copying a new
shipped `host_os` onto an installed file that still omits the field, or
re-aligning a platform-mismatched sandbox. `orbit init --force` deletes the
global root first, so it resets these files to the shipped defaults.

Inspect the effective definitions with:

```bash
orbit doctor providers
orbit doctor providers --json
```

That command reports each executor's type, command, sandbox, fallback setting
and CLI availability as Orbit resolves them on this host. Verify flags against
the provider CLI itself before you put them in `args`; Orbit passes `args`
through and does not validate them against the CLI's help.

## Envelope

Every executor file is a schema version 2 resource envelope. The file name,
without `.yaml`, must match `metadata.name`:

```yaml
schemaVersion: 2
kind: Executor
metadata:
  name: codex
spec:
  executor_type: direct_agent
  command: codex
  args:
    - exec
    - --json
  stdout_format: envelope
  sandbox: linux-bwrap
  model_flag: "--model"
  env: {}
```

An unsupported `schemaVersion`, a `kind` other than `Executor`, a
`metadata.name` that does not match the file, or an invalid value for a
checked field such as `host_os` fails to load, naming the file. Keep the
envelope intact when you override a field: copy the installed file and edit
only the `spec` entries you mean to change.

## `spec` fields

| Field | Type | Purpose |
|---|---|---|
| `executor_type` | string | Executor family: `direct_agent` for shipped agent CLIs, `local_shell` for deterministic local commands. See the retired values below. |
| `command` | string | Program to spawn, resolved on `PATH` (for example `codex`). Omit it for `local_shell` steps that name their own program. |
| `args` | list of strings | Static arguments on every run. Keep the execution envelope and prompt out of these; Orbit supplies them off `argv`. |
| `inspection_args` | list of strings, or omitted | Arguments appended only for pinned-source inspection runs. An empty list and an omitted field mean different things; see below. |
| `stdout_format` | `envelope`, `json` or `text` | How Orbit reads the subprocess output. Leave the shipped value unless the CLI's output shape changes with your `args`. |
| `model_pair_override` | `{ strong, weak }` | Legacy audit and attribution override for older definitions. It does not choose the model the subprocess runs; new definitions leave it out and use crew-selected models. |
| `model_flag` | string | Flag Orbit appends with the step's model for a `direct_agent` (for example `"--model"`). Only the flag name; the model value comes from the crew or step. |
| `timeout_seconds` | integer | Optional wall-clock limit where the executor family uses one, notably as the `local_shell` fallback budget. |
| `auth_probe` | object | Optional bounded authentication recovery call. Only the shipped `claude` executor declares one; see below. |
| `env` | map of strings | Extra environment entries for the subprocess. Credential values do not belong here; pass variable names through the configured environment instead. |
| `sandbox` | `off`, `macos-sandbox-exec` or `linux-bwrap`, or omitted | OS sandbox backend. See the next section. |
| `allow_fallback` | boolean, default `false` | Degrade to a bare process when the declared sandbox primitive is unavailable, instead of failing closed. |
| `host_os` | list, or omitted | Host operating systems this executor can run on. See the `host_os` section. |
| `created_at`, `updated_at` | timestamps | Maintained when Orbit writes the definition. You do not need to set them by hand. |

A complete override that only restricts hosts and keeps everything else from
the shipped Codex definition looks like this on a Linux host. Start from your
installed file, because its `sandbox` value is already the one seeded for your
platform:

```yaml
schemaVersion: 2
kind: Executor
metadata:
  name: codex
spec:
  executor_type: direct_agent
  command: codex
  args:
    - exec
    - --json
  inspection_args:
    - --config
    - features.shell_tool=true
    - --config
    - features.unified_exec=true
  stdout_format: envelope
  sandbox: linux-bwrap
  allow_fallback: false
  host_os:
    - linux
  model_flag: "--model"
  env: {}
```

## Sandbox and fallback

`sandbox` selects the OS wrapper Orbit applies around the provider CLI,
scoped by the activity's filesystem profile:

- `macos-sandbox-exec` applies on macOS only.
- `linux-bwrap` applies on Linux only.
- `off` explicitly disables the outer Orbit sandbox and the provider's inner
  sandboxing. It applies on any host, and it is a persistent operator choice,
  not a per-run flag.
- Omitting `sandbox` leaves the choice unspecified. Installed Linux defaults
  migrate to Bubblewrap during seeding; other executors keep provider
  behaviour. The bundled `local-shell` executor declares no sandbox by design.

Shipped assets declare their sandbox intent once, and seeding translates it
for the host: a shipped executor installs with `linux-bwrap` on Linux and
keeps `macos-sandbox-exec` on macOS. A value you set yourself is not
translated. Declaring a backend unsupported on the host OS (such as
`macos-sandbox-exec` on Linux or `linux-bwrap` on macOS) is always refused
before provider launch; `allow_fallback` does not override an OS/backend
mismatch. The remedy is to configure the host's native backend (`linux-bwrap` on
Linux, `macos-sandbox-exec` on macOS; WSL2 on Windows) or explicitly set
`sandbox: off`.

On a supported host OS, `allow_fallback: true` permits running without an OS
wrapper if the host's native sandbox primitive is missing or unusable (for
example, on Linux when Bubblewrap is absent or its user namespace probe fails,
or on macOS if `/usr/bin/sandbox-exec` is missing):

```yaml
# Supported Linux host where Bubblewrap may be unavailable:
spec:
  sandbox: linux-bwrap
  allow_fallback: true
```

With `allow_fallback: true`, an unavailable sandbox primitive on a supported OS
degrades to running the CLI with no OS wrapper rather than refusing dispatch.
The activity's tool policy and in-process guards still apply, but the filesystem
profile is no longer kernel-enforced. Leave it `false` unless that trade-off is
acceptable on this machine. Check readiness before a run with
`orbit doctor providers`.

## Host constraint: `host_os`

`host_os` limits where an executor may run:

```yaml
spec:
  host_os:
    - macos
```

Rules:

- The value is a non-empty subset of `linux`, `macos` and `windows`. An empty
  list or any other name is refused when the file loads, and the error names
  the file.
- Omit `host_os`, or set it to null, to allow every host.
- A concrete sandbox backend is separate from `host_os`. A macOS-only
  executor still needs a sandbox value that applies there; `host_os` never
  translates one backend into another.

Admission intersects the executor's `host_os` with the task's `os:` tags. The
task's effective OS requirement is that intersection, and task creation,
`orbit run ship` discovery and pull admission all read the same intersection:

- An empty intersection — an `os: linux` task whose crew's executor declares
  `host_os: [macos]` — is refused when the task is created or updated. Nothing
  is written, so the task never becomes one no host can run.
- A crew pinned to an executor this host cannot run stays assigned and waits,
  for example `waits for a macos host`. Orbit does not redraw it onto another
  crew. Pool and default-crew draws on this host skip crews whose executor
  excludes it.
- `orbit doctor providers` reports such an executor as unsupported on this OS
  rather than telling you to install its CLI here.

Most shipped executors omit `host_os`, so they run wherever their seeded
sandbox applies. Declare it when a wrapped CLI genuinely exists on only some
of your hosts.

## Inspection runs: `inspection_args`

Task pilot, finding verification and escalation triage inspect pinned source
with the provider CLI's own file-read and shell tools. `inspection_args` names
the arguments appended to that executor's `args` for those runs only:

```yaml
spec:
  inspection_args:
    - --config
    - features.shell_tool=true
    - --config
    - features.unified_exec=true
```

The three states matter:

- **A list** appends those arguments to an inspection run. The shipped
  `codex` executor declares the list above so its native shell stays on.
- **An empty list** (`inspection_args: []`) declares that the static `args`
  already enable the native tools. The shipped `claude` executor declares
  `[]`, because its `args` already pass `--tools default`.
- **Omitted** declares no known inspection surface. Inspection draws skip that
  executor's crews, and a run that still names one is refused before launch.
  No other shipped executor declares the field.

An installed definition that omits the field takes the shipped default's
declaration for the same executor name, so an existing install keeps
inspecting after an upgrade. A value you set wins over that default.

These arguments do not grant write access. Read-only inspection stays
enforced by the `reviewer` filesystem profile and the OS sandbox, and the
apply step still checks the revision the inspection reports against the
pinned source.

## Authentication probes: `auth_probe`

An executor may declare one bounded call that a pull drain uses to recover an
authentication-excluded provider account. Only the shipped `claude` executor
declares one. A probe never runs at drain start, for a healthy crew, or for a
capacity or refusal exclusion.

```yaml
spec:
  auth_probe:
    args: ["-p", "--model", "haiku", "--output-format", "text", "--tools", "", "--disallowedTools", "mcp__*", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}', "--max-turns", "1", "--no-session-persistence"]
    stdin: "Reply with exactly ORBIT_AUTH_OK."
    timeout_seconds: 60
    success:
      kind: stdout_contains
      text: ORBIT_AUTH_OK
    relogin_hint: "Run `claude auth login`, or renew `claude setup-token`."
```

- `args` replaces the activity arguments for the probe call, using the same
  resolved command. `stdin` carries the minimal prompt, never `argv`.
- `timeout_seconds` is between 1 and 120. Each attempt costs one real model
  request.
- `success.kind` is `exit_zero`, or `stdout_contains` with non-empty `text`.
  Both also require a zero exit.
- `relogin_hint` is the operator action Orbit surfaces when the probe keeps
  failing.

Copy this shape only for an executor whose minimal model call and success
signal you have verified. Declaring a probe with an unverified success check
can re-admit a provider that still cannot authenticate.

## What this is not

- **Adding an unsupported provider.** A new provider lane needs a supported
  CLI contract and changes to Orbit itself, not just a YAML file. Writing an
  executor file with a new name does not register a new provider family with
  crews, effort handling or a stdout adapter. Use a shipped provider id in a
  crew, or run that CLI outside Orbit.
- **The retired External Executor Protocol.** `executor_type: external` still
  parses so old definitions load, but no external transport is dispatched and
  nothing spawns from it. Do not write new definitions with that value.
- **The legacy `agent_cli` spelling.** Older definitions using it migrate to
  `direct_agent` during seeding. Write `direct_agent` in new files. The old
  `cli_command` spelling of `local_shell` is likewise still accepted on load
  and re-serialized as `local_shell`.
- **Per-workspace or per-task configuration.** Executor definitions are one
  per machine. For per-work choices, configure the crew in `config.toml`
  instead; see [Configuration](../config/).

## Related pages

- [Agents and crews](../../concepts/agents/) — providers, crews and platform
  support.
- [Configuration](../config/) — `config.toml` layering and crew settings.
- [Scoping rules](../scoping/) — where Orbit stores each artifact type.
