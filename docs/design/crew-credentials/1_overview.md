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

Status: phase 1 (`crews.<name>.env`, [ORB-17287]), phase 2 (crew as account, [ORB-17288])
and file-backed sources ([ORB-20547]) have landed. [ORB-17462] completed same-provider
auth-failure attribution to the failing crew account. No committed implementation remains.
Bearing: [`[execution.env]`](../../config/execution.md#executionenv--the-agent-subprocess-environment) is an
allowlist of names, never values; [provider usage limits](../../config/crews.md#provider-usage-limits)
and [auth recovery probes](../../config/providers.md#executor-authentication-recovery-probes) are keyed by
provider account: the provider's default login, or a crew that sets `env`.

## Problem

A crew is `provider` + `model` + `effort`. The credential its CLI uses comes from the agent
environment, which is one `execution.env.pass` list per workspace. So every crew of a provider
shares one account, and an operator with a second account (prepaid Anthropic API credits next
to a Claude subscription) cannot point one crew at it. The only lever today is per drain: list
`ANTHROPIC_API_KEY` in `pass` and set it only in the shell that starts that drain. Then every
Claude activity in the drain moves to the paid account, pilot, review and recovery included,
and exporting the key anywhere persistent moves every Claude run on the host.

## Acceptance (feature level)

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

## Task References

- [ORB-17287] — Add crew-specific environment overlays (phase 1).
- [ORB-17288] — Isolate limits, holds and exclusions by crew account (phase 2).
- [ORB-17462] — Attribute same-provider activity auth failures to the actual failing crew account.
- [ORB-20547] — Read owner-only file-backed crew credentials at child spawn.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
