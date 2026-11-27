---
title: "Scope: the Claude Code mod links task and run ids and draws nothing else"
owner: claude
created_date: 2026-11-26
updated_date: 2026-11-26
last_validated: 2026-11-26
status: Accepted
stage: designing
next: "ORB-28914 (address key) and ORB-28915 (permalinks) wait for approval to build"
feature: claude-code-mod
doc_role: scope
type: design
summary: "Scope: replace the Orbit plugin's band, status line, toasts, pane, prompt cards and commit trailer with one behaviour, task and run ids in the transcript drawn as links to the dashboard, with task prefixes read from the host registry, dashboard permalinks that pin the host, and a Codex Desktop trial of links the model writes from a result's url"
tags: [claude-code-mod, plugin, dashboard, host-registry]
---

# Scope: the Claude Code mod links task and run ids and draws nothing else

Status: design approved by Daniel on 2026-11-26, in the Orbit captain's chat ("yes, I
approve the revised design"). Phase 1, the Codex Desktop trial, passed ([ORB-28913]). No
code is built: the tasks for phases 2 to 5 are filed and wait for approval, so the stage
stays `designing`.
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

Codex loads the plugin's skills and MCP server only. It has no hooks module, and the review
of this proposal found no documented Codex equivalent of Claude Code's `ui.render` for
redrawing a transcript row. In Codex a link can exist only where the model writes one.

## Goal

In Claude Code, a task id or a run id in the transcript is a link to its page on the
dashboard, with no plugin setting and no background work. In Codex Desktop, a reply that
reports a task or a run links it, if a trial shows the link works there. Everything else
the mod draws or injects is removed. The plugin's skills, MCP server and the session-start
workspace check are unchanged.

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
- The function is pure (text, prefixes and address in, text out), so it is tested without
  Claude Code.

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

The dashboard gains two permalinks. Their redirect targets are the dashboard's to change, so
a link written into a transcript, a pull request or a mail keeps working. Every redirect
names the host. The dashboard restores the last remote host a browser chose when the query
has no `host`, and an explicit empty `host=` selects the serving machine
(`dashboard/js/host-switch.js`), so a target without it would open on whichever host was
used last.

**A task: `GET /t/<id>`.** The id's prefix is resolved as the CLI does (`hosts/route.rs`).

| Prefix | Redirect |
|---|---|
| The serving machine's | `/?host=&workspace=all#tasks/<id>` |
| A registered host's | `/?host=<name>&workspace=all#tasks/<id>` |
| Unknown | The dashboard's not-found view for that id |

**A run: `GET /r/<id>`.** A run id carries no prefix. Ids are allocated per host
(`repository/job_run/write.rs`), so two hosts can mint the same id in the same minute, and
the id alone does not say which run is meant. The permalink has two forms:

| Form | Meaning | Redirect |
|---|---|---|
| `/r/<id>?machine=<machine id>&workspace=<workspace>` | That exact run. `machine` is the stable `[machine] id`; the dashboard maps it to the serving machine or to a registered host's name. | `/?host=<name or empty>&workspace=<workspace>#runs/<id>` |
| `/r/<id>` | A lookup on the serving machine only | `/?host=&workspace=all#runs/<id>` |

A `machine` the serving machine has not registered is not found, and names that machine. It
never falls back to a local lookup: that is how a link would open an unrelated run with the
same id. A producer that knows where a run executed writes the first form. The mod sees only
text, so it writes the second.

The mod's links are `<address>/t/<id>` and `<address>/r/<id>`.

### Codex Desktop

Codex cannot redraw a row, so the link has to be in the reply the model writes. Two parts
make that cheap:

- **A result carries its own link.** A tool or CLI result that describes one task or one
  run gains `url`: the task permalink, or the run permalink in its exact form, with the
  machine and workspace the producer already knows. A list result carries none, since a URL
  per row would spend context on rows nobody opens.
- **One skill line.** When a result carries `url`, write the id in prose as a Markdown
  link to it; leave an id in a command or in code as text, so it stays copyable.

This needs no polling and no extra lookup: the address arrives with the record. It covers
newly written replies only, not earlier rows or messages from other sessions.

It was to ship only after a trial in Codex Desktop, which needed no code: give the session
the address of one task and one run by hand, have it reply with each id linked in prose and
repeated in a command, click both links, and record the host, the workspace and the record
that open.

The trial passed on 2026-11-26 ([ORB-28913]). With a Tailscale Serve address, a Desktop
reply linked one task and one run in prose and repeated the bare ids in fenced commands;
both clicks opened the serving host, the right workspace and the right record. With a
loopback address the task link worked and the run link did not open when clicked, though
direct navigation reached both. The cause was not established.

In Claude Code the mod links ids without the model's help, and it leaves an
existing link alone, so a link the model wrote from `url` is kept as written.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `machine.web.public_url` | unset | The address this machine's operator opens the dashboard at: a Tailscale Serve name, or `http://localhost:<port>` for a local server or an `orbit web connect` tunnel, where `<port>` is the port that server or tunnel listens on. `http` or `https` only. Global. |

One fact, two readers:

- **Results** carry `url` only when the producing machine has the key set. An unset key
  adds nothing, since `localhost` would be wrong on the machine that reads the result.
