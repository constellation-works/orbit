---
title: Plugin Pages — Overview
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-27
last_validated: 2026-11-27
status: Draft
stage: designing
next: "Operator approval of this design [ORB-29504]"
feature: plugin-pages
doc_role: overview
type: design
summary: "A plugin ships a static web bundle that the dashboard serves in a sandboxed frame as a sub-tab under Plugins, reading data only through the plugin's own tools"
tags: [plugin-pages, plugins, dashboard, orbit-web]
related_features: [plugins, user-interface, remote-access]
related_artifacts: [ORB-29504]
---

# Plugin Pages — Overview

A plugin page is a screen a plugin ships and the dashboard shows as a sub-tab under Plugins. The
plugin supplies built static files. The dashboard serves them in a sandboxed frame, and the page
gets data only by calling tools the plugin's manifest names. It exists so that a plugin such as
Scribe or Formation can show the operator more than a table, without plugin code ever running
with the dashboard's authority. It is a draft for the operator's approval ([ORB-29504]) and
nothing in it is built.

## 1. Motivation

The [plugin standard](../plugins/1_scope.md) gives a plugin one manifest, one sandboxed backend,
consented grants and audited tool calls. Its dashboard contribution is declarative: panels drawn
by one generic renderer, and link tiles ([ORB-12738]).

A plugin cannot show the operator anything a table does not fit.

| Observed | Cause |
|---|---|
| Formation's roster view, mail log and status designs each end with a pane or a page to be built later, and have nowhere to put one. Scribe has no view outside the terminal | A panel is `kv`, `table`, `markdown` or `json` over one read-only tool. It cannot draw a tree, group a thread or lay out several reads on one screen |
| A panel cannot be filtered. A mail log shows its default fifty rows and nothing else | The panel read passes the tool no input |
| A plugin that wants its own screen links to a server it hosts itself | The link tile opens another port in a new browser tab. That port is not carried by `orbit web connect` or by the host picker, so the screen is missing wherever the dashboard is reached from another machine |
| The Plugins tab is one list | The standard says plugins cannot add tabs, and `router.js` has one `plugins` entry |

The standard excluded dashboard code on purpose: script from a plugin would run with the
dashboard's origin and could call every `/api/*` route the operator's session can. That
reason holds, and the design keeps it.

**Goal.** A plugin ships a page, and the operator opens it as a sub-tab under Plugins, on the local host
or any host the dashboard reaches. The page draws whatever it wants and gets its data only by
calling tools its own manifest names, through the same audited dispatch as every other tool
call. Orbit runs no new process and needs no new runtime to serve it. Panels, links and the
rest of the plugin standard are unchanged, and plugins still add no top-level destination.

**Not in it:**

- **Top-level destinations.** Pages live under Plugins. The rail's ten destinations stay
  Orbit's.
- **Changing what a panel is.** Panels keep one renderer and no input. A plugin whose data fits
  a table should still use one, since it costs no bundle.
- **Building pages.** No install-time build for a bundle and no dependency fetch. The consented
  build the standard has is for the backend.
- **The Scribe and Formation pages themselves.** Each needs a plugin manifest, read-only tools
  over its CLI's JSON, and a bundle, designed in its own repository.
- **Pages for task or run detail.** A plugin does not add sections to Orbit's own screens.

## 2. Core Concepts

| Term | Meaning |
|---|---|
| Page | One entry under `spec.web.pages` in a plugin's manifest: an id, a title, a directory and the tools it may call |
| Bundle | The page's directory of built static files, with `index.html` as its entry. Its digest is recorded at install |
| Frame | The sandboxed frame the dashboard draws the bundle in. It has a unique origin and no network |
| Bridge | The `postMessage` exchange by which a page asks the dashboard to call one of its declared tools |
| Kit | A script and a stylesheet Orbit serves, which wrap the bridge and carry the dashboard's look |

## 3. At a Glance

Nothing is built, so the table maps each concern to the design section that owns it and the
phase that delivers it.

| Concern | Design section | Phase |
|---|---|---|
| What a plugin declares, and what validation refuses | [The manifest](./2_design.md#1-the-manifest) | 1 |
| How the files are served, and what the frame cannot do | [Serving](./2_design.md#2-serving) | 1 |
| How a page gets data | [The bridge](./2_design.md#3-the-bridge) | 1 |
| Where the operator finds a page | [Surfaces](./2_design.md#4-surfaces) | 1 |
| Pages of a host reached through this dashboard | [Other hosts](./2_design.md#5-other-hosts) | 2 |
| Why Orbit starts no server for a page | [Why no server per plugin](./2_design.md#6-why-no-server-per-plugin) | — |
| A page that changes something | [Phasing](./2_design.md#8-phasing) | 3 |
| The Scribe and Formation pages | [Phasing](./2_design.md#8-phasing) | 4 |

## 4. Open for approval

1. **A static bundle, or a server per plugin?** The design recommends the bundle, for the
   reasons in [Why no server per plugin](./2_design.md#6-why-no-server-per-plugin). The cost is
   that a page cannot hold server-side state or push; it asks its tools.
2. **Actions in the first build, or after?** The design recommends after, as phase 3. A page
   that can settle a request or close a room is plugin code acting with the operator's
   authority, and the confirmation drawn outside the frame is what keeps that honest. The cost
   of waiting is that the first Formation page reads and does not answer.

## Task References

- [ORB-12738] — added the Plugins tab, the generic panel renderer and link tiles.
- [ORB-29504] — this design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
