---
title: Distributions — Overview
owner: claude
created_date: 2026-10-27
updated_date: 2026-11-28
last_validated: 2026-11-24
status: Accepted
stage: shipped
next: "—"
feature: distributions
doc_role: overview
type: design
summary: "Name the role of an app built on Orbit (a distribution), and let it choose which shipped default routines and auto-tasks its workspaces carry through a persistent seeding policy and a TOML preset"
tags: [distributions, presets, routines, auto-tasks, plugins, workspace]
---

# Distributions — Overview

Status: accepted and shipped. Orbit supports persistent seeding policies and TOML
workspace presets. This page records that behavior; ideas that were superseded or remain
unbuilt are called out separately in [2_design.md](./2_design.md#future-ideas-not-part-of-the-shipped-scope).

Bearing: The [plugin standard](../plugins/1_overview.md) lets a package add tools, CLI groups,
panels, routines, auto-tasks, activities, jobs and skills from one manifest. Plugin-seeded
definitions land disabled and become active only through an operator act. Orbit also ships
7 default routines and 18 default auto-tasks: 25 defaults in total. By default it seeds all
25 into each workspace. The routines `worktree_gc` and `task_pilot` are seeded
enabled. Other shipped routines are seeded disabled. Among auto-tasks, `host-health` is also seeded disabled
([ORB-24934], reversing [ORB-21596]); enable it with
`orbit auto-task toggle host-health on`. The routine
`crew_performance_refresh` ([ORB-17816]), once seeded enabled,
was retired in [ORB-22804]. Seeded definitions carry managed
provenance and digest-based retirement ([ORB-10800]). A distribution can narrow the Orbit defaults its workspace carries and choose
which of its already-seeded plugin definitions to enable with a preset.

## Problem

An app built on Orbit is not itself a plugin. It is a product for an audience, with its own
app or UI, the components it installs, a workspace setup and an installation story. Calling
that product a **distribution** gives it a name distinct from the plugins that add
capabilities to Orbit.

Orbit's developer-focused defaults do not suit every distribution's workspace. Without a
persistent policy, an operator who removes defaults can see them return on re-init or an
upgrade. A distribution also needs a supported way to provide workspace settings and enable
some of its own plugin definitions after the operator has enabled those plugins.

## Goal

Name the role: an app built on Orbit is a **distribution**. Give each workspace a durable
way to choose which Orbit-shipped default routines and auto-tasks it carries, and let a
distribution apply workspace settings and enable definitions its plugins have already
seeded. Orbit remains generic: a preset is data the distribution supplies, and Orbit does
not branch on a distribution's name. Without a seeding policy, a workspace keeps the
default behavior of seeding every shipped default.

## Acceptance (feature level)

- With both seeding keys set to `"none"`, initialization, re-initialization, sync and
  upgrade seeding omit Orbit-shipped defaults.
- Narrowing a policy removes Orbit-owned defaults by digest and preserves edited defaults
  under `.orbit/.retired-managed/`.
- A valid preset applies seeding and config before default seeding and applies `[enable]`
  afterward, only for definitions that an enabled plugin has seeded.
- A preset with an unknown field or refused config key is rejected before it writes
  anything.
- Without an explicit seeding policy, Orbit seeds all shipped defaults, including new
  defaults added by a later release.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-10800] — gave seeded definitions managed provenance and digest-based retirement.
- [ORB-17816] — added `crew_performance_refresh`, the one default routine seeded enabled.
- [ORB-22804] — retired `crew_performance_refresh` with crew-performance weighting.
- [ORB-21596] — shipped the host-health auto-task enabled by default (superseded by ORB-24934).
- [ORB-24934] — shipped the host-health auto-task disabled by default.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
