---
title: Claude Code Mod — Overview
owner: claude
created_date: 2026-11-26
updated_date: 2026-11-28
last_validated: 2026-11-27
status: Accepted
stage: building
next: "Trial peer-message links in Claude Code [ORB-28918]"
feature: claude-code-mod
doc_role: overview
type: design
summary: "Replace the Orbit plugin's band, status line, toasts, pane, prompt cards and commit trailer with one behaviour, task and run ids in the transcript drawn as links to the dashboard, with task prefixes read from the host registry, dashboard permalinks that pin the host, and a Codex Desktop trial of links the model writes from a result's url"
tags: [claude-code-mod, plugin, dashboard, host-registry]
---

# Claude Code Mod — Overview

Status: design approved by Daniel on 2026-11-26. Phases 1 to 4 have landed ([ORB-28913],
[ORB-28914], [ORB-28915], [ORB-28916], [ORB-28917]); phase 5 remains, so the stage is
`building`.

## 1. Motivation

Bearing: the Orbit plugin's hooks module ([`plugin/hooks/mod`](../../../plugin/hooks/mod/README.md),
built by [ORB-13862]) draws a band, a status line, toasts and a Board / Ship pane in Claude
Code, adds a task card to prompts and a trailer to commits. The
[host registry](../host-registry/1_overview.md) ([ORB-14448]) records each machine's
`task_prefix`, and the dashboard (`orbit web serve`) shows a task at `#tasks/<id>`, a run at
`#runs/<id>` and another host's with `?host=<name>`. Daniel asked for this on 2026-11-26: a
personal mod that links task ids "improves quality of life", and the existing drawings are
to go "as I don't find them useful". He then asked for run ids as well
(`jrun-20261126-0531-c3`).

### The gap

The mod is 1,741 lines with 685 lines of tests, nine hooks, four settings and three
commands. What it costs and what it returns:

- **It polls.** Every session that loads the plugin runs `orbit task list` every three
  minutes by default (`refreshMinutes` sets the periodic refresh interval). A completed
  turn also refreshes, but only when the snapshot is missing or the last refresh attempt
  is more than 30 seconds old. The reads fill the band. In a replica checkout each read is
  an SSH call to the owner. Ten sessions open on one machine make ten such loops.
- **It repeats the dashboard.** The band's counts, the board's lanes and the ship tracker
  are the dashboard's Tasks and Jobs views, drawn again in a narrower space. Approve,
  reject, ship and accept are already MCP tools and CLI commands.
- **The prompt card is unasked context.** A prompt that names a task carries that task's
  card from the last successful snapshot. That snapshot is up to one refresh interval old
  while reads succeed. After a failed refresh, or with a longer `refreshMinutes`, it is
  older, and the card shows no age. The prompt hook checks neither the snapshot's age nor
  the last refresh error. The model can read the current task with `orbit.task.show` when
  it needs it.
- **The commit trailer is one Orbit does not read.** The mod adds `Task: <id>`. Delivery
  finds a task's commits by `[<id>]` in the subject or an `Orbit-Task: <id>` trailer
  (`vcs/delivery_marker.rs`), so `Task:` links nothing.
- **Its tests do not run in CI.** `claude plugin test` needs Claude Code, which no CI job has.

A second, personal mod (462 lines with its tests) does one thing: it draws `ORB-123` in
an assistant message as a link to that task on the dashboard. It runs no process, keeps
no state and changes only the drawing. That is the behaviour worth shipping. As written
it cannot ship:

- Its three prefixes are fixed in the code, and each has its own URL setting whose default
  names one person's machine.
- Each URL repeats the dashboard's query grammar (`?host=…&workspace=all#tasks/{id}`), and
  must say which host serves which prefix. The dashboard already knows that.
- It links ids in the assistant's text only. Ids in a message another session sent stay
  plain.
- It links tasks only. A run id, which a failure report usually leads with, stays plain.

Codex loads the plugin's skills and MCP server only. It has no hooks module, and the review
of this proposal found no documented Codex equivalent of Claude Code's `ui.render` for
redrawing a transcript row. In Codex a link can exist only where the model writes one.

### Intended outcome

