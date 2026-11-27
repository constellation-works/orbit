---
title: Plugin Pages — Decisions
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-27
last_validated: 2026-11-27
status: Accepted
feature: plugin-pages
doc_role: decisions
type: design
summary: "Plugin Pages — Decisions: a page gets server-side work only through its plugin's tools, plugin code acts only behind a confirmation drawn outside its frame, and a page is one document whose frame fetches nothing"
tags: [plugin-pages]
related_features: [plugins]
related_artifacts: [ORB-29504, ORB-29573]
---

# Plugin Pages — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## A page gets server-side work only through its plugin's tools

**Recorded:** 2026-11 · [ORB-29504]

### Context

A plugin's page needs data and sometimes computation. The first sketch of this feature had
Orbit start a server process for each plugin's page. A page server needs a second runtime on
every host, a long-lived process and listener per plugin, plugin code that runs outside the
plugin sandbox and its grants, and a port that the dashboard's tunnel does not carry.

### Decision

Orbit starts no process for a page and gives it no network. Anything a page needs from the
server side, it asks of a tool its manifest names, through the audited tool dispatch. This
holds for needs nobody has raised yet: a page that wants live updates, a cache or a long
computation gets a tool, or a general bridge capability every page can use, never a process
or a socket of its own. A plugin that must run its own server keeps the link tile.

### Consequences

- A page is covered by the grants, sandbox and audit trail its plugin's backend already has.
- A page works wherever the dashboard is reached, with nothing to forward.
- Cost: a page has no server-side state between calls and cannot be pushed to. It polls, and
  with an `exec` backend each poll starts a process.

## Plugin code acts only behind a confirmation drawn outside its frame

**Recorded:** 2026-11 · [ORB-29504]

### Context

A page is plugin code running in the operator's browser. If it could call a mutating tool on
its own, a page could act with the operator's authority without the operator having decided
anything, and a click inside the frame proves nothing because the page draws the frame.

### Decision

A page reads freely through its declared read-only tools. Every call that changes something
needs the operator session and a confirmation that the dashboard draws outside the frame,
naming the plugin, the tool and the input. This applies to any later way a page might cause a
change, including one reached indirectly through a read-only tool: if a call can change
state, it is an action and takes the confirmation.

### Consequences

- The first pages are read-only, and actions arrive as a later phase.
- Cost: every action takes a second step from the operator, and a page cannot offer a
  one-click control for anything that writes.

## A page is one document, and its frame fetches nothing

**Recorded:** 2026-11 · [ORB-29573]

### Context

The design first served a page as a directory of files from an asset route, admitted by
`script-src 'self'`. Measured in browsers ([ORB-29532]), a frame with a unique origin could not
fetch its own files: WebKit matched `'self'` to nothing, and Chromium marked each request
cross-site, which the router's origin guard refuses. The ways to keep files were to accept
cross-site requests on one route, or to drop scripts and ship pages that only display.

### Decision

Orbit writes a page's markup, scripts and styles into one document and admits each script by
its hash. The frame makes one request, its own navigation, and nothing after it. The origin
guard is not widened for pages. A need that looks like a second request, such as a lazy
chunk, a late image or a larger asset, is met inside the document or through the bridge, not
by a route the frame can call.

### Consequences

- The guard, the session and the frame policy need no exception for pages, and the result is
  the same in every engine measured.
- Only the scripts Orbit wrote can run, which the first contract did not give.
- Cost: a page ships bundled scripts, carries images and fonts as `data:` URLs at about a
  third more bytes, and cannot split its code. Another host's page has to be written by the
  local dashboard from parts, which phase 2 must build.

## Task References

- [ORB-29504] — designed plugin pages; the operator approved the first two decisions with it.
- [ORB-29532] — measured the first page contract in Chromium and WebKit.
- [ORB-29573] — revised the page contract; the operator approved the third decision with it.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
