---
title: Executor YAML
description: "Override a shipped executor: file location, envelope, fields, sandbox, host_os, inspection arguments, auth probes and image input."
---

An **executor** is the recipe Orbit uses to spawn a provider CLI: its command,
static arguments, output format and OS sandbox. Edit an executor's YAML to:

- pin it to the hosts it can run on (`host_os`);
- change its sandbox backend or fallback;
- adjust `command` or `args` for a locally wrapped CLI;
- declare inspection arguments (`inspection_args`), image delivery
  (`image_input`) or an authentication probe (`auth_probe`).

To choose a provider and write a crew, see
[Agents and crews](/concepts/agents/). A crew names a provider and model; the
executor is the definition underneath it.

## Where the file lives

Executors are machine scope. `orbit init` seeds one file per shipped executor:

```text
~/.orbit/resources/executors/<name>.yaml
```

Editing `~/.orbit/resources/executors/codex.yaml` changes the Codex executor
for every workspace on this machine. There is no workspace-local override: a
file under a checkout's `.orbit/resources/` does not replace it.

Plain `orbit init` keeps a file you customized. It only applies narrow
migrations to a shipped default, such as adding a new shipped `host_os` to a
file that omits the field or re-aligning a sandbox that does not match the
platform. `orbit init --force` resets the global root, and these files with
it.

Inspect the definitions as Orbit resolves them on this host:

```bash
orbit doctor providers
orbit doctor providers --json
```

It reports each executor's type, command, sandbox, fallback setting and CLI
availability. Orbit passes `args` through without checking them against the
CLI's help, so verify flags against the provider CLI first.

## Envelope

Every executor file is a schema version 2 resource envelope. The file name,
without `.yaml`, must match `metadata.name`. To override a field, copy the
installed file and change only the `spec` entries you mean to. For example,
this Codex definition on Linux restricts `host_os`:

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

Start from your installed file: its `sandbox` value is already the one seeded
for your platform.

A file fails to load, with an error that names it, when `schemaVersion` is
unsupported, `kind` is not `Executor`, `metadata.name` does not match the file
name, or a checked field such as `host_os` holds an invalid value.

## `spec` fields

