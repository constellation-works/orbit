---
title: "Scope: the Claude Code mod links task and run ids and draws nothing else"
owner: claude
created_date: 2026-11-26
updated_date: 2026-11-26
last_validated: 2026-11-26
status: Draft
stage: designing
next: "Daniel reviews this proposal; no task is filed until he approves it"
feature: claude-code-mod
doc_role: scope
type: design
summary: "Scope: replace the Orbit plugin's band, status line, toasts, pane, prompt cards and commit trailer with one behaviour, task and run ids in the transcript drawn as links to the dashboard, with task prefixes read from the host registry and dashboard permalinks that resolve the host"
tags: [claude-code-mod, plugin, dashboard, host-registry]
---

# Scope: the Claude Code mod links task and run ids and draws nothing else

Status: a proposal. Nothing here is built or approved.
Bearing: the Orbit plugin's hooks module ([`plugin/hooks/mod`](../../../plugin/hooks/mod/README.md),
built by [ORB-13862]) draws a band, a status line, toasts and a Board / Ship pane in Claude
Code, adds a task card to prompts and a trailer to commits. The
[host registry](../host-registry/1_overview.md) ([ORB-14448]) records each machine's
`task_prefix`, and the dashboard (`orbit web serve`) shows a task at `#tasks/<id>`, a run at
`#runs/<id>` and another host's with `?host=<name>`. Daniel asked for this on 2026-11-26: a
personal mod that links task ids "improves quality of life", and the existing drawings are
to go "as I don't find them useful". He then asked for run ids as well
(`jrun-20261126-0531-c3`).

## Problem

The mod is 1,741 lines with 685 lines of tests, nine hooks, four settings and three
commands. What it costs and what it returns:

- **It polls.** Every session that loads the plugin runs `orbit task list` every three
  minutes and again after a turn, to fill the band. In a replica checkout each read is an
  SSH call to the owner. Ten sessions open on one machine make ten such loops.
- **It repeats the dashboard.** The band's counts, the board's lanes and the ship tracker
  are the dashboard's Tasks and Jobs views, drawn again in a narrower space. Approve,
  reject, ship and accept are already MCP tools and CLI commands.
- **The prompt card is unasked context.** A prompt that names a task carries that task's
  card from a snapshot up to three minutes old. The model can read the current task with
  `orbit.task.show` when it needs it.
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

Codex loads the plugin's skills and MCP server only. It has no hooks module, so nothing in
this scope reaches a Codex session.

## Goal

In Claude Code, a task id or a run id in the transcript is a link to its page on the
dashboard, with one setting and no background work. Everything else the mod draws or
injects is removed. The plugin's skills, MCP server and the session-start workspace check
are unchanged.

## Shape

### What stays and what goes

| Part | Decision | Reason |
|---|---|---|
| Task id links | **Added**, from the personal mod | The one behaviour in daily use |
| Run id links | **Added** | Same mechanism; a run id is what a failure report names |
| Band above the prompt | Removed | The dashboard shows the counts; it is the reason for the poll |
| Status line entry | Removed | Fed by the same poll |
| Toasts on blocked, review, done | Removed | Fed by the same poll; attention reports these |
| Board / Ship pane, `/orbit-board`, `/orbit-ship`, `/orbit-band` | Removed | The dashboard and the MCP tools do each action |
| Task card on a prompt | Removed | Stale and unasked; `orbit.task.show` is current |
| `Task:` commit trailer | Removed | Orbit reads `[<id>]` and `Orbit-Task:`, not `Task:` |
| `ownerHost`, `refreshMinutes`, `band`, `commitTrailer` | Removed | Nothing left reads them |
| Session state contract (`plugin/types/index.d.ts`) | Removed | The mod keeps no state |
| `check-workspace.sh` at session start | Kept | A shell hook, not part of the mod; it costs one line |

After the change the module is one `session.start` hook, one `ui.render` hook and a pure
function.

### Linking

The rules of the personal mod carry over unchanged, with its 54 cases:

- An id in a paragraph, heading, list item, quote or table cell is drawn as
  `[ORB-123](<url>)`. A run id is drawn the same way.
- An id is left alone inside inline code, a fenced or indented code block, an existing
  link or its definition, an HTML tag and a bare URL. An id in a command stays copyable.
- The stored message is not changed, only its drawing. A reply whose rewritten text would
  pass the engine's 100,000-character limit is drawn as it was.
- The function is pure (text, prefixes and address in, text out), so it is tested without Claude
  Code.

### Which words are ids

A pattern such as `[A-Z]{2,5}-\d+` would also link `STD-12`, `UTF-8` and `SHA-256`. The mod
links only the prefixes Orbit knows:

1. At `session.start` it runs `orbit host list --no-probe --json` once. That prints this
   machine's `task_prefix` and each registered host's, from the stored registry, and
   contacts no host.
2. The pattern is built from those prefixes, longest first, so `ORBA-5` is not read as
   `ORB` followed by `A-5`.
3. When the call fails or `orbit` is not installed, the mod links no task and says nothing.
   A session's prefixes are fixed at its start.

A run id needs no list. Its shape is Orbit's own and matches nothing else:
`jrun-<YYYYmmdd>-<HHMM>`, then an optional `-<role letter><sequence>` (`workflow/run/id.rs`;
older ids have a bare stem or a plain number). Run ids are linked even when the host list
could not be read.

### Where a link goes

