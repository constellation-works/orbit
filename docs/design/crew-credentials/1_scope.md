---
title: "Scope: per-crew credentials (`crews.<name>.env`)"
owner: claude
status: Draft
feature: crew-credentials
doc_role: scope
type: design
summary: "Scope: a crew-level env list, so one crew can run on a different provider account (an API key with prepaid credits) than the rest of its provider"
tags: [crew-credentials, crews, execution-env, provider-limits]
last_updated: 2026-10-21
last_validated: 2026-10-21
---

# Scope: per-crew credentials (`crews.<name>.env`)

Status: phase 1 (`crews.<name>.env`, [ORB-17287]) and phase 2 (crew as account, [ORB-17288]) implemented.
Bearing: [`[execution.env]`](../../CONFIG.md#executionenv--the-agent-subprocess-environment) is an
allowlist of names, never values; [provider usage limits](../../CONFIG.md#provider-usage-limits)
and [auth recovery probes](../../CONFIG.md#executor-authentication-recovery-probes) are keyed by
provider account: the provider's default login, or a crew that sets `env`.

## Problem

A crew is `provider` + `model` + `effort`. The credential its CLI uses comes from the agent
environment, which is one `execution.env.pass` list per workspace. So every crew of a provider
shares one account, and an operator with a second account (prepaid Anthropic API credits next
to a Claude subscription) cannot point one crew at it. The only lever today is per drain: list
`ANTHROPIC_API_KEY` in `pass` and set it only in the shell that starts that drain. Then every
Claude activity in the drain moves to the paid account, pilot, review and recovery included,
and exporting the key anywhere persistent moves every Claude run on the host.

## Shape

```toml
[crews.opus-credits]
provider = "claude"
model = "claude-opus-5-5"
effort = "high"
env = ["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]   # or just "ANTHROPIC_API_KEY"
```

`env` is a list of extra names this crew's agents receive, on top of `pass`:

- **`NAME`** passes `NAME` from the launching process, like a `pass` entry, but only to this crew.
- **`NAME=SOURCE`** gives the agent `NAME` with the value of `SOURCE` from the launching process.
  The source can then live in `~/.zprofile` or `~/.orbit/clock.env` under a name no CLI reads,
  so interactive shells and other crews are unaffected, and different crews can use different
  keys (`_1`, `_2`).
- Names only, never values. Both sides must be valid identifiers; privilege-bearing `ORBIT_*`
  names are refused, as in `allowlisted_child_env_from`.

Rules:

1. **Global config only.** `crews.<name>.env` is read only from the global file, like
   `machine.*`; a workspace file that sets it fails load. A workspace file cannot pull host
   secrets into agents.
2. **Crew env wins.** A crew's `NAME` overrides the same name from `pass`. When a crew sets a
   Claude credential (`ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`), the other one is
   removed from that crew's agents, so exactly one credential is present.
3. **Unset source = crew excluded.** A drain or ship start whose crew window includes a crew
   with an unset `env` source warns (same path as unset `pass` names) and excludes that crew
   rather than letting it fall back to the default login. `orbit doctor` reports it.
4. **A crew with `env` is its own account.** Usage-limit readings, provider failure holds,
   pull-drain auth exclusions and probes key on `(provider, crew)` for such crews and on
   `provider` for the rest. The subscription's 5-hour limit does not stop the credits crew,
   and a bad key does not block the subscription crews. `provider_limit_budgets` accepts a
   crew target (`opus-credits:200usd/30d`).

## Where it lands

| Concern | File |
|---|---|
| Parse, validate, global-only | [`raw.rs`](../../../crates/orbit-config/src/raw.rs), [`Crew`](../../../crates/orbit-types/src/identity/agent_pair.rs) |
| Overlay on the agent env | [`child_env.rs`](../../../crates/orbit-common/src/security/child_env.rs), `agent_subprocess_environment` in [engine trait](../../../crates/orbit-engine/src/context/hosts/runtime_host.rs) and [core host](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/host.rs), [`CrewConfig`](../../../crates/orbit-engine/src/context/hosts/config.rs); also the detached worker snapshot and the auth probe |
| macOS Claude credential gate | [`environment.rs`](../../../crates/orbit-engine/src/activity_job/cli_runner/orchestrator/environment.rs) checks the composed crew env |
| Limits, holds, exclusions | [`provider_hold.rs`](../../../crates/orbit-runtime/src/task/provider_hold.rs), [`host_provider_hold.rs`](../../../crates/orbit-core/src/application/distributed/host_provider_hold.rs), [`auth_recovery.rs`](../../../crates/orbit-core/src/application/distributed/auth_recovery.rs), crew window, admission policy |
| Surfaces | `config show/get/set`, dashboard Settings, `orbit doctor` (`env-pass`, `claude-worker-token`), run detail credential origin: names only |

## Phasing

1. **Env list** [ORB-17287]. Parse, global-only rule, overlay, credential exclusivity,
   unset-source exclusion, doctor and config surfaces. Usable alone; limits still per provider
   (documented).
2. **Crew as account** [ORB-17288]. Limits, holds, exclusions, probes and budgets keyed by crew
   for crews with `env`. Existing records read as provider-level; an additive migration adds
   the crew-account readings table and the invocation `account` column.

## Open questions

1. **Pull drains.** The executing host's global config defines the crew, so a follower without
   the crew or its source excludes it. Is that enough, or should the owner refuse to offer the
   crew to followers that lack it?
2. **Exact-version admission.** An older follower that does not know `env` must refuse the crew,
   not run it on the default login.

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

## Task References

- [ORB-17287] — phase 1: `crews.<name>.env` overlays the agent environment
- [ORB-17288] — phase 2: a crew with `env` is its own account for limits, holds and exclusions

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
