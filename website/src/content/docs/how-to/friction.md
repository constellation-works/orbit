---
title: Report and triage friction
description: "Record what made work harder than it should have been, then find, triage, resolve and rehome the record."
---

A friction record says what made the work harder than it should have been: a
confusing error, a missing flag, a stale instruction, a tool you had to work
around. It is a ledger entry in one workspace, not a task. Recording it lets the
cause be found, counted and fixed.

Your agent records friction when it hits it. The commands below do the same
from a terminal, and the dashboard's
[Knowledge view](/how-to/dashboard/#knowledge--frictions) shows the same ledger.

## Record it

```bash
orbit friction add \
  --title "Help text gives a 2h maximum the runtime does not enforce" \
  --body "What happened, what you expected, and what you did instead." \
  --tag docs,tooling \
  --model claude \
  --during-task <task-id>
```

| Option | Effect |
|---|---|
| `--body <BODY>` | Required. Markdown describing what happened and why it caused friction. |
| `--model <MODEL>` | Required. The agent family the record counts toward: `codex`, `claude`, `gemini`, `grok` or `muse`. |
| `--title <TITLE>` | One-line handle, at most 120 characters. Derived from the body when omitted. |
| `--tag <TAGS>` | Taxonomy tag. Repeat the flag or comma-separate several. See [Taxonomy](#taxonomy). |
| `--during-task <TASK>` | The task you were working on. An ID that names no task is refused. |

The record starts as `open`. The command prints its ID, in the form
`FYYYY-MM-NNN`, which `show`, `update`, `resolve` and `rehome` take.

The record lands in the active workspace. Select another with the global
`--workspace <SELECTOR>` or `--root <DIR>`.

`--during-task` is context only. The dashboard links the record to the task, and
`orbit friction list --q <task-id>` and `orbit search` find the record by it.
Finishing the task does not close the record: resolve it yourself once the fix
lands.

What to record:

- A command, tool, error or policy rule cost you time, and the next person or
  agent would hit the same thing.
- Documentation, help text or a skill instruction was missing, stale or wrong.
- The same annoyance happened again. Each hit counts toward the rate.

What not to record:

- The work itself. A bug to fix or a feature to build is a task. Link the
  friction to it with `--during-task`.
- Secrets, tokens or private output. The body is stored as Markdown in the
  workspace. Name the credential or file, not its value.

Record one cause per record, when it happens, while the evidence is in front of
you.

## Taxonomy

Tags say what kind of friction it was, so `stats` can split rates by kind. List
the tags the active workspace accepts:

```bash
orbit friction tags
```

The shipped defaults:

| Tag | Covers |
|---|---|
| `automation` | Scheduler, auto-task or delivery-automation friction. |
| `build` | Make, format, lint, CI or test friction. |
| `docs` | Stale or missing documentation. |
| `history-diverged` | A rewritten branch history orphaned recorded automation state. |
| `lifecycle` | Task lifecycle confusion or transition issues. |
| `naming` | Naming drift or duplicated sources of truth. |
| `other` | Fallback when nothing else fits. |
| `policy` | Filesystem-profile or sandboxing surprises. |
| `skill-guidance` | Misleading or incorrect skill instructions. |
| `tooling` | Tool, CLI or MCP failures. |

A workspace can extend this list. A tag the workspace does not configure is
refused. These spellings are accepted and stored as the canonical tag:

| You pass | Stored as |
|---|---|
| `ci`, `test`, `tests`, `testing` | `build` |
| `cli`, `mcp`, `tool` | `tooling` |
| `auto-task`, `auto-tasks`, `scheduler`, `routine` | `automation` |
| `skill`, `prompt` | `skill-guidance` |

With `--json`, the command reports each substitution under `tag_normalizations`.

## Find records

```bash
orbit friction list
orbit friction show <friction-id>
```

`list` shows `open` and `triaged` records, newest first. Narrow it:

| Option | Effect |
|---|---|
| `--status <STATUS>` | `open`, `triaged` or `resolved`. Comma-separate several, or pass `all`. |
| `--model <MODEL>` | Only this agent family. |
| `--tag <TAG>` | Only records carrying this tag. |
| `--q <QUERY>` | Case-insensitive match over ID, model, tags, status, task and body. |
| `--month <YYYY-MM>` | Only records reported in that month. |
| `--from <RFC3339>` / `--to <RFC3339>` | Bounds on when records were reported. |
| `--limit <N>` / `--offset <N>` | Page through the result. |
| `--order oldest\|newest` | Creation-time order. Default `newest`. |

```bash
orbit friction list --status open --tag tooling
orbit friction list --status all --q "reservation" --order oldest
orbit friction list --month 2026-10 --model claude --json
```

For a script, add `--json`, `--format plain` or `--format ndjson` to any
`orbit friction` command.

`orbit search` covers frictions together with tasks, and can reach other
workspaces. It matches frictions by case-insensitive substring and shows `open`
ones by default:

```bash
orbit search "reservation limit" --kind friction
orbit search "sandbox" --kind friction --all               # add triaged and resolved
orbit search "sandbox" --kind friction --status friction:resolved
orbit search "sandbox" --kind friction --all-workspaces    # every workspace on this machine
```

## Triage: open, triaged, resolved

| Status | Meaning |
|---|---|
| `open` | Reported, not yet looked at. Every record starts here. |
| `triaged` | Read and classified: the tags are right, the cause is understood, and someone has decided whether it earns a task. |
| `resolved` | The cause is fixed, or the record no longer applies. |

```bash
orbit friction update <friction-id> --status triaged
orbit friction update <friction-id> --status triaged --tag tooling,docs
orbit friction update <friction-id> --title "Shorter handle" --body "Updated evidence."
orbit friction resolve <friction-id>
```

`update` changes only the fields you pass:

| Option | Effect |
|---|---|
| `--status <STATUS>` | `open`, `triaged` or `resolved`. |
| `--tag <TAGS>` | Replaces the stored tag set. Repeat or comma-separate. |
| `--body <BODY>` | Replaces the Markdown body. |
| `--title <TITLE>` | Replaces the title, at most 120 characters. An empty value restores the derived title. |
| `--rehome-to <WORKSPACE>` | Moves the record to the workspace that owns it. See [Rehome](#rehome-a-record-filed-in-the-wrong-workspace). |

`resolve` is the shortcut for `--status resolved` and takes no other options.
There is no delete. Resolve a record filed by mistake and update its body to say
why.

## Read the rates

```bash
orbit friction stats
orbit friction stats --json
```

`stats` covers the active workspace and prints:

- totals: `total`, `open`, `triaged`, `resolved` and `resolved_this_month`
- `by_family`: per agent family, its friction count, `tasks_done` and
  `frictions_per_10_tasks`
- `by_tag`: the same rates split by tag within each family

A rate is frictions per 10 done tasks. A high rate on a small `tasks_done` is a
small sample, so read the counts beside it.

## Rehome a record filed in the wrong workspace

A record belongs to the workspace whose work caused it. Move one that landed
elsewhere:

```bash
orbit friction rehome <friction-id> --to-workspace <workspace>
```

`<workspace>` is a registered name or `ws_*` ID on this machine. The owning
workspace gets a copy under a new ID, with the original title, reporter,
creation time, task, status and body. The original is resolved with a pointer
to the copy, and the command returns the copy as `rehomed_as`. Tags the target
workspace does not configure are dropped and named in the result.

You can do the same while you triage:

```bash
orbit friction update <friction-id> --rehome-to <workspace> --status triaged
```

When the owning workspace is not registered on this machine, record where the
record belongs without moving it. Clear that later with an empty value:

```bash
orbit friction update <friction-id> --rehome-to <workspace> --move false
orbit friction update <friction-id> --rehome-to ""
```

## Related

- [Use the dashboard](/how-to/dashboard/#knowledge--frictions): search, triage
  and resolve the same records in the browser.
- [CLI commands](/reference/cli/): every `orbit friction` verb.
