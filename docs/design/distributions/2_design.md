---
title: Distributions — Design
owner: claude
created_date: 2026-10-27
updated_date: 2026-11-28
last_validated: 2026-11-24
status: Accepted
feature: distributions
doc_role: design
type: design
summary: "Design: name the role of an app built on Orbit (a distribution), and let it choose which shipped default routines and auto-tasks its workspaces carry through a persistent seeding policy and a TOML preset"
tags: [distributions, presets, routines, auto-tasks, plugins, workspace]
---

# Distributions — Design

This doc holds the mechanisms behind [1_overview.md](./1_overview.md), which states the problem and the goal.

## Shape

### Vocabulary

| Term | Meaning | Example |
|---|---|---|
| **Engine** | Orbit: tasks, runs, routines, auto-tasks, the tool registry, MCP and the clock | `orbit` |
| **Component** | A plugin: the unit that adds tools and definitions to the engine | `office`, `gateway`, `cloudcli` |
| **Distribution** | A product built on Orbit for an audience, with its own app or UI, components and installation story | orbit-office |
| **Preset** | A TOML file a distribution supplies to configure a workspace | `office.toml` |

"Plugin" remains the right word for a component, not the product. A distribution may
consist of one plugin and a preset; it is still a distribution when it has an audience and
an install story of its own.

### Who owns what

