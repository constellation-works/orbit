---
title: Report and triage friction
description: "Record what made work harder than it should have been with orbit friction, then search, triage, resolve, and rehome the record in the right workspace."
sidebar:
  order: 7
---

A friction record says what made the work harder than it should have been:
a confusing error, a missing flag, a stale instruction, or a tool that failed
in a way you had to work around. It is a ledger entry in one workspace, not a
task. File the work itself as a task; record the friction so the cause can be
found, counted, and fixed instead of being worked around again in silence.

Your agent records friction for you when it hits it. The commands below are
the same surface from a terminal, and the
[dashboard's Knowledge view](../dashboard/#knowledge--frictions) is the same
ledger in the browser.

## When to record

Record friction when:

- a command, tool, or error cost you time and the next person or agent would
  hit the same thing;
- documentation, help text, or a skill instruction was missing, stale, or
  wrong, and you had to derive the real behavior;
- a policy, sandbox, or lifecycle rule surprised you and the surprise was not
  documented where you looked;
- the same annoyance has happened before. A second record is evidence, not
  noise: rates only show up when each hit is counted.

Do not record:

- the work itself. A bug to fix or a feature to build is a task. Link the
  friction to that task instead (see [Task linkage](#task-linkage));
- a one-off failure you cannot describe. Write what happened, what you
  expected, and what you did instead, or wait until you can;
- secrets, tokens, or private output. The body is stored Markdown in the
  workspace. Name the credential or file; do not paste its value.

Record it when it happens. One record per cause, with the evidence still in
front of you, beats one summary at the end of the day.

## Record it

```bash
orbit friction add \
  --body "What happened, what you expected, and what you did instead." \
  --model claude
```

| Option | Effect |
|---|---|
| `--body <BODY>` | Required. Markdown describing what happened and why it caused friction. |
| `--model <MODEL>` | Required. The agent family to attribute the record to: `codex`, `claude`, `gemini`, `grok`, or `muse`. |
| `--title <TITLE>` | One-line handle, at most 120 characters. When you omit it, Orbit derives one from the body. |
| `--tag <TAGS>` | Taxonomy tag. Repeat the flag or comma-separate several. See [Taxonomy](#taxonomy). |
| `--during-task <TASK>` | The task you were working on. An ID that names no task is refused. See [Task linkage](#task-linkage). |

```bash
orbit friction add \
  --title "Help text gives a 2h maximum the runtime does not enforce" \
  --body "The --help output and the enforced limit disagree, so the published maximum is wrong for values between them. Evidence: the two values, where each is printed, and a command that shows which one wins." \
  --tag docs,tooling \
  --model claude \
  --during-task <task-id>
```

The new record starts as `open`. The command prints its ID, in the form
`FYYYY-MM-NNN`. Keep that ID: `show`, `update`, `resolve`, and `rehome` all
take it.

Attribution is by agent family only. There is no free-form agent name: the
older `agent` field is retired, and naming a person or a session does not
change the rate the record counts toward.

## Taxonomy

Tags say what kind of friction it was, so `stats` can split rates by kind.
List the tags configured for the active workspace before you guess:

```bash
orbit friction tags
```

The shipped defaults are:

| Tag | Covers |
|---|---|
| `automation` | Scheduler, auto-task, or delivery-automation friction. |
| `build` | Make, format, lint, CI, or test friction. |
| `docs` | Stale or missing documentation or design docs. |
| `history-diverged` | A rewritten branch history orphaned recorded automation state. |
| `lifecycle` | Task lifecycle confusion or transition issues. |
| `naming` | Naming drift or duplicated sources of truth. |
| `other` | Fallback when nothing else fits. |
| `policy` | Filesystem-profile or sandboxing surprises. |
| `skill-guidance` | Misleading or incorrect skill instructions. |
| `tooling` | Tool, CLI, or MCP failures. |

A workspace can extend this list, so `orbit friction tags` is the authority
for the workspace you are in, not this table. A few familiar spellings are
accepted and stored as the canonical tag: `ci`, `test`, `tests`, and
`testing` become `build`; `cli`, `mcp`, and `tool` become `tooling`;
`auto-task`, `auto-tasks`, `scheduler`, and `routine` become `automation`;
`skill` and `prompt` become `skill-guidance`. The command reports any such
substitution. A tag the workspace does not configure is refused.

## Model and task context

Every record carries three pieces of context beside its body:

- **Model.** The agent family from `--model`. Rates in `stats` are per
  family, so attribute the record to the family that hit the friction.
- **Workspace.** The record lives in the active workspace. Select another
  with the global `--workspace <SELECTOR>` (a registered name, `ws_*` ID,
  or checkout path) or `--root <DIR>`. A record filed in the wrong
  workspace is not lost; [rehome](#rehome-a-record-filed-in-the-wrong-workspace)
  it.
- **Task.** The optional `--during-task` link described next.

## Task linkage

`--during-task <TASK>` records which task you were working on when the
friction occurred:

```bash
orbit friction add \
  --body "The reservation help and the enforced limit disagree." \
  --model claude \
  --tag docs \
  --during-task <task-id>
```

The link is context, in both directions:

- The dashboard shows `during <task-id>` on the record and links the detail
  panel to that task.
- `orbit friction list --q <task-id>` and `orbit search` find the record by
  the task it names.

The link changes nothing else. A friction record is not a task status, and
finishing the task does not close the record. When a task does fix the
cause, resolve the friction yourself once the fix lands, so the ledger says
when the cause stopped costing time:

```bash
orbit friction resolve <friction-id>
```

## Find records

```bash
orbit friction list
orbit friction show <friction-id>
```

`list` shows `open` and `triaged` records, newest first. Narrow it:

| Option | Effect |
|---|---|
| `--status <STATUS>` | `open`, `triaged`, or `resolved`; comma-separate several, or `all` for every status. |
| `--model <MODEL>` | Only this agent family. |
| `--tag <TAG>` | Only records carrying this tag. |
| `--q <QUERY>` | Case-insensitive match over ID, model, tags, status, task, and body. |
| `--month <YYYY-MM>` | Only records reported in that month. |
| `--from <RFC3339>` / `--to <RFC3339>` | Bounds on when records were reported. |
| `--limit <N>` / `--offset <N>` | Page through the result. |
| `--order oldest\|newest` | Creation-time order. Default `newest`. |

```bash
orbit friction list --status open --tag tooling
orbit friction list --status all --q "reservation" --order oldest
orbit friction list --month 2026-10 --model claude --json
```

Add `--json`, `--format plain`, or `--format ndjson` to any `orbit friction`
command when a script reads the output. On a terminal the default is a
table; piped, it is plain text.

`orbit search` covers the same ledger together with tasks. Friction matches
a case-insensitive substring, and `search` shows `open` frictions by
default:

```bash
orbit search "reservation limit" --kind friction
orbit search "sandbox" --kind friction --all              # add triaged and resolved
orbit search "sandbox" --kind friction --status friction:resolved
orbit search "sandbox" --kind friction --all-workspaces    # every workspace on this machine
```

Use `list` to work one workspace's queue with exact filters. Use `search`
when you do not yet know whether the record is a task or a friction, or
which workspace holds it.

## Triage: open, triaged, resolved

Every record moves through the same three states:

1. **`open`** — reported, not yet looked at. Every new record starts here.
2. **`triaged`** — looked at and classified: the tags are right, the cause
   is understood, and someone has decided whether it earns a task. Triaging
   changes no behavior; it says the record has been read.
3. **`resolved`** — the cause is fixed, or the record no longer applies.
   Resolve only when you can say what changed. A record resolved because
   its fix task landed should say so in that task's summary or in an
   updated body.

Triage a record with `update`. Pass only the fields you change:

```bash
orbit friction update <friction-id> --status triaged
orbit friction update <friction-id> --status triaged --tag tooling,docs
orbit friction update <friction-id> --title "Shorter handle" --body "Updated evidence."
orbit friction resolve <friction-id>
```

| Option | Effect |
|---|---|
| `--status <STATUS>` | `open`, `triaged`, or `resolved`. |
| `--tag <TAGS>` | Replacement tag set. Repeat or comma-separate; this replaces the stored tags, it does not add to them. |
| `--body <BODY>` | Replacement Markdown body. |
| `--title <TITLE>` | Replacement title, at most 120 characters. An empty value restores the derived title. |
| `--rehome-to <WORKSPACE>` | Move the record to the registered workspace that owns it. See [Rehome](#rehome-a-record-filed-in-the-wrong-workspace). |

`resolve` is the shortcut for `--status resolved`. It takes no other
options. There is no delete: a record you filed by mistake is resolved,
with its body updated to say why, so the count stays honest.

## Read the rates

```bash
orbit friction stats
orbit friction stats --json
```

`stats` reads the active workspace's friction and task stores and prints:

- totals: `total`, `open`, `triaged`, `resolved`, and
  `resolved_this_month`;
- `by_family`: per agent family, its friction count, its `tasks_done`, and
  `frictions_per_10_tasks`;
- `by_tag`: the same rates split by tag within each family.

A rate is frictions per 10 done tasks, so families and tags with more work
behind them compare fairly against quieter ones. A high rate on a small
`tasks_done` is a small sample; read the counts beside the rate before you
act on it. These numbers cover one workspace. Run the command with
`--workspace <SELECTOR>` for another.

## Rehome a record filed in the wrong workspace

A record belongs to the workspace whose work caused it. When one lands in
the wrong workspace, move it instead of copying it by hand:

```bash
orbit friction rehome <friction-id> --to-workspace <workspace>
```

`<workspace>` is a registered name or `ws_*` ID on this machine. The owning
workspace gets a copy with the original title, reporter, creation time,
task, status, and body, under a new ID. This record is resolved with a
pointer to the copy, and the command returns the copy as `rehomed_as`. Tags
the target workspace does not configure are dropped and named in the
result.

The same move is available while you triage:

```bash
orbit friction update <friction-id> --rehome-to <workspace>
orbit friction update <friction-id> --rehome-to <workspace> --status triaged
```

When the owning workspace is not registered on this machine, record the
disposition without moving anything, and clear it later with an empty
value:

```bash
orbit friction update <friction-id> --rehome-to <workspace> --move false
orbit friction update <friction-id> --rehome-to ""
```

`--move` applies only with a non-empty `--rehome-to`. Rehoming does not
merge workspaces and does not move tasks; it moves one friction record and
leaves the resolved pointer behind so old references still resolve.

## In the dashboard

**Knowledge → Frictions** shows the same queue for the selected workspace:
counts of `open` and `triaged` records and how many were resolved this
month, a search box, a **Show** filter (`Active`, `Open`, `Triaged`,
`Resolved`, `All`), and a detail panel where you set the status, edit the
tag set, and resolve the record. See
[Use the dashboard](../dashboard/#knowledge--frictions).

## Related

- [Use the dashboard](../dashboard/#knowledge--frictions): triage the same
  records in the browser.
- [CLI commands](../../reference/cli/): every `orbit friction` verb in one
  table.
- [Concepts](../../concepts/): where the friction ledger sits among
  Orbit's layers.
