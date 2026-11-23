# Configuration: crews

## Contents

- [Crews](#crews)
  - [Human crews](#human-crews)
  - [Crew names containing a colon](#crew-names-containing-a-colon)
- [Crew selection and actual execution](#crew-selection-and-actual-execution)

## Crews

A crew is one named provider-and-model assignment. A task's `crew` field selects
its executor; `workflow.default_crew` covers the rest.

Optional `effort` tunes the existing provider-mapped reasoning vocabulary.
Any crew reference (pools, `default_crew`, task `crew`, `--crew`) may
override it as `<crew>@<effort>`: the reference wins over the crew's `effort`, is a
distinct pool member and scorecard row, shares the crew's account, and an effort the
provider rejects is refused at load (never ignored). Crew names may not contain `@`.

Each launch records the effective value after crew/inline precedence and
admission, alongside its resolved crew and model. Run inspection with
`orbit run show --json` or the operator-only `orbit.run` action `show` MCP tool
exposes `agent_executions` on the run and displayed steps:
`reasoning_effort: null` means provider default; `"unknown"` means historical
execution without a record. Editing a crew never rewrites history.

Scorecards, experiment arms and managed usage include effort
breakdowns; inspect those populations when tuning a crew. Scorecard rankings
retain their crew/model score, with effort shown as descriptive execution usage.

### Human crews

`kind = "human"` declares a crew for operator work: tasks a person does
themselves, tracked beside agent work but never dispatched. `kind` defaults
to `"agent"`; any other value fails load.

```toml
[crews.operator]
kind = "human"
description = "Operator's own work"
tags = ["ops"]
```

**Supported keys and restrictions.** A human crew takes only `description`
and `tags` besides `kind`. Setting an agent key (`provider`, `model`,
`effort`, `env`, `vision`, `enabled`, or `backend`) fails load naming the
crew. Its name must not match an agent crew,
built-in ones included. Human crews are kept apart from the agent crews, so
a `[crews]` table holding only human crews still gets the built-in agent
crews. Names normalize to lowercase letters, digits, `_` and `-`.

**Seeding and adding another.** When the global config has no human crew,
`orbit init` adds one named after the OS user. At a terminal it offers the
OS user as the default; without a terminal, or with `--non-interactive`, it
takes the OS user as-is; `--human-name <NAME>` chooses another name. A name
an agent crew already holds is asked again at a terminal and refused
otherwise, before init writes anything. A re-run keeps an existing human
crew unchanged and never adds a second one.

To track another operator's
work, hand-author a second `[crews.<name>]` table the same way in the
global config.

**Never drawn.** `workflow.default_crew`, the complexity, type and purpose
pools, `purpose_crews.review`, and experiment arm overlays may not name a
human crew. Config load refuses such a reference, and starting an
experiment refuses an arm naming one. A redraw never selects a task onto a
human crew and never moves one there.

**Never admitted.** A drain or pull follower skips a human-crew task even
when `--allow-crew` names the crew. `--allow-crew` accepts a human crew's
name but admits nothing. An explicit ship of the task admits nothing, and
the dashboard shows no Ship control for it.

**Never piloted.** The task pilot excludes the task (reason `human_crew`),
refuses an explicit request for it (`task_pilot_human_crew`), and records
no assessment, warning or hold on it. A task-pilot run naming one still
starts: that crew is not the run's agent crew, preparation leaves the task
out, and the run keeps an agent crew. An approve-proposed drain does not
select the task.

**Closed by the operator.** The operator's own
`orbit task update <id> --status <status>` or the dashboard status control
(which offers every status for such a task) moves a human-crew task from
any status to any other, for example proposed to done, with no plan,
review, completion evidence or `--force`. A note stays optional. Each move
is recorded as a `forced` status entry with the actor, the from and to
statuses and a note naming the human crew (`human-crew bypass`).

Only the
operator's CLI and dashboard get this; tool and MCP updates keep the normal
lifecycle on the same task, so assigning a human crew never lets an agent
skip review. A non-terminal local run that owns the task still refuses the
move, and leaving `done` or `archived` still needs `--force`.

**Out of agent analytics.** Crew analytics, performance rankings and
provider usage leave human crews out. The dashboard tags each human-crew
task `for <name>`.

**Verify with config and readiness.** A reference that fails load names the
crew and the offending key before anything dispatches:

```bash
orbit config show
orbit config get crews.operator.kind
orbit config get crews.operator.description
orbit run readiness --json
```

`orbit config show` must exit 0 and list the human crew with provider
`human`; `orbit config get crews.operator.kind` answers `human` (for an
agent crew, `kind` is absent). `orbit run readiness` reports the task with
reason `human_crew` and its crew. After an operator move, the task history
holds the `forced` entry naming the human-crew bypass.

### Crew names containing a colon

The pool grammar reserves `:` for weights: `workflow.*_complexity_crews` and
`workflow.purpose_crews.*` entries are written `crew[@effort]` or `crew[@effort]:weight`. Orbit
therefore refuses any crew whose name contains a colon, both where it is
defined and in `orbit config set crews.<name>.<field>`. It checks each config
file on its own as it loads, before the layers merge.

A `config.toml` that
already defines such a crew, such as `[crews."gpt-5:codex"]`, stops every
command that loads configuration, including `orbit config show`,
`orbit config set` and `orbit doctor`. The refusal names the defining file:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':'; ... The crew is defined in '~/.orbit/config.toml'; Orbit refuses to load that config, so `orbit config set` cannot repair it either. Edit '~/.orbit/config.toml' and rename or remove the [crews."gpt-5:codex"] table, then rerun the command
```

Two routes that look like repairs do not work:

- `orbit config set` loads the same files first, so it is refused too. Edit the
  file by hand.
- A workspace override does not help. The global file is still checked before
  the merge, so a valid `<workspace>/.orbit/config.toml` leaves the refusal in
  place. A workspace table with the same colon name is refused by the same check.

Repair it by editing only the file the refusal names:

1. Rename the table to a name without a colon, such as `[crews."gpt-5-codex"]`,
   or delete it if nothing uses it. Keep the crew's other fields, every other
   table, and any `env` entries unchanged; those entries hold variable names,
   not values, so leave them as they are.
2. Update every reference to the old name:
   - `workflow.default_crew`. When any crew is defined, Orbit requires this key
     to name one, and a value naming an undefined crew fails the load with
     `crew '<name>' is not defined in [crews.*]`.
   - Each entry of `workflow.*_complexity_crews`, `workflow.type_complexity_crews`
     and `workflow.purpose_crews.<purpose>`: replace the old name and keep any
     `:weight` suffix on that entry.
   - A task's crew, with `orbit task update <task-id> --crew <new-name>`.
     Changing it to an empty value draws a fresh crew instead.
3. Run `orbit config show`. It should exit 0 and list the renamed crew with its
   `ENABLED` state. Then run `orbit doctor`. Once configuration loads, a later
   row about a provider CLI, sandbox or permissions is a separate finding, not
   a symptom of the crew rename; for a missing CLI, follow
   [Provider setup](providers.md#provider-setup).

## Crew selection and actual execution

For ship dispatch, an explicit run crew overrides the task's explicit pin,
validated pool assignment, or `default` fallback. Otherwise the purpose, type,
complexity and default chain apply as described below.

A creation without a
crew draws in this order: its purpose pool, when its task carries a system tag
or `crew-purpose:<purpose>` tag naming a configured purpose pool with an enabled
member; else its type pool, `workflow.type_complexity_crews.<type>.<tier>`, when
its type and tier have one; else its complexity pool, falling back to
`default_crew`. It stores `crew_source` as `explicit`, `purpose:<purpose>`,
`type:<type>/<tier>`, `pool:<complexity>` or `default`, also recorded in
assignment history.

A complexity re-rate, or a tag or context-file edit that
changes the task's type pool, redraws a pool-, type- or default-sourced crew
and records the previous and new sources and crews in `crew_redrawn` history;
explicit and purpose-sourced crews stay pinned. A `default` fallback is not an
explicit pin. Status transitions alone preserve the choice.

Admission
revalidates pool and type-pool assignments and `default` fallbacks against the
current tier and enabled pool members, recovering legacy provenance from
assignment history when needed, without writing to the task. An empty crew
string on task update re-draws for the task's current complexity rather than
leaving the field empty.

Discover actual
crew names through the connected server's crew discovery when available, or
inspect effective configuration; executor names are not a list of crew names.

The task's `resolved_crew` and `crew_model`, when returned, describe the recorded
run rather than today's default. Unsupported/unavailable explicitly selected
providers fail rather than silently switching vendors. Provider login and model
availability must be checked on the execution host before dispatch.

For lane-specific authentication, sandbox grants and MCP versus shell access,
follow [Provider setup](providers.md#provider-setup). Shell tool access through `orbit tool run`
enforces the same allowlist and caller-role gates as MCP; disabling that shell
also removes Orbit tool reach for a shell-only lane.