The dashboard gains a permalink, `GET /t/<id>`. It resolves the id's prefix as the CLI
does (`hosts/route.rs`): the serving machine's own prefix redirects to
`/?workspace=all#tasks/<id>`, a registered host's to the same with `?host=<name>`, and an
unknown prefix to the dashboard's not-found view for that id. The redirect target is the
dashboard's to change; a link written into a pull request, a mail or a transcript keeps
working.

A second permalink, `GET /r/<id>`, does the same for a run and redirects to
`#runs/<id>`. A run id carries no prefix, so it does not say which host ran it. The
dashboard looks on the serving machine; on a miss it shows not found for that id beside
the host switch. It does not search the other hosts.

The mod's links are then `<dashboardUrl>/t/<id>` and `<dashboardUrl>/r/<id>`.

### Configuration

| Option | Default | Meaning |
|---|---|---|
| `dashboardUrl` | `http://localhost:7878` | Where the dashboard is reached from this machine: a local `orbit web serve`, an `orbit web connect` tunnel, or a Tailscale Serve name. Empty turns linking off. A value that contains `{id}` is used whole as the task link template, for a dashboard older than the permalinks; run ids are then not linked. |

Only `http`, `https` and `file` addresses are accepted, as today.

### Surfaces

- **Claude Code** terminal, desktop app and VS Code: the engine raises the assistant
  message's `ui.render` on every surface, so one hook covers them.
- **Codex**: none.
- **Docs**: the mod's README and the website page
  [Use the Claude Code mod](../../../website/src/content/docs/how-to/claude-code.md) are
  rewritten; `hooks.json` and the plugin description lose the band and pane. Removing the
  settings and commands is a breaking change, so the implementing task carries the
  `breaking-change` tag.
- **CI**: the pure linking cases run under Node in `make ci-guards`. The two tests that
  mount the hook stay under `claude plugin test`, run by hand.

## Non-goals

- **Task data in the session.** No hover title, status badge or count. Each needs the read
  this scope removes; the link opens the page that has them.
- **Actions from the session.** Approve, ship, accept and rescue stay with the MCP tools,
  the CLI and the dashboard.
- **Friction, decision and notice ids.** Only task and run ids are linked.
- **Codex.** A skill line telling the model to write ids as links would spend tokens on
  every reply and depend on the model doing it.
- **Rewriting what the model reads.** The link exists in the drawing only.

## Concerns and honest limitations

- **Removal is breaking and there is no usage data.** Anyone who uses the band, the pane
  or the three commands loses them. The one known user asked for the removal. Old settings
  left in a user's configuration are ignored.
- **A dead link when no dashboard listens.** The default address assumes a local server or
  a tunnel. A click then fails in the browser; the mod does not probe.
- **The markdown reading is the mod's own.** It is a 274-line scanner, not the engine's
  parser. Where they could disagree it leaves the id plain: a missed link, never broken
  markdown. The 54 cases are the record of what it handles.
- **A host registered mid-session is not linked** until the next session.
- **A run on another host is not found by its link.** The id does not name its host, and
  `/r/<id>` looks only on the serving machine. The reader picks the host from the switch.
  Searching the registered hosts on a miss would cost one call per host and is left out
  until the miss is felt.
- **The task permalink trusts the serving machine's registry.** A prefix that machine has not
  registered is not found, even when the linking machine knows it.

## Phasing

1. **Dashboard permalinks.** `GET /t/<id>` with the three outcomes above, and `GET /r/<id>`.
2. **Replace the mod.** Move the linking function and its cases into `plugin/hooks/mod`,
   add the run pattern, read the prefixes at session start, add `dashboardUrl`, delete the
   rest, update the docs. Task links do not wait for phase 1, since a `{id}` template covers
   the gap. Run links do.
3. **Messages from other sessions (a trial first).** A peer's message, a task
   notification and hook feedback arrive as user rows, and their ids stay plain. The
   engine documents an assistant message's text as markdown and a user row's as printable
   text. The trial: rewrite a user row's text with a markdown link, and separately draw
   the row from the hook's own tree with link elements, on terminal and desktop. Whichever
   keeps the engine's row and gives a working link ships; if neither does, this phase is
   dropped.

## Open questions

1. **Should the address come from Orbit, not a plugin setting?** If a host's registry
   entry recorded where its dashboard is reached, the mod would need no setting. That adds
   a field to the registry for one reader.
2. **An id inside code.** A desktop context-menu row ("Open ORB-123 in Orbit") could cover
   ids in code blocks and tool output without touching the text. It is left out until
   someone misses it.

## Acceptance (feature level)

- Given a reply that names `ORB-12` in a sentence and in a fenced command, Claude Code
  draws the first as a link to `<dashboardUrl>/t/ORB-12` and leaves the second as text.
- Given a machine whose registry holds prefixes `ORB` and `ORBA`, `ORBA-5` and `ORB-5` link
  to their own ids, and `STD-12` is not linked.
- Given `GET /t/<id>` for a registered host's prefix, the dashboard opens that host's view
  of the task; for an unknown prefix it shows not found.
- Given a reply that names `jrun-20261126-0531-c3`, Claude Code draws it as a link to
  `<dashboardUrl>/r/jrun-20261126-0531-c3`, and the dashboard opens that run when the
  serving machine holds it.
- Given a session with the plugin loaded and idle for an hour, the mod has started one
  process (the host list at session start) and no timer.
- The plugin has no band, pane, status line entry, toast, prompt card, commit rewrite or
  slash command, and its only setting is `dashboardUrl`.

## Task References

- [ORB-13862]: built the mod this scope replaces.
- [ORB-14448]: built the host registry the prefixes are read from.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