- **The mod** reads the key at `session.start` on the machine it runs on. Unset, it uses
  `http://localhost:7878`, the conventional port, without checking which port answers. That
  is right only when the dashboard listens on 7878. A local `orbit web serve --port <N>`, or
  an `orbit web connect --port <N>` tunnel on another port, can leave the link pointing at
  nothing or at another local listener, so set the key to the origin the dashboard actually
  answers on. Without `--port`, `orbit web connect` picks an ephemeral port instead of 7878
  when 7878 is busy, so the default can be wrong even when nobody chose another port.

The plugin has no setting of its own.

### Surfaces

- **Claude Code** terminal, desktop app and VS Code: the engine raises the assistant
  message's `ui.render` on every surface, so one hook covers them.
- **Codex Desktop**: links in replies the model writes, from a result's `url`, after the
  trial.
- **MCP and CLI results**: `url` on a result that describes one task or one run.
- **Docs**: the mod's README and the website page
  [Use the Claude Code mod](../../../website/src/content/docs/how-to/claude-code.md) are
  rewritten; `hooks.json` and the plugin description lose the band and pane;
  [CONFIG.md](../../CONFIG.md) gains the key. Removing the settings and commands is a
  breaking change, so the implementing task carries the `breaking-change` tag.
- **CI**: the pure linking cases run under Node in `make ci-guards`. The two tests that
  mount the hook stay under `claude plugin test`, run by hand.

## Non-goals

- **Task data in the session.** No hover title, status badge or count. Each needs the read
  this scope removes; the link opens the page that has them.
- **Actions from the session.** Approve, ship, accept and rescue stay with the MCP tools,
  the CLI and the dashboard.
- **Friction, decision and notice ids.** Only task and run ids are linked.
- **Redrawing Codex's transcript.** No hook for it is documented. Earlier rows and other
  sessions' messages stay plain there.
- **A link on every list row.** A list result carries no `url`.
- **Rewriting what the model reads.** The link exists in the drawing only.

## Concerns and honest limitations

- **Removal is breaking and there is no usage data.** Anyone who uses the band, the pane
  or the three commands loses them. The one known user asked for the removal. Old settings
  left in a user's configuration are ignored.
- **A dead link when no dashboard listens.** With the key unset the mod assumes a local
  server or a tunnel. A click then fails in the browser; the mod does not probe.
- **A loopback run link failed on a click in the trial.** The cause is unknown, so the
  loopback default is the weaker address until it is found. A set key avoids it.
- **Codex links cost output and depend on the model.** Each link is about 25 more tokens
  than the bare id, and the model may skip one. The trial linked both of its ids and did
  not measure how often a model skips one.
- **The markdown reading is the mod's own.** It is a 274-line scanner, not the engine's
  parser. Where they could disagree it leaves the id plain: a missed link, never broken
  markdown. The 54 cases are the record of what it handles.
- **A host registered mid-session is not linked** until the next session.
- **The mod's run link is a lookup, not an exact address.** It is written from text and
  cannot know the host. When the serving machine and another host both hold the id, it
  opens the serving machine's run. The run page names its host and workspace. A link
  written from a result's `url` is exact.
- **The task permalink trusts the serving machine's registry.** A prefix that machine has not
  registered is not found, even when the linking machine knows it.

## Phasing

1. **Codex Desktop trial** ([ORB-28913], done). No code. It passed, so phase 4 may be built.
2. **Dashboard permalinks and the address key.** `machine.web.public_url` ([ORB-28914], proposed);
   `GET /t/<id>`, both forms of `GET /r/<id>` and every redirect naming the host
   ([ORB-28915], proposed).
3. **Replace the mod** ([ORB-28916], proposed, blocked by phase 2). Move the linking function and its cases into
   `plugin/hooks/mod`, add the run pattern, read the prefixes and the address at session
   start, delete the rest, update the docs.
4. **A result's `url` and the skill line** ([ORB-28917], proposed, blocked by the trial and phase 2).
5. **Messages from other sessions in Claude Code, a trial first** ([ORB-28918], proposed, blocked by phase 3). A
   peer's message, a task notification and hook feedback arrive as user rows, and their
   ids stay plain. The engine documents an assistant message's text as markdown and a
   user row's as printable text. The trial: rewrite a user row's text with a markdown
   link, and separately draw the row from the hook's own tree with link elements, on
   terminal and desktop. Whichever keeps the engine's row and gives a working link
   ships; if neither does, this phase is dropped.

## Open questions

1. **Should the skill line apply in Claude Code too?** There the mod already links ids for
   nothing, and the model's link would add only the exact run address. Proposed: Codex
   only, unless a wrong run is opened in practice.
2. **An id inside code.** A desktop context-menu row ("Open ORB-123 in Orbit") could cover
   ids in code blocks and tool output without touching the text. It is left out until
   someone misses it.

## Acceptance (feature level)

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

## Task References

- [ORB-13862]: built the mod this scope replaces.
- [ORB-14448]: built the host registry the prefixes are read from.
- [ORB-28913]: ran the Codex Desktop trial, which passed.
- [ORB-28914]: adds the `machine.web.public_url` setting.
- [ORB-28915]: adds the task and run permalinks.
- [ORB-28916]: replaces the mod with task and run links.
- [ORB-28917]: returns exact URLs in results and adds the Codex skill line.
- [ORB-28918]: trials links in messages from other sessions in Claude Code.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
