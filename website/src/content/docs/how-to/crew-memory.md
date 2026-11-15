---
title: Give models a memory
description: "Turn on crew memory, deliver it through an experiment arm, write and review notes, and grant agents read access."
---

Crew memory gives each model a small space of Markdown notes, plus one
`global` space every model shares. Orbit puts those notes in the model's
prompt, so lessons from one week's work reach the next dispatch. Every write
appends a revision; earlier revisions stay readable.

Crew memory is experimental and off by default.
[CLI commands: Crew memory](/reference/cli/#crew-memory-experimental) lists
every `orbit memory` verb; `orbit memory <verb> --help` has its flags.

## Turn it on

```bash
orbit config set crew_memory.enabled true
```

While [`crew_memory.enabled`](/reference/config/#settable-keys) is `false`,
every memory write and proposal command is refused, `orbit experiment start`
refuses an experiment with a memory arm, and running memory arms deliver
nothing. `orbit memory show`, `history` and `export` still work.

Turning it on delivers nothing by itself. Memory reaches a prompt only through
the memory arm of a running `live` experiment.

## Create a memory-arm experiment

A memory arm is an experiment arm whose overlay has a `memory` field:

```json
{"memory": {"roles": ["implement"], "global": true}}
```

| Field | Values | Default |
|---|---|---|
| `roles` | Any of `implement`, `review`, `recovery` | `["implement"]` |
| `global` | `true` also delivers the shared `global` space | `false` |

Experiments must be enabled too
(`orbit config set experiments.enabled true`). Then create and start the
experiment like any other:

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

Start in `shadow`. The mode is frozen once the experiment starts, so to go
`live`, clone the experiment and set `--mode live` on the new draft (see
[Run an experiment](/how-to/experiments/)).

| Mode | What happens |
|---|---|
| `shadow` | Records the memory each task would receive, but delivers nothing. Use it to check the population, the spaces and the readout before any prompt changes. |
| `live` | Delivers the notes as a `crew_memory` field in the agent envelope, with one fixed sentence telling the agent that the notes are data and that the task, its criteria, the instructions and the repository's guidance take precedence. |

A task receives the newest revision of its drawn crew's model space. A model
with no revision gets an empty block, which is still delivered and recorded. A
delivered block holds at most 9 KiB of note text.

Only one experiment may own the `memory` surface for a task. A second
experiment that would enroll the same task on it excludes that task with a
surface conflict; see
[Arms and overlays](/how-to/experiments/#arms-and-overlays).

## Write memory

Two writers exist: you, from the terminal, and the weekly
[dreaming routine](#dreaming-the-weekly-reflection).

```bash
orbit memory show [<key>|global] [--revision N]
orbit memory history <key>
orbit memory write <key> --file PATH… [--evidence ID…] [--reason TEXT]
orbit memory reset <key> [--reason TEXT]
orbit memory copy <from> <to> [--reason TEXT]
orbit memory export [--dir DIR]
```

`<key>` is a model id, or `global`.

| Command | Effect |
|---|---|
| `write` | Replaces the whole space with the given files, as a new revision. |
| `reset` | Empties the space, as a new revision. |
| `copy` | Copies one model's space into another's, for a model upgrade. |
| `export` | Writes every current space to `global.md` and `<model>/<file>.md` (default `.orbit/memory`). Orbit never reads these files back. |

### The write rules

A write that breaks a rule is refused whole, and the error names the rule:

| Rule | What it refuses |
|---|---|
| `caps` | A model space over 8 files or 6 KiB, `global` over 1 file or 3 KiB, or a file name outside `[a-z0-9-]{1,40}.md`. |
| `provenance` | Evidence that is not a task id. A write from a run must cite at least one evidence task id. |
| `score_blind` | Scorecard vocabulary, or another crew's or model's name. |
| `not_instructions` | A URL, an Orbit tool name, a shell command block or an injection phrase. Memory is data, not instructions. |
| `rewrite_whole` | A file named twice in one write. |

A refusal changes nothing. `orbit memory history <key>` lists it as a
`write_refused` event with the rule it broke.

## Dreaming: the weekly reflection

The shipped `crew-memory-dream` routine writes most memory. It runs Monday at
02:00 host-local time, on the owner checkout only, and does nothing unless
`crew_memory.enabled` is true and a running `shadow` or `live` experiment has
a memory arm.

For each model with at least 10 implementer units in the past 7 days, it
files one `low` chore, "Crew memory: reflect on \<model\>'s week", for that
model's base crew. The task carries an evidence pack,
`crew-memory-evidence.json`: the week's problem tasks in plain descriptions,
with no scores or ranks. A model that already has an open dreaming task, or
had no problem task that week, is skipped.

Only that task's run may write memory, and only within these limits:

- It writes its own model's key and nothing else.
- It must cite evidence from its pack.
- It cannot write `global`. It may raise at most two global proposals.

A dreaming task that ends without a revision is recorded as `no_revision`.

To stop dreaming without turning crew memory off, find the routine's exact
name with `orbit routine list` and run `orbit routine disable <name>`. See
[Schedule recurring work](/how-to/recurring-work/).

## Review global proposals

A dreaming run proposes notes for `global`; you decide them:

```bash
orbit memory proposals
orbit memory proposal accept <id> [--edit FILE]
orbit memory proposal reject <id> --reason <TEXT>
```

| Command | Effect |
|---|---|
| `proposals` | Lists open proposals with their evidence and expiry. |
| `accept` | Appends the note to the global text as a new revision (3 KiB maximum). |
| `accept --edit FILE` | Replaces the whole global text with that UTF-8 file, so you can drop older notes. |
| `reject` | Records the reason and keeps the proposal's text and evidence. |

An accepted result must pass the write rules for `global`. An open proposal
lapses after four weeks: it becomes `withdrawn`, with a `proposal_expired`
event in `orbit memory history global`.

## Grant an agent read access

Which memory tools an agent can call depends on its run:

| Run | Tools it lists |
|---|---|
| A dreaming task's run | `orbit.memory.write` and `orbit.memory.propose` for its own model's key, plus `orbit.memory.show` for its own space and `global`. |
| A run in a memory arm | `orbit.memory.show` for its own model's space and `global`. |
| A run whose task you granted | `orbit.memory.show`, `orbit.memory.history`, or both, as named in the grant. |

Set the grant on one task, or on an auto-task definition, which copies it to
every task it files. Only an operator can:

```bash
orbit task update <id> --granted-tools orbit.memory.show,orbit.memory.history
orbit auto-task update <name> --granted-tools orbit.memory.show,orbit.memory.history
```

Each tool needs its own entry. `orbit.memory.show` reads any space at any
revision and the all-spaces summary; `orbit.memory.history` reads any space's
history. The grant is read-only: writing and proposing stay with dreaming
runs, and `reset`, `copy`, `export` and proposal review stay with you.

A granted task runs only on the owner host, because memory lives in the
owner's store. For grants in general, see
[Task tool grants](/concepts/tasks/#task-tool-grants).

## Read the result

Read a space back with `orbit memory show` and `orbit memory history`, or in
the dashboard's read-only
[Knowledge › Memory view](/how-to/dashboard/#knowledge--memory).

In the [experiment readout](/how-to/experiments/#read-the-readout), a memory
arm adds these fields:

| Field | Arm | Meaning |
|---|---|---|
| `memory_strata` | live | The arm's tasks grouped by the ISO week of the revision their first dispatch received (`empty` for an empty block). A new revision is a new stratum, not drift. |
| `memory_captures` | shadow | Per task, what would have been delivered: the captured digest, the model and global revisions and the renderer version. |
| `memory_capture_strata` | shadow | Those captured digests grouped by revision week. |
| `dreaming_overhead` | any memory arm | The dreaming tasks' cost spread over the arm's priced tasks, so the arm's price includes keeping its memory current. |