| Concern | Orbit | Distribution |
|---|---|---|
| Engine behavior, shipped defaults and upgrades | Provides them | Pins a tested Orbit version |
| Tools and definitions | Provides the plugin and seeding interfaces | Ships its components |
| Which Orbit defaults a workspace carries | Implements the seeding policy | Selects a policy in its preset |
| Which of its own definitions start enabled | Enables only definitions already seeded in the workspace | Lists those definitions in its preset |
| Plugin permissions | Requires `orbit plugin enable` consent | Asks the user and never bypasses consent |
| Human-in-the-loop domain decisions | Provides the generic `plugin_request` gate, `orbit attention` visibility, dashboard and channel routing ([human-decision-channel §12](../human-decision-channel/2_design.md#12-plugin-raised-decision-requests-with-an-apply-callback), designed) | Raises closed questions and owns its operator-allowlisted apply tool |
| UI, onboarding and installer | Provides public interfaces | Owns them |

### Seeding policy

[ORB-19186] implemented the seeding policy, presets and their workspace surfaces. The `seeding.routines` and `seeding.auto_tasks` workspace configuration keys choose which
Orbit-shipped defaults are seeded. Each accepts `"all"` (the default), `"none"`, or a TOML
array of shipped names:

```toml
[seeding]
routines = ["worktree_gc", "store_gc"]
auto_tasks = "none"
```

The policy is honored by workspace initialization and re-initialization, workspace sync,
and create-only seeding of registered workspaces after an upgrade. `"all"` includes a
default added by a later release; `"none"` and a list do not. A list name Orbit has never
shipped is rejected when config is loaded or set and by seeding commands. A name shipped by
an earlier release but since retired remains admissible and seeds nothing, so older
workspace configuration remains loadable.

When a policy is narrowed, an excluded default whose bytes match the digest Orbit recorded
is removed. An operator-edited default is preserved under
`.orbit/.retired-managed/` with a migration warning, and an unrecorded file is left alone.
Widening the policy seeds the included defaults on the next sync. A default excluded by
policy is not reported missing. An auto-task deletion remains opted out while the policy
excludes it, and restoring an excluded auto-task is refused with the policy identified. For
routines, a list policy is the durable per-name opt-out; there is no separate routine
delete record.

`orbit workspace show` reports the effective policies and applied preset. `orbit doctor`
does not report an excluded default as missing. See [Configuration](../../config/README.md)
for the operator-facing reference.

### Workspace presets

`orbit workspace init --preset <file>` applies a TOML preset to a new or existing
workspace. `--force` applies it to an already registered workspace. A preset can set
seeding policy, workspace config keys accepted by `orbit config set`, and plugin definitions
to enable after they have been seeded:

```toml
name = "office"                  # required; 1–64 ASCII letters, digits, '.', '_' or '-'

[seeding]
routines = "none"
auto_tasks = "none"

[enable]
routines = ["office-daily-digest"]
auto_tasks = []

[config]
"pr.labels" = ["office"]
```

`orbit workspace init --preset <file>` applies it to a new or an existing workspace:

1. **Validated before any write.** `name` is required; unknown fields and `[config]` keys
   refused by `orbit config set` are refused. Presets have one placement rule of their own:
   seeding policies belong in `[seeding]`, so `seeding.*` is refused under `[config]` even
   though those keys are settable with `orbit config set`. `[enable]` names must already be
   seeded by an enabled plugin, and their files must be present and loadable; a missing,
   unloadable or stale name refuses the whole preset. The file is capped at 256 KiB. A preset
   refuses fields that request plugin permissions or plugin enablement; that consent belongs
   to `orbit plugin enable`. It can set other settable workspace config, including
   `workflow.low_complexity_crews`.
2. **Recorded.** The workspace keeps one record of the last applied preset's `name`, the
   digest of its bytes and `applied_at`. Applying the same file again runs initialization and
   preset application again: it rewrites `applied_at` and enables the requested definitions
   again, including ones an operator disabled after the earlier application. A changed file
   replaces the record and applies its policies.
3. **Enabling follows plugin seeding.** Bootstrap the workspace, enable plugins whose
   definitions the preset names, then apply the preset. Its `[enable]` entries switch on
   those already-seeded definitions as part of preset application. Plugin seeding itself
   leaves routines and auto-tasks disabled; carrying enable requests forward to a later plugin
   seeding event is not implemented.
4. **No layering.** A workspace has at most one recorded preset. A later
   `--preset` replaces it.

### How a distribution installs

A distribution installer can follow this sequence:

1. Install or check the pinned Orbit release; do not upgrade an existing installation
   without asking.
2. Add its plugin components and create the workspace.
3. Ask for consent and run `orbit plugin enable` for components whose definitions the
   preset will enable. Presets never grant apply-tool consent. Once the live-raise
   slice of [plugin-raised decision requests](../human-decision-channel/2_design.md#12-plugin-raised-decision-requests-with-an-apply-callback)
   has shipped (§12.11), also ask for explicit operator consent via
   `orbit decision apply-tool allow <ns>.<verb>` for any component apply tool. That
   slice is the first that accepts `orbit.decision.plugin_raise` against persisted
   consent, and it ships only with a working human answer already in place. Do not
   add this install step for an earlier slice. An earlier release may already
   provide the allowlist command; consent alone does not make a plugin question
   safe to raise.
4. Run `orbit workspace init --force --preset <file>` to apply its seeding policy, config
   and selected enables. The preset's `[enable]` entries must already have been seeded.
5. Store secrets with `orbit plugin secret set`; never write them to a file.
6. Start its app, which uses Orbit's public CLI, MCP or documented HTTP API rather than
   reading `.orbit/` directly.

### Non-goals

- Packaging, hosting, signing or updating distributions. They ship from their own
  repositories with their own installers.
- A distribution registry or an `orbit distribution` command. The word names a role, not an
  engine object.
- Per-distribution behavior in Orbit core. Orbit does not branch on a preset's `name`.
- Granting permissions. Consent stays with `orbit plugin enable`; a preset cannot widen
  what a plugin may do.
- Removing a plugin's own definitions. A distribution that does not want one should not
  ship it.
- Theming Orbit's dashboard. A distribution brings its own UI.

## Future ideas (not part of the shipped scope)

These ideas appeared in earlier proposals but are not current behavior or requirements:

- **YAML presets and deferred plugin seeding.** The shipped format is TOML. A preset
  enables only definitions that a plugin has already seeded, after workspace seeding; it
  does not defer enablement to a later plugin-seeding event.
- **Per-name routine delete records.** Routine list policies provide the durable opt-out.
  A separate delete verb or record is not shipped.
- **A dashboard count of excluded defaults.** This was proposed as a diagnostic surface but
  is not implemented; `orbit workspace show` and `orbit doctor` report the policy and its
  relevant diagnostic instead.
- **Distribution-specific install behavior in Orbit.** Orbit stays generic. Each
  distribution owns and documents its own installer and adoption work.

## Task References

- [ORB-19186] — implemented the seeding policy, presets and their workspace surfaces.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
