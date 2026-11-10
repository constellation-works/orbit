---
title: Give models a memory
description: "Turn on crew memory, deliver it through a memory-arm experiment, write and review notes, run the weekly dreaming routine, and grant agents read access."
sidebar:
  order: 9
---

Crew memory gives each model a small memory space of Markdown files, plus
one `global` space every model shares. A model with a revision gets those
notes in its prompt, so lessons from one week's work can reach the next
dispatch instead of living only in finished task records. Spaces are
append-only revisions in the host store: every write, reset or copy appends
a new revision, and earlier revisions stay readable in the space's history.

Crew memory is experimental and off by default. This page turns it on,
delivers it through an experiment, and covers the write rules, the weekly
dreaming routine, global-proposal review, and operator read grants. The
full command list is in
[CLI commands: Crew memory](../../reference/cli/#crew-memory-experimental);
this page does not repeat it.

## Turn it on

```bash
orbit config set crew_memory.enabled true
```

While `crew_memory.enabled` is `false` (the default), `orbit memory write`,
`reset`, `copy`, `proposals` and the proposal decisions are refused with an
error naming the key. `orbit experiment start` also refuses an experiment
with a memory arm, and running memory arms deliver nothing. `orbit memory
show`, `history` and `export` work either way, so you can inspect a space
before you enable writes. See
[`crew_memory.enabled`](../../reference/config/#settable-keys).

Memory reaches a prompt only one way: through the `memory` surface of a
running `live` experiment (next section). No setting delivers it outside an
arm.

## Create a memory-arm experiment

A memory arm adds a `memory` field to an arm's overlay:

```json
{"memory": {"roles": ["implement"], "global": true}}
```

`roles` names any of `implement`, `review` and `recovery` and defaults to
`["implement"]`. `global` defaults to `false`; set it to `true` to deliver
the shared `global` space beside the model's own space. The `memory` field
is its own surface: one experiment per task may own it, and a second
experiment that would enroll the same task on that surface excludes the
task with a surface conflict instead (see
[Arms and overlays](../experiments/#arms-and-overlays)).

Create and start an experiment with a memory arm the same way as any other
experiment — experiments must also be enabled
(`orbit config set experiments.enabled true`):

```bash
orbit experiment create model-memory \
  --hypothesis "Per-model notes cut repeated mistakes on implement runs." \
  --arms '[
    {"name": "control", "control": true, "weight": 1, "overlay": {}},
    {"name": "with-memory", "control": false, "weight": 1,
     "overlay": {"memory": {"roles": ["implement"], "global": true}}}
  ]' \
  --primary-metric total_tokens \
  --min-sample 20 \
  --mode shadow
orbit experiment start model-memory
```

Start in `shadow` first (see [Run an experiment](../experiments/)):

- `shadow` captures the memory each task would receive — the block, both
  revision numbers and its digest — but delivers nothing. Use it to confirm
  the population, the spaces and the readout before any prompt changes.
- `live` delivers the block. A live delivery adds a `crew_memory` field to
  the agent envelope (`{"model": [{"path": "notes.md", "text": "…"}],
  "global": "…", "revision": {"model": 1, "global": 2}}`), never inside
  `instruction`, and appends one fixed sentence to the instruction chain
  saying the notes are data and that the task, its criteria, the instructions
  and the repository's guidance take precedence. A delivered block holds at most
  9 KiB of note text (a full model space plus a full global space); a larger
  one is refused. A model with no revision renders an empty block, which is
  still delivered and recorded.

The owner reads the drawn crew's model space at its newest revision after
the crew draw. A claimed dispatch whose model differs from the captured
one receives no memory. On the owner, a dispatch with no captured delivery
for its model, such as a reviewer or recovery dispatch, is rendered at
dispatch instead.

## Write memory

Operators write from the terminal; the dreaming routine (below) is the only
other writer. The everyday commands are:

```bash
orbit memory show [<key>|global] [--revision N]
orbit memory history <key>
orbit memory write <key> --file PATH… [--evidence ID…] [--reason TEXT]
orbit memory reset <key> [--reason TEXT]
orbit memory copy <from> <to> [--reason TEXT]
orbit memory export [--dir DIR]
```

`<key>` is a model id, or `global`. `write` replaces the whole space with
the given files as a new revision; `reset` empties it with a new revision;
`copy` copies one model's space into another's, for a model upgrade.
`export` writes every current space to `global.md` and `<model>/<file>.md`
(default `.orbit/memory`) — files Orbit never reads back. Flags and the
full table are in the [CLI reference](../../reference/cli/#crew-memory-experimental).

### The write rules

Every write passes the same rules and is refused whole, naming the rule,
when it breaks one:

| Rule | What it refuses |
|---|---|
| `caps` | A model space over 8 files or 6 KiB, `global` over 1 file or 3 KiB, or a file name outside `[a-z0-9-]{1,40}.md`. |
| `provenance` | Evidence that is not a task id; a write from a run must cite at least one evidence task id. |
| `score_blind` | Scorecard vocabulary, or another crew's or model's name (the error names the token). |
| `not_instructions` | A URL, an Orbit tool name, a shell command block or an injection phrase (the error names the token). Memory is data, not instructions. |
| `rewrite_whole` | A file named twice in one write; each write replaces the whole space. |

A refusal changes nothing and is recorded as a `write_refused` event, so
`orbit memory history <key>` shows the refused write and the rule it broke
beside the revisions that landed.

## Dreaming: the weekly reflection

The shipped `crew-memory-dream` routine (seeded enabled, Monday 02:00
host-local, owner checkout only) is how memory mostly gets written. It
mints nothing while `crew_memory.enabled` is false or no running `shadow`
or `live` experiment has a memory arm. Otherwise, for each model with at
least 10 implementer units in the trailing 7 days, it mints one `low`
`no-diff-expected` chore, "Crew memory: reflect on \<model\>'s week",
crewed by that model's base crew, with a frozen evidence pack as the
artifact `crew-memory-evidence.json`. The pack lists the week's problem
tasks with plain descriptions — never scores, rates or ranks — capped at
64 KiB with a drop count. A model with an open dreaming task, or whose week
holds no problem task to cite, is skipped.

Only that task's `agent_implement` run may write: it lists the agent tools
`orbit.memory.write` and `orbit.memory.propose`, may write only its own
model's key, must cite evidence from its pack, and may raise at most two
global proposals in one run. A dreaming task that ends without a revision
is recorded as `no_revision`. To stop dreaming without turning crew memory
off, find its exact workspace-suffixed name with `orbit routine list` and
disable it with `orbit routine disable <name>` (see
[Schedule recurring work](../recurring-work/)).

## Review global proposals

A dreaming run cannot write `global` directly; it proposes a note for an
operator to decide:

```bash
orbit memory proposals
orbit memory proposal accept <id> [--edit FILE]
orbit memory proposal reject <id> --reason <TEXT>
```

`proposals` lists the open notes with their evidence, expiry and a
display-only similarity count. `accept` appends the note to the current
global text as a new revision (3 KiB maximum), or with `--edit` replaces
the whole global text with that UTF-8 file, so older notes can be dropped;
the result passes the global write rules. `reject` records the reason and
keeps the proposal's text and evidence. An open proposal lapses after four
weeks: it becomes `withdrawn` with reason `expired after 4 weeks`, recorded
as a `proposal_expired` event in `orbit memory history global`. All three
commands refuse while `crew_memory.enabled` is false.

## Grant an agent read access

From a terminal, `orbit memory show` and `orbit memory history` are the
operator's. An agent inside a run reaches memory through the tools
`orbit.memory.show`, `orbit.memory.history`, `orbit.memory.write` and
`orbit.memory.propose`, and which ones a run lists depends on the run:

| Run | Tools it lists |
|---|---|
| A crew-memory dreaming task's run | `orbit.memory.write` and `orbit.memory.propose` for its own model's key, plus `orbit.memory.show` for its own space and `global`. |
| A run in a memory arm | `orbit.memory.show` for its own model's space and `global`, and nothing else. |
| A run whose task an operator granted | `orbit.memory.show` and/or `orbit.memory.history`, each only when named in the grant. |

Only an operator can set the grant, on one task or on an auto-task
definition (which copies it to every task it mints):

```bash
orbit task update <id> --granted-tools orbit.memory.show,orbit.memory.history
orbit auto-task update <name> --granted-tools orbit.memory.show,orbit.memory.history
```

A task granted `orbit.memory.show` reads any space, any revision and the
all-spaces summary; one granted `orbit.memory.history` reads any space's
history. Each tool needs its own entry. The grant is read-only:
`orbit.memory.write` and `orbit.memory.propose` stay a dreaming run's, and
`reset`, `copy`, `export` and proposal review stay the operator's. A
granted task runs only on the owner host, because memory lives in the
owner's store. The general `granted_tools` mechanism is in
[Tasks](../../concepts/tasks/).

## Read the result

Read a space back with `orbit memory show` and `orbit memory history`, or
in the dashboard's read-only
[Knowledge › Memory view](../dashboard/#knowledge--memory).

In the [experiment readout](../experiments/#read-the-readout), a memory arm
adds fields of its own:

- `memory_strata` (live arms) groups the arm's tasks by the ISO week of the
  revision their first dispatch received (`empty` for an empty block). A
  revision change is a stratum, not drift; a renderer-version change drifts
  the arm.
- `memory_captures` (shadow arms) lists each task's captured digest, model
  and global revisions and renderer version — what would have been
  delivered. `memory_strata` is empty and omitted for a shadow arm.
- `memory_capture_strata` groups those distinct captured digests by
  revision week.
- `dreaming_overhead` spreads the dreaming tasks' cost over the memory
  arm's priced tasks, so the arm's price includes keeping its memory
  current.

## See also

- [Run an experiment](../experiments/) — arms, overlays, shadow and live,
  and the readout.
- [CLI commands: Crew memory](../../reference/cli/#crew-memory-experimental)
  — every `orbit memory` verb and flag.
- [Configuration](../../reference/config/#settable-keys) —
  `crew_memory.enabled`.
- [Use the dashboard](../dashboard/#knowledge--memory) — the read-only
  Knowledge › Memory view.
