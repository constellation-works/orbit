---
title: "Scope: workspace seeding policy and presets for distributions"
owner: claude
last_updated: 2026-10-27
last_validated: 2026-10-27
status: Accepted
stage: shipped
next: "Count left-out defaults on the dashboard Automation view; decide whether routines need a per-name delete verb beside the list policy"
feature: distributions
doc_role: scope
type: design
summary: "Scope: persisted `seeding.routines` and `seeding.auto_tasks` policies over Orbit's shipped default routines and auto-tasks, honoured by every seeding path with digest-based retirement, plus `orbit workspace init --preset <file>` that applies a policy, enables plugin-seeded definitions and sets config without granting consent"
tags: [distributions, seeding, routines, auto-tasks, presets, plugins]
---

# Scope: workspace seeding policy and presets for distributions

Status: shipped. This page records what was built. The operator reference is
[configuration › `[seeding]`](../../CONFIG.md#seeding--which-shipped-defaults-a-workspace-carries).

## Problem

Every workspace carries every default routine and auto-task Orbit ships. A distribution that
builds on Orbit (a plugin bundle, a team setup, a narrow product) cannot keep a workspace to the
defaults it wants. Deleting them works only partly. An auto-task deleted with
`orbit auto-task delete` stays out, but a routine comes back on the next `orbit workspace sync`.
Nothing stops an upgrade from seeding a default the distribution never chose. A distribution also
had no single, reviewable way to set a workspace up. Its setup was a script of `orbit config set`
calls and hand edits that nothing recorded.

## What was built

### Seeding policy

Two config keys, read per workspace over the global layer like every key:

- `seeding.routines`: `"all"` (default), `"none"`, or a list of shipped routine file stems.
- `seeding.auto_tasks`: the same, by shipped auto-task name.

The rules:

- **One choke point.** `workspace_sync::reconcile_managed_artifacts` reads the policy itself and
  hands reconciliation only the defaults it includes. Every seeding path runs through it:
  `orbit workspace init`, `--force` re-init, `orbit workspace sync`, and the create-only seeding
  host `orbit init` runs over registered workspaces after an upgrade. None of them can skip it.
- **Upgrades.** A list and `"none"` never seed a default added in a later release. `"all"` still
  does, unchanged.
- **Admission.** `orbit-config` admits the keys against its own copy of the shipped and retired
  names, because it cannot see the embedded assets. `orbit-core` asserts at compile time that its
  asset tables carry exactly those names. An unknown name fails config load, `orbit config set`
  and every seeding command, naming the key and the name. A name a later release retires still
  admits, so the workspace that lists it stays loadable.
- **Retirement by digest.** A default the policy leaves out takes the existing path for a default
  a release dropped:
  - an unedited file whose digest Orbit recorded is removed;
  - an edited file moves under `.orbit/.retired-managed/`, with a warning to migrate it;
  - a file Orbit never recorded is left alone.
- **Reseeding.** Widening the policy seeds the default again, because retirement removed its
  manifest entry.
- **Opt-outs.** Auto-task opt-outs recorded by `orbit auto-task delete` are kept while the default
  is left out. `orbit auto-task restore` refuses a left-out name and names the policy.
- **Light loader.** The policy loader reads only the two keys, so a fault elsewhere in either
  config file does not stop convergence. A full config load still reports that fault.

### Presets

`orbit workspace init --preset <file>` applies a TOML preset. It also works with `--force` on a
registered workspace. The file holds:

- `name` (required, 1–64 of `[A-Za-z0-9._-]`);
- `[seeding]` with `routines` and `auto_tasks`;
- `[enable]` with `routines` and `auto_tasks`, by seeded name;
- `[config]` with workspace keys, nested or dotted.

Every table denies unknown fields. The whole file is validated before anything is written or
registered:

- each `[config]` key and value is staged into the workspace config exactly as `orbit config set`
  would, then validated for this workspace;
- `seeding.*` under `[config]` is refused, because it belongs in `[seeding]`;
- each `[enable]` name must be a definition an enabled plugin already seeded into this workspace,
  checked against the plugin asset manifest;
- the file is capped at 256 KiB.

**Consent stays with the operator.** A preset never grants plugin permissions or enables a
plugin. These fields are refused at the top level or under `[enable]`: `grant`, `grants`,
`permissions`, `plugin`, `plugins`, `enable_plugins` and `plugin_enablement`. So is any
`plugin_enablement*` key under `[config]`. The operator runs `orbit plugin enable` first.

**Ordering.** `[seeding]` and `[config]` are written in one locked, validated save after the review
policy is seeded and before defaults are reconciled. The first seeding therefore already follows
the policy. `[enable]` runs after seeding:

- auto-tasks through `auto_task_toggle`;
- routines through the routine edit path.

**Provenance.** `.orbit/state/workspace-preset.json` records `name`, the `sha256:` digest of the
file bytes and `applied_at`.

### Surfaces

- `orbit workspace init` prints the applied preset and the definitions it enabled. Its JSON gains
  a `preset` object.
- `orbit workspace show` prints both policies, the shipped defaults they leave out, and the
  applied preset. Its JSON gains `seeding` (`routines`, `auto_tasks`, `left_out`) and `preset`.
- `orbit doctor`:
  - drops left-out defaults from the artifact catalogs, so they are not reported missing;
  - reports a leftover file of a left-out default as one `orbit workspace sync` will retire;
  - adds a `seeding-policy` row. The row is silent under the default policy and otherwise
    summarizes what is left out. It warns while `workflow.crew_performance` is not `off` and
    `seeding.routines` leaves out `crew_performance_refresh`, since nothing then refreshes the
    snapshot that crew draws read.
- `orbit config keys` and the configuration references list both keys.

## Not built

- **Dashboard count.** The Automation view does not count left-out defaults. `orbit workspace
  show` and `orbit doctor` carry that information today.
- **Routine delete verb.** There is no per-name delete verb for routines. A list policy is the
  durable per-name opt-out for routines.
- **MCP.** There is no MCP tool for `workspace show`. The CLI JSON carries the fields.
- **Settings forks.** The settings-only fork migration still runs over every shipped auto-task
  name, whatever the policy.

## Counts at the time of writing

11 shipped default routines and 23 shipped default auto-tasks.
