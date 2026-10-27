---
title: "Scope: Distributions and workspace presets"
owner: claude
last_updated: 2026-10-27
last_validated: 2026-10-27
status: Draft
stage: designing
next: "Build the seeding policy and presets (ORB-19186); orbit-office adopts them (ORB-19187)"
feature: distributions
doc_role: scope
type: design
summary: "Scope: name the role of an app built on Orbit (a distribution), and let it choose which of Orbit's shipped default routines and auto-tasks a workspace carries through a persisted seeding policy and a preset file"
tags: [distributions, presets, routines, auto-tasks, plugins, workspace]
---

# Scope: Distributions and workspace presets

Status: designing. Nothing is built; [ORB-19186] builds the Orbit side and
[ORB-19187] is the first distribution adopting it.

Bearing: The [plugin standard](../plugins/1_scope.md) lets a package add tools, CLI groups,
panels, routines, auto-tasks, activities, jobs and skills from one manifest. Plugin-seeded
definitions land `enabled: false` and become active only through an operator act.
`orbit workspace init` seeds eleven shipped default routines
([seeded defaults](../routines/2_design.md#seeded-defaults-and-ownership)) and 24 shipped
default auto-tasks ([managed seeding](../auto-tasks/2_design.md#5a-managed-seeding)). All of
them are disabled except `crew_performance_refresh` [ORB-17816]. Since [ORB-10800], seeding
is manifest-aware. Each kind records the digest Orbit last wrote, so a default Orbit wrote can be
retired by content, and an operator-edited one is preserved. Deleting a shipped
auto-task records a durable opt-out, and `orbit auto-task restore` clears it. Routines have
no opt-out: plain re-init recreates a deleted default.

## Problem

orbit-office is an app for office workers that runs on Orbit. It installs the `office` and
`gateway` plugins and creates the user's workspace at `~/orbit` with
`orbit workspace init`. That workspace then carries Orbit's developer automation:

- **35 definitions**: CI-failure and Dependabot sweeps, task pilot, finding verification,
  ship sweep, code review, flaky-test hunts and architecture audits, beside office's own
  four routines and two auto-tasks.
- **One running by default.** `crew_performance_refresh` records a job run every hour.
- **Visible clutter.** The app's Routines screen reads `orbit.auto_task.list`, so an office
  worker would see 24 developer auto-tasks they will never use, any of them one click
  from running.
- **No lasting way out.** Deleting the auto-tasks one by one survives, because delete
  records an opt-out. Deleting routines does not: re-init recreates them. Either way, the
  next Orbit release seeds any newly shipped default into every workspace.

There is a second gap behind the first: nothing in Orbit names what orbit-office *is*. It is
not a plugin. It is a product that uses Orbit as its engine and ships plugins as some of its
parts. With no name for that role, each such product invents its own install rules, and the
question "which of Orbit's defaults belong here?" has nowhere to live.

## Goal

Name the role: an app built on Orbit is a **distribution**. Give a distribution one
supported, durable way to choose which of Orbit's shipped default routines and auto-tasks its
workspaces carry, and which of its own plugin-seeded definitions start enabled. The choice
must survive re-init, `orbit workspace sync` and Orbit upgrades. Orbit stays generic: a
preset is data the distribution ships, and Orbit knows no particular distribution.
A workspace with no policy behaves exactly as it does today.

## Shape

### Vocabulary

| Term | Meaning | Example |
|---|---|---|
| **Engine** | Orbit: tasks, runs, routines, auto-tasks, the tool registry, MCP, the clock | `orbit` 0.34 |
| **Component** | A plugin: the unit that adds tools and definitions to the engine | `office`, `gateway`, `cloudcli` |
| **Distribution** | A product built on Orbit for one audience: its own app or UI, the components it installs, a preset, and an installer. It lives in its own repository and uses only Orbit's public surfaces | orbit-office |
| **Preset** | A file the distribution ships that says what a workspace it creates carries | `office.yaml` |

"Plugin" stays the right word for a component. It is the wrong word for the product.
A distribution may consist of a single plugin plus a preset; it is still a distribution
when it has an audience and an install story of its own.

### Who owns what

| Concern | Orbit | Distribution |
|---|---|---|
| Engine behaviour, defaults, upgrades | yes | pins a tested Orbit version |
| Tools and definitions | plugin standard | ships its components |
| Which shipped defaults a workspace carries | the seeding policy mechanism | the policy, in its preset |
| Which of its own definitions start enabled | honours the recorded preset | lists them in its preset |
| Plugin permissions | `orbit plugin enable` consent | asks the user, never bypasses |
| UI, onboarding, install script | — | all of it |

### Seeding policy

Two workspace config keys decide which shipped defaults are seeded:

```toml
[seeding]
routines = "none"                       # "all" (default) | "none" | ["worktree_gc", "store_gc"]
auto_tasks = "none"
```

Rules:

1. The policy is read by every path that seeds shipped defaults: `orbit workspace init`,
   plain re-init, `orbit workspace sync`, and the seeding an upgrade performs. A shipped
   default outside the policy is never written, including one a later release adds.
2. A list names shipped defaults by their base name, with no workspace suffix. An unknown
   name fails config load and names it.
3. Narrowing the policy on an existing workspace retires defaults through the managed-asset
   path. A file whose bytes match the digest Orbit recorded is removed. An operator-edited
   file is preserved under `.retired-managed/`, as release retirement does today. Widening
   it seeds the newly included defaults.
4. A default left out by policy is not "missing". Doctor and sync do not report it, and
   routines gain the same opt-out record auto-tasks already keep, so an explicit
   per-name delete also survives re-init.
5. `"all"` is the default and the absence of the section, so every existing workspace keeps
   today's behaviour.

### Presets

A preset is a small, versioned YAML file:

```yaml
schemaVersion: 1
kind: WorkspacePreset
name: office
seeding:
  routines: none
  auto_tasks: none
enable:                     # plugin-seeded definitions to start enabled
  routines: [office-daily-digest, office-kt-inbox, office-signals]
  auto_tasks: []
config:                     # only keys `orbit config set` accepts
  workflow.auto_ship: false
```

`orbit workspace init --preset <file>` applies it to a new or an existing workspace:

1. **Validated before any write.** Unknown fields are refused, and so is any `config` key
   that `orbit config set` refuses. A preset has no field that grants a permission, enables
   a plugin or names a crew pool, so it cannot ask for one.
2. **Recorded.** The workspace records the preset's `name` and the digest of its bytes,
   beside the seeding policy it wrote. Applying the same file again is a no-op. Applying a
   changed file is an update, and it follows the policy rules above.
3. **Enabling is deferred to seeding.** Workspace init runs before the user enables the
   plugins, because `orbit plugin enable` needs a workspace. So when a plugin seeds its
   definitions into a workspace, it seeds the ones the recorded preset names
   `enabled: true` and the rest `enabled: false`. The operator's preset is the operator
   act the plugin standard requires; a plugin still never enables its own schedules.
4. **No layering.** A workspace has at most one recorded preset. A later
   `--preset` replaces it.

### How a distribution installs

A distribution's installer (orbit-office's `install.sh` and `office init`, [ORB-19181])
does this, in order:

1. Install or check the pinned Orbit release, and never upgrade an existing one unasked.
2. `orbit plugin add` each component.
3. Create the folder and run `orbit workspace init --preset <its preset>`.
4. Run `orbit plugin enable <component>` in that workspace, after showing the grants and
   getting consent. Definitions the preset names come up enabled here.
5. Store secrets with `orbit plugin secret set`. Never write them to a file.
6. Start its own app, which talks to Orbit only through `orbit tool run`, MCP or the
   documented HTTP API, and never reads `.orbit/` directly.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `seeding.routines` | `"all"` | Shipped default routines this workspace carries: `"all"`, `"none"`, or a list of base names |
| `seeding.auto_tasks` | `"all"` | Shipped default auto-tasks this workspace carries, same forms |

The recorded preset (name and digest) is workspace state, not a config key. Operators change
it only by applying a preset.

### Surfaces

- `orbit workspace show` and its tool report the preset's name and digest, and the
  effective policy.
- `orbit doctor` stays quiet about defaults the policy excludes. It warns when
  `workflow.crew_performance` is not `off` while `crew_performance_refresh` is excluded,
  because draws would then read a snapshot that never refreshes.
- The dashboard's Automation view says "N shipped defaults not seeded (preset `office`)"
  rather than leaving the count silently smaller.
- `orbit auto-task list` and `orbit routine list` are unchanged: they list what exists.

## Non-goals

- **Packaging or hosting distributions.** Orbit does not build, publish, sign or update
  them. They ship from their own repositories with their own installers.
- **A distribution registry or an `orbit distribution` command.** The noun is a role,
  not an engine object. Revisit only if several distributions need Orbit to know about them.
- **Per-distribution behaviour in core.** Nothing in Orbit branches on a preset's `name`.
- **Granting permissions.** Consent stays with `orbit plugin enable`. A preset cannot widen
  what a plugin may do.
- **Removing a plugin's own definitions.** A distribution that does not want one of its own
  definitions should not ship it.
- **Theming the dashboard.** A distribution brings its own UI. Orbit's dashboard stays
  Orbit's.

## Concerns and honest limitations

- **Excluding `crew_performance_refresh`.** Crew performance draws then have no fresh
  snapshot. That is harmless while `workflow.crew_performance` is `off`, the default, and
  doctor warns otherwise.
- **Surprise for developers.** Someone who later wants a developer default in a preset
  workspace finds it absent. The fix is to widen the policy or run `orbit auto-task restore`,
  and `orbit workspace show` makes the policy visible.
- **Deferred enabling ties preset to plugin seeding.** If the preset is applied after the
  plugin was already enabled, the named definitions already exist, disabled. Applying the
  preset then enables them directly, which needs the same refusal rules for definitions an
  operator has edited.
- **Narrowing is destructive for Orbit-written bytes only.** Retirement proves authorship by
  digest. A workspace created before managed provenance has no digests, so its defaults are
  preserved rather than removed, and sync says so.

## Phasing

1. **Seeding policy.** `seeding.*` keys honoured by init, re-init, sync and upgrade seeding.
   Routine opt-out parity. Retirement on narrowing [ORB-19186].
2. **Presets.** The file format, `orbit workspace init --preset`, the recorded preset, and
   plugin seeding consulting it for `enable` [ORB-19186].
3. **Surfaces.** `workspace show`, the doctor rules and the dashboard count [ORB-19186].
4. **First distribution.** orbit-office ships `office.yaml`, applies it from `office init`
   and its installer, and adopts the distribution wording [ORB-19187].

## Open questions

1. **Key names?** `seeding.*` describes the mechanism; `defaults.*` reads better to an
   operator but collides with the many "default" values in CONFIG.md.
2. **Should a single-plugin distribution declare its preset in the plugin manifest?** It
   would save a file. But a plugin would then shape Orbit's own defaults, which crosses
   the capability boundary the plugin standard draws. Leaning no.
3. **Should presets carry crew or model routing?** An office workspace wants a cheaper
   default crew. But crew pools are host-owned operator settings, and a preset that
   names crews would not travel between hosts. Leaning no for now.

## Acceptance (feature level)

- Given `seeding.routines = "none"` and `seeding.auto_tasks = "none"`, Orbit seeds no shipped
  default on init, re-init, sync or after an upgrade that ships a new default.
- Given an existing workspace narrowed from `all` to `none`, Orbit removes the defaults it
  wrote and preserves operator-edited ones.
- Given `orbit workspace init --preset office.yaml` and then `orbit plugin enable office`,
  the workspace lists no Orbit shipped default, and office's named routines are enabled
  while its others are disabled.
- Given a preset with an unknown field or a refused config key, Orbit writes nothing and
  names the problem.
- Given no seeding section, Orbit seeds exactly what it does today.

## Task References

- [ORB-10800] — gave seeded definitions managed provenance and digest-based retirement.
- [ORB-17816] — added `crew_performance_refresh`, the one default seeded enabled.
- [ORB-19181] — builds orbit-office's `office init`, which applies the preset.
- [ORB-19186] — builds the seeding policy, presets and their surfaces.
- [ORB-19187] — makes orbit-office the first distribution to ship a preset.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