In Claude Code, a task id or a run id in the transcript is a link to its page on the
dashboard, with no plugin setting and no background work. In Codex Desktop, a reply that
reports a task or a run links it, if a trial shows the link works there. Everything else
the mod draws or injects is removed. The plugin's skills, MCP server and the session-start
workspace check are unchanged.

### Out of scope

- **Task data in the session.** No hover title, status badge or count. Each needs the read
  this feature removes; the link opens the page that has them.
- **Actions from the session.** Approve, ship, accept and rescue stay with the MCP tools,
  the CLI and the dashboard.
- **Friction, decision and notice ids.** Only task and run ids are linked.
- **Redrawing Codex's transcript.** No hook for it is documented. Earlier rows and other
  sessions' messages stay plain there.
- **A link on every list row.** A list result carries no `url`.
- **Rewriting what the model reads.** The link exists in the drawing only.

### Acceptance (feature level)

- Given a reply that names `ORB-12` in a sentence and in a fenced command, Claude Code
  draws the first as a link to `<address>/t/ORB-12` and leaves the second as text.
- Given a machine whose registry holds prefixes `ORB` and `ORBA`, `ORBA-5` and `ORB-5` link
  to their own ids, and `STD-12` is not linked.
- Given a browser that last selected a remote host, a permalink for a task of the serving
  machine, opened in a new tab, shows that task on the serving machine.
- Given `GET /t/<id>` for a registered host's prefix, the dashboard opens that host's view
  of the task; for an unknown prefix it shows not found.
- Given two hosts that each hold a run with the same id, a link emitted for the remote run
  (`/r/<id>?machine=…&workspace=…`) opens the remote record, and the bare `/r/<id>` opens
  the serving machine's.
- Given `/r/<id>?machine=<an unregistered machine>`, the dashboard shows not found and
  names the machine; it does not open a local run.
- Given a Codex Desktop reply that reports one task and one run from results carrying
  `url`, each id in prose is a link that opens the right host, workspace and record when
  clicked, and the same ids in a command are plain text.
- Given a reply in Claude Code that names `jrun-20261126-0531-c3`, it is drawn as a link to
  `<address>/r/jrun-20261126-0531-c3`.
- Given a session with the plugin loaded and idle for an hour, the mod has run two short
  local commands at session start (the host list and the address) and no timer.
- The plugin has no band, pane, status line entry, toast, prompt card, commit rewrite,
  slash command or setting.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Mod.** The Orbit plugin's Claude Code hooks module, which this feature cuts down to drawing task and run ids as links.
- **Permalink.** A short dashboard address: `/t/<id>` for a task and `/r/<id>` for a run, with `?machine=…&workspace=…` naming another host's record.
- **Host registry.** The record of each machine's `task_prefix`, which decides which host serves a task id; a prefix that is not registered is not linked.
- **Link address.** The dashboard's public URL, read once at session start (`machine.web.public_url`) rather than from a plugin setting.
- **Exact URL.** The `url` that task and run results carry, so a Codex Desktop reply can link a record without redrawing the transcript.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| The mod being replaced | [plugin/hooks/mod](../../../plugin/hooks/mod/README.md) | [ORB-13862] |
| Host registry and task prefixes | [host-registry](../host-registry/1_overview.md) | [ORB-14448] |
| What stays and what goes | [2_design.md](./2_design.md#what-stays-and-what-goes) | [ORB-28916] |
| Public address setting | [2_design.md](./2_design.md#configuration) | [ORB-28914] |
| Task and run permalinks | [2_design.md](./2_design.md#where-a-link-goes) | [ORB-28915] |
| Codex Desktop links and exact URLs | [2_design.md](./2_design.md#codex-desktop) | [ORB-28913], [ORB-28917] |
| Which words are ids | [2_design.md](./2_design.md#which-words-are-ids) | [ORB-28916] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-13862]: built the mod this feature replaces.
- [ORB-14448]: built the host registry the prefixes are read from.
- [ORB-28913]: ran the Codex Desktop trial, which passed.
- [ORB-28914]: adds the `machine.web.public_url` setting.
- [ORB-28915]: adds the task and run permalinks.
- [ORB-28916]: replaces the mod with task and run links.
- [ORB-28917]: returns exact URLs in results and adds the Codex skill line.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
