---
title: Crew Credentials — Overview
owner: claude
created_date: 2026-10-20
updated_date: 2026-11-28
last_validated: 2026-11-08
status: Draft
stage: shipped
next: "—"
feature: crew-credentials
doc_role: overview
type: design
summary: "A crew-level env list, so one crew can run on a different provider account (an API key with prepaid credits) than the rest of its provider"
tags: [crew-credentials, crews, execution-env, provider-limits]
---

# Crew Credentials — Overview

Crew credentials let a crew run its provider CLI under its own account, through a per-crew environment overlay that names variables or owner-only files, never secret values. Usage limits, holds and exclusions then follow that account instead of the provider as a whole.

Status: phase 1 (`crews.<name>.env`, [ORB-17287]), phase 2 (crew as account, [ORB-17288])
and file-backed sources ([ORB-20547]) have landed. [ORB-17462] completed same-provider
auth-failure attribution to the failing crew account. No committed implementation remains.

## 1. Motivation

Bearing: [`[execution.env]`](../../config/execution.md#executionenv--the-agent-subprocess-environment) is an
allowlist of names, never values; [provider usage limits](../../config/crews.md#provider-usage-limits)
and [auth recovery probes](../../config/providers.md#executor-authentication-recovery-probes) are keyed by
provider account: the provider's default login, or a crew that sets `env`.

### The gap

A crew is `provider` + `model` + `effort`. The credential its CLI uses comes from the agent
environment, which is one `execution.env.pass` list per workspace. So every crew of a provider
shares one account, and an operator with a second account (prepaid Anthropic API credits next
to a Claude subscription) cannot point one crew at it. The only lever today is per drain: list
`ANTHROPIC_API_KEY` in `pass` and set it only in the shell that starts that drain. Then every
Claude activity in the drain moves to the paid account, pilot, review and recovery included,
and exporting the key anywhere persistent moves every Claude run on the host.

### Acceptance (feature level)

- With the example config and `ANTHROPIC_CREDITS_API_KEY_1` set, an `opus-credits` activity's
  agent env holds `ANTHROPIC_API_KEY` and no `CLAUDE_CODE_OAUTH_TOKEN` or
  `ANTHROPIC_CREDITS_API_KEY_1`; an `opus` activity in the same drain holds the OAuth token and
  neither key name.
- `crews.<name>.env` in a workspace file fails load naming the crew; no value appears in any
  output.
- With the source unset, a drain start warns and its window excludes `opus-credits`.
- After phase 2, a usage-limit failure on `opus` holds only the subscription Claude crews;
  `opus-credits` keeps dispatching, and the reverse.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Crew env overlay.** `crews.<name>.env`: names or file paths that add variables to one crew's agents on top of `execution.env.pass`, read only from the global config.
- **Variable source.** `NAME=SOURCE` takes the value of `SOURCE` from the launching process; `NAME=file:PATH` reads an owner-only file at each child spawn.
- **Crew account.** A crew that sets `env` is its own account: usage limits, provider holds, auth exclusions and probes key on `(provider, crew)` for it and on `provider` for the rest.
- **Crew exclusion.** A crew whose variable source or file is unavailable is excluded from the drain window with a warning instead of falling back to the default login.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Parse, validate, global-only | [raw.rs](../../../crates/orbit-config/src/raw.rs) | [ORB-17287] |
| Overlay on the agent env | [child_env.rs](../../../crates/orbit-common/src/security/child_env.rs) | [ORB-17287] |
| File-backed sources | [operator_env.rs](../../../crates/orbit-common/src/security/operator_env.rs) | [ORB-20547] |
| Limits, holds and exclusions per crew account | [provider_hold.rs](../../../crates/orbit-runtime/src/run/task_coupling/provider_hold.rs) | [ORB-17288] |
| Auth-failure attribution to the failing crew | [2_design.md](./2_design.md#where-it-lands) | [ORB-17462] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-17287] — Add crew-specific environment overlays (phase 1).
- [ORB-17288] — Isolate limits, holds and exclusions by crew account (phase 2).
- [ORB-17462] — Attribute same-provider activity auth failures to the actual failing crew account.
- [ORB-20547] — Read owner-only file-backed crew credentials at child spawn.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