| Field | Type | Purpose |
|---|---|---|
| `executor_type` | string | `direct_agent` for agent CLIs, `local_shell` for deterministic local commands. Retired values are under [What this is not](#what-this-is-not). |
| `command` | string | Program to spawn, resolved on `PATH`. Omit it for `local_shell` steps that name their own program. |
| `args` | list of strings | Static arguments on every run. Orbit supplies the execution envelope and prompt itself, off `argv`; keep them out. |
| `inspection_args` | list of strings, or omitted | Arguments appended only for pinned-source inspection runs. Empty and omitted mean different things; see [Inspection runs](#inspection-runs-inspection_args). |
| `image_input` | object, or omitted | How image files reach the model. Omitted means the executor cannot take images; see [Image input](#image-input). |
| `stdout_format` | `envelope`, `json` or `text` | How Orbit reads the subprocess output. Keep the shipped value unless your `args` change the CLI's output shape. |
| `model_pair_override` | `{ strong, weak }` | Legacy audit and attribution override. It does not choose the model that runs; leave it out of new definitions. |
| `model_flag` | string | Flag name Orbit appends with the step's model for a `direct_agent`, for example `"--model"`. The value comes from the crew or step. |
| `timeout_seconds` | integer | Optional wall-clock limit; the fallback budget for `local_shell`. |
| `auth_probe` | object | Optional authentication recovery call; see [Authentication probes](#authentication-probes-auth_probe). |
| `env` | map of strings | Extra environment entries for the subprocess. Do not put credential values here; pass variable names through the configured environment. |
| `sandbox` | `off`, `macos-sandbox-exec` or `linux-bwrap`, or omitted | OS sandbox backend; see [Sandbox and fallback](#sandbox-and-fallback). |
| `allow_fallback` | boolean, default `false` | Run without an OS wrapper when the declared sandbox primitive is unavailable, instead of failing. |
| `host_os` | list, or omitted | Host operating systems this executor can run on; see [Host constraint](#host-constraint-host_os). |
| `created_at`, `updated_at` | timestamps | Maintained by Orbit. Do not set them by hand. |

## Sandbox and fallback

`sandbox` selects the OS wrapper around the provider CLI, scoped by the
activity's filesystem profile.

| Value | Effect |
|---|---|
| `macos-sandbox-exec` | Applies on macOS only. |
| `linux-bwrap` | Applies on Linux only. |
| `off` | Disables Orbit's outer sandbox and the provider's inner sandboxing, on any host. It is a persistent choice, not a per-run flag. |
| omitted | Unspecified. Installed Linux defaults migrate to Bubblewrap during seeding; other executors keep provider behaviour. The shipped `local-shell` executor omits it by design. |

Seeding translates a shipped executor's sandbox for the host: `linux-bwrap` on
Linux, `macos-sandbox-exec` on macOS. A value you set yourself is not
translated. A backend that does not match the host OS is refused before the
provider launches, and `allow_fallback` does not override that. Set the
host's native backend, or `sandbox: off`.

`allow_fallback: true` covers a different case: the backend matches the host,
but its primitive is missing or unusable (Bubblewrap absent or failing its
user-namespace probe on Linux, `/usr/bin/sandbox-exec` missing on macOS).

```yaml
spec:
  sandbox: linux-bwrap
  allow_fallback: true
```

The CLI then runs with no OS wrapper instead of dispatch being refused. Tool
policy and in-process guards still apply, but the kernel no longer enforces
the filesystem profile. Leave it `false` unless you accept that on this
machine. Check readiness with `orbit doctor providers`.

## Host constraint: `host_os`

`host_os` limits where an executor may run:

```yaml
spec:
  host_os:
    - macos
```

- The value is a non-empty subset of `linux`, `macos` and `windows`. An empty
  list or any other name fails to load.
- Omit `host_os`, or set it to null, to allow every host. Most shipped
  executors omit it.
- `host_os` does not pick or translate a sandbox. A macOS-only executor still
  needs a `sandbox` value that applies on macOS.

Admission intersects the executor's `host_os` with the task's `os:` tags:

- **Empty intersection.** An `os: linux` task whose crew's executor declares
  `host_os: [macos]` is refused when the task is created or updated. Nothing
  is written.
- **Crew pinned to an executor this host cannot run.** The task stays
  assigned and waits for a host that can. Orbit does not redraw it onto
  another crew. Pool and default-crew draws on this host skip such crews.
- **`orbit doctor providers`** reports the executor as unsupported on this
  OS, not as a missing CLI.

## Inspection runs: `inspection_args`

Task pilot and escalation triage inspect pinned source with the provider
CLI's own file-read and shell tools. `inspection_args` names the arguments
appended to `args` for those runs only:

```yaml
spec:
  inspection_args:
    - --config
    - features.shell_tool=true
    - --config
    - features.unified_exec=true
```

| State | Meaning |
|---|---|
| A list | Appended to an inspection run. The shipped `codex` executor declares the list above so its native shell stays on. |
| `[]` | The static `args` already enable the native tools. The shipped `claude` executor declares this, because its `args` pass `--tools default`. |
| Omitted | No inspection surface. Inspection draws skip that executor's crews, and a run that still names one is refused before launch. No other shipped executor declares the field. |

An installed file that omits the field takes the shipped default for the same
executor name; a value you set wins.

These arguments grant no write access. The `reviewer` filesystem profile and
the OS sandbox keep inspection read-only.

## Authentication probes: `auth_probe`

An executor may declare one bounded call that a pull drain uses to recover a
provider account excluded for an authentication failure. Only the shipped
`claude` executor declares one. A probe never runs at drain start, for a
healthy crew, or for a capacity or refusal exclusion.

```yaml
spec:
  auth_probe:
    args: ["-p", "--model", "haiku", "--output-format", "text", "--tools", "", "--disallowedTools", "mcp__*", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}', "--max-turns", "1", "--no-session-persistence"]
    stdin: "Reply with exactly ORBIT_AUTH_OK."
    timeout_seconds: 60
    success:
      kind: stdout_contains
      text: ORBIT_AUTH_OK
    relogin_hint: "Run `claude auth login`, or renew `claude setup-token` and pass CLAUDE_CODE_OAUTH_TOKEN in [execution.env].pass."
```

| Field | Meaning |
|---|---|
| `args` | Replaces the activity arguments for the probe call, on the same resolved command. |
| `stdin` | The minimal prompt. It never goes on `argv`. |
| `timeout_seconds` | 1 to 120. Each attempt costs one real model request. |
| `success.kind` | `exit_zero`, or `stdout_contains` with non-empty `text`. Both also require a zero exit. |
| `relogin_hint` | The operator action Orbit shows when the probe keeps failing. |

Declare a probe only for an executor whose minimal model call and success
signal you have verified. An unverified success check can re-admit a provider
that still cannot authenticate.

## Image input

`spec.image_input` declares how image files reach the model. Orbit stages
images as files in the run's scratch directory; they never travel in prompt
text.

```yaml
spec:
  image_input:
    mode: attach_args          # or native_read
    formats: [png, jpeg, webp] # png is required: it is the canary's format
    max_images: 40             # canary included; at least 2
    attach_args: ["--image", "{path}"] # attach_args only: argv per image
```

| Key | Rule |
|---|---|
| `mode` | `native_read`: the prompt lists the staged paths and the CLI's own file-read tool presents them. `attach_args`: the images are attached at launch. |
| `formats` | At least one of `png`, `jpeg`, `webp`, `gif`, with no repeats. Must include `png`. |
| `max_images` | Most images one invocation may receive, canary included. At least 2. |
| `attach_args` | Required in `attach_args` mode, refused in `native_read`. Appended once per image with `{path}` replaced by the staged file. No entry may be empty, and one must contain `{path}`. |

An invalid declaration, including an unknown key, fails to load with
`invalid spec.image_input: …`.

Shipped declarations:

| Executor | Mode | Formats |
|---|---|---|
| `claude` | `native_read` | `png`, `jpeg`, `webp`, `gif` |
| `codex` | `attach_args`, `--image {path}` | `png`, `jpeg`, `webp` |
| `muse` | `attach_args`, `--image {path}` | `png` |

The other shipped executors declare none. An installed file that omits the
field takes the shipped default for the same executor name; a value you set
wins.

Before you declare `image_input` for a lane, probe it with
`orbit agents vision-probe`; see [CLI commands](/reference/cli/#analytics).

### When a crew cannot take images

- **The executor omits `image_input`.** An activity that asks for images on
  that crew fails before launch with the non-retryable
  `image_input_unavailable` error.
- **The crew sets `vision = false`** in `config.toml`. It is never given
  images, even on an image-capable executor. The default is `true`; use
  `false` for a text-only model. See [Configuration](/reference/config/).
- **A requested image is not a regular file** (a directory, FIFO, socket or
  device, or a symlink to one). The run is refused before launch with a
  `stage image …` error that names the path.

### The canary

Every image-input invocation also receives a canary: a generated PNG of a
random six-character code, which the agent must return as `result.canary`. A
result without the right code is refused as `[image_not_seen]`, a retryable
step failure. Each check emits an `activity.image_canary_checked` audit
event. The canary proves delivery, not judgement.

A crew whose newest two or more checks all missed is held for visual work
until 30 minutes after its last miss. The hold doubles with each further miss,
up to 24 hours, and the next passing check clears it. Probes and deliveries
both count.

## What this is not

- **A way to add a provider.** A new provider needs changes to Orbit itself.
  An executor file with a new name registers no provider family. Use a
  shipped provider id in a crew, or run that CLI outside Orbit.
- **Per-workspace or per-task configuration.** Executors are one per machine.
  For per-work choices, configure the crew in `config.toml`; see
  [Configuration](/reference/config/).
- **`executor_type: external`.** The value still parses so old definitions
  load, but nothing is dispatched from it. Do not use it.
- **`agent_cli` and `cli_command`.** These old spellings of `direct_agent`
  and `local_shell` still load. Write the new names.
