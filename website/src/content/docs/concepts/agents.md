---
title: Agents
description: "How Orbit invokes coding agents through CLI and HTTP runtimes."
sidebar:
  order: 6
---

## Runtime Paths

Orbit spawns official provider CLIs as supervised subprocesses under an
`FsProfile` and policy guardrails. The agent CLI is responsible for talking to
its provider.

This is the only agent execution path. The `backend: http | cli | auto`
selector was retired: an activity, job, or config that still declares
`backend: cli` keeps working and the value is ignored, while `http` and `auto`
are rejected with a migration message rather than being remapped onto the CLI
agent.

## Providers

Canonical provider values are:

- `claude`
- `codex`
- `gemini`
- `grok`
- `ollama`
- `openai_compat`

CLI execution is available for `claude`, `codex`, `gemini`, and `grok`.
Transport support is provider-specific; selecting an unsupported
provider fails instead of silently switching providers.

## Tool Allowlists

Agent-loop activities declare the tool names an agent may call. Empty means no tools are allowed.

```yaml
spec:
  type: agent_loop
  tools:
    - orbit.task.show
    - orbit.search
```

`on_denial` controls whether a denied tool call terminates the loop or returns a
structured error for the agent to handle. Under agent dispatch, tool allowlist
enforcement is delegated to the harness and recorded in the audit trail.

## Crews

A **crew** is one named provider-model assignment. Activities do not carry a
model-selection role: a task names a crew, and a run resolves it at dispatch —
an explicit crew on the activity input first, then the task's `crew` field, then
`[workflow] default_crew`.

```toml
[crews.sol]
provider = "codex"
model = "gpt-5.6-sol"
effort = "high"
```

`effort` is an optional reasoning-budget request, forwarded through each
provider's own argument and validated against the provider and model at config
load. Claude and Codex accept `low` through `max`; Grok's accepted set depends
on the model; other providers reject it. See
[Configuration](../../reference/config/#reasoning-effort).

Reassigning work between providers is always explicit — `orbit task update <id>
--crew <name>`. Nothing in Orbit silently moves a task to a different provider.

## Platform Support

Orbit wraps the spawned agent subprocess in an OS-level sandbox scoped by the
activity's resolved `FsProfile`. `orbit init` persists the host-appropriate
backend into the shipped executor assets:

- **macOS** — `sandbox-exec`, with the profile compiled to SBPL.
- **Linux** — Bubblewrap via a trusted `/usr/bin/bwrap`. Writes are confined to
  the resolved profile; host filesystem reads and host network access remain
  available, so read rules and network egress stay delegated. Dispatch **fails
  closed** if `bwrap` is missing or its namespace-and-mount probe fails, unless
  the executor explicitly sets `allow_fallback: true`.
- **Windows and other platforms** — no OS-level wrapper. Process supervision,
  tool allowlists, and in-process guards for Orbit's own built-in tools still
  apply.

The bundled `local-shell` executor declares no sandbox on any platform, by
design. See [Install Orbit](../../getting-started/install/#prepare-the-sandbox)
for the Linux prerequisites.
