---
type: design
summary: "Spec: Retired agent backend selection and its migration"
tags: ["activity-job"]
last_validated: 2026-09-05
---

# Spec: Retired Agent Backend Selection

> **Current release scope.** Orbit executes agent activities through the CLI
> agent path only. The `backend: http | cli | auto` selector, its precedence
> chain, and the engine-driven HTTP agent loop it chose were removed in
> ORB-10801. There is no runtime backend choice left to make.

## What Was Removed

- The `--backend` flag on job runs.
- The precedence chain `--backend` → `ORBIT_BACKEND` → `[runtime] backend` →
  `cli`, along with the resolver that folded `auto` to a concrete value.
- The engine-driven HTTP agent loop, its dispatch arm, and the
  `session:` binding that only that loop could honor.
- `[crews.<name>] backend`, which pinned the same selector per crew.

## Why

Only the CLI agent path was supported. Keeping a selector whose other values
either failed structurally or silently degraded made backend choice look like a
live tuning knob, and it forced every new execution surface — including
asynchronous job submission — to thread a parameter that could not change the
outcome.

## Migration

Each retired declaration is recognized, so it is either accepted as inert or
refused with an actionable message. Nothing is silently reinterpreted:
remapping `http` onto the CLI agent would change which runtime executes the
work without saying so.

| Declaration | `cli` | `http` / `auto` |
|---|---|---|
| `backend:` in an activity or job asset | parses, ignored | refused at asset load |
| `[runtime] backend` in `config.toml` | accepted, ignored | refused at config load |
| `ORBIT_BACKEND` | accepted, ignored | refused at config load |
| `[crews.<name>] backend` | accepted, ignored | refused at config load |

`session:` on a job step has no `cli` equivalent — cross-iteration
conversation history was an HTTP-loop capability — so **any** `session:`
binding is refused at job load. Remove the binding and carry the state the loop
needs through its step inputs instead.

Every refusal carries the same instruction: remove the setting; agent execution
runs through the CLI agent path.

`orbit doctor` applies that same activity-asset refusal on every production
catalog directory, including workspace-local files. For the known retired
values `http` and `auto` it names one opt-in repair,
`orbit doctor --fix-retired-activity-backends`, which deletes only
`spec.backend` and leaves unknown values or unrelated parse failures
untouched.

## Invariants

- `target: activity:<name>` resolution happens before job execution begins, so
  retired-declaration validation sees concrete specs.
- A retired declaration is refused at load time, before a run is persisted or a
  detached worker is started — a run never begins work it cannot finish as
  written.
- `openai_compat` has no CLI runtime and therefore cannot be executed by any
  crew; selecting it fails structurally rather than falling back.

## Agent Signature

Last revised by `claude`.
