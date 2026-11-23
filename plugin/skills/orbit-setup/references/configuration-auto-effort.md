# Configuration: automatic effort selection

## Contents

- [Automatic effort selection: `<crew>@auto`](#automatic-effort-selection-crewauto)

## Automatic effort selection: `<crew>@auto`

A pool entry can leave the effort to each draw instead of fixing it:
`<crew>@auto`, or `<crew>@auto:100` in a weighted pool. At admission the draw
resolves the entry through the crew's ordered effort bands and runs at the
resolved reference. The run's crew identity is that resolved reference:
`<crew>@<effort>`, or the bare crew when the crew's configured effort applies
and names none.

The resolution record keeps the pool entry:
`auto_effort.requested` is `<crew>@auto`, and the draw's source note still
contains `<crew>@auto` (see Verify below).

**Where `@auto` is allowed.** Only these pools accept it:

- the implementer complexity pools, `workflow.low_complexity_crews`,
  `workflow.medium_complexity_crews`, `workflow.hard_complexity_crews` and
  `workflow.xhard_complexity_crews`;
- the per-type implementer pools, `workflow.type_complexity_crews`;
- `workflow.purpose_crews.task_pilot`;
- the review pools, `workflow.purpose_crews.review` and
  `workflow.purpose_crews.pre_landing_review`.

Everywhere else it is refused: any other pool, `workflow.default_crew`, a
task's `crew` field, and `--crew` or other run-time crew overrides. The
refusal happens when configuration loads, on `orbit config set`, and on run
overrides, so an unsupported field never silently accepts it. A crew also
needs bands (below) before any pool may name it with `@auto`; a crew without
bands is refused the same way.

**Bands.** `workflow.auto_effort.bands` maps each crew — by its bare
`[crews.<name>]` name, never a `<crew>@<effort>` reference — to an ordered
list of bands. Bands default to empty (`{}`): no crew has bands, and every
`@auto` entry is refused, until an operator configures them. Each band sets
an `effort` the crew's provider and model accept, plus at least one
condition. The first band whose conditions the draw's signal all meets
chooses the effort; later bands are not consulted.

| Condition | Meets it when |
|---|---|
| `complexity = ["hard", "xhard"]` | The signal's tier is listed. Only `low`, `medium`, `hard` and `xhard` are valid; `unassessed` is refused. |
| `below_files = N` | The signal counts fewer than `N` files (`N` above 0). |
| `below_lines = N` | The signal counts fewer than `N` reviewable lines (`N` above 0). |

A band with no condition, a bound of 0, an unknown crew key, an empty band
list for a crew, or an effort the crew's provider and model reject all fail
configuration load with a message naming the band. To drop a crew's bands,
remove its key; an empty list is refused, not treated as removal.

Each role resolves from the best signal it has at admission:

| Role | Complexity signal | Size signal |
|---|---|---|
| Implementer | The task's assessed tier | `below_files` counts the distinct files the task's declared context files name (the pre-dispatch footprint). An implementer never has a line count, so `below_lines` never matches it. |
| Task pilot | The task's filed tier only | None; the pilot is what produces a size. |
| Reviewer | The reviewed tasks' highest settled tier | `below_lines` counts the candidate's reviewable lines and `below_files` its changed files. |

A condition on a signal the role does not have does not match. With no
signal at all, or when no band matches, the draw falls back to the crew's
own configured effort: the bare crew when its table sets no `effort`,
otherwise `<crew>@<effort>` from the table. A task keeps its `@auto` pool
member while the effort it resolves to moves; the next admission re-resolves
it against the task's current tier and footprint.

**Exploration.** A fixed share of `0.1` (10%) of seeded draws is dispatched at
another effort the crew can resolve to — one its bands name, or its
configured effort — so each side of a cutover keeps getting data. The roll
is seeded per draw (by the task for implementers, by the run and partition
for pilots, by the run and candidate head for reviewers), so a retried
selection resolves alike. A task's creation draw never explores, and a draw
with no subject to seed it never explores.

An explored draw records
`explored: true` together with the policy reference it replaced.

**Bounded pilot escalation.** Escalation applies only to a task pilot that
ran at an `@auto` resolution, and only once. When the pilot raises the task
above its filed tier to a tier whose band maps to a higher effort than it
ran at, or reports confidence `medium` or `low` (the target is then the
highest effort the crew's bands name), the assessment is held back and the
run re-pilots the task once at the higher effort; that second assessment is
the one applied.

A target at or below the effort the pilot ran at never
escalates (a crew whose table sets no effort counts as below every band
effort for that comparison), and the re-pilot never escalates again.

**Example.** Replace `example-crew` with a crew defined on the host, and
name that same crew with `@auto` in one of the supported pools:

```toml
[workflow.auto_effort]
bands.example-crew = [
  { complexity = ["hard", "xhard"], effort = "xhigh" },
  { below_files = 4, effort = "medium" },
]

[workflow]
medium_complexity_crews = ["example-crew@auto:100", "example-crew@medium:50"]
```

Order matters: a hard task with few files runs at `xhigh`, because the
complexity band comes first. Keep the fixed-effort member beside the `@auto`
member only when both shares are wanted; they are distinct pool members.

**Verify without dispatching production work.** Configuration alone proves
the policy is admitted, not that any draw has used it:

1. Read the policy back: `orbit config get workflow.auto_effort.bands` and
   `orbit config show --all`. The show must load without error and report
   each value with its source. A band against an undefined crew, or a band
   with no condition fails here, before any
   dispatch.
2. Confirm each `@auto` entry sits in a supported pool above and names a
   crew that has bands: `orbit config show` lists the pools. Do not test
   an unsupported field by setting it there; load and `orbit config set`
   refuse it by design.
3. Treat a live rollout as proven only by a recorded resolution. After a
   real draw, inspect it (`orbit run show --json` for a run, or the task's
   recorded crew selection). The implementer's crew selection stores the
   resolution record under `auto_effort`: `requested` (`<crew>@auto`),
   `resolved`, `policy`, `basis` (`band`, `fallback` or `escalation`), `band`,
   `explored` and `signal`. `band` is the zero-based index of the matching
   band and is present only for basis `band`.

   The draw's source note reads
   `auto effort <crew>@auto -> <resolved> (<basis>)`, where basis is `band N`
   (N is the one-based band), `configured effort` or `pilot escalation`. An
   explored draw reads `auto effort <crew>@auto -> <resolved> (explored;
   policy <policy> by <basis>)`. A pilot partition draw appends this note to
   its `source`, and an admitted reviewer appends it to its `crew_source`.
   Until such a record exists, report the feature as configured, not rolled
   out.


```toml
[crews.reviewer]
provider = "claude"
model = "opus"
description = "Deep review passes"
tags = ["review"]
```

Crews layer by name *and* field, so a workspace can override just the model of a
globally defined crew. A `default_crew` naming an undefined crew
fails config load — deliberately, since the alternative is silently dispatching
to the wrong model.

`enabled = false` on a crew keeps it defined and listed (`orbit config show`
has an `ENABLED` column; `orbit.workspace.list` with `include: ["crews"]`
returns `enabled` per crew) but takes it out
of execution. Omitting `enabled` means enabled, so older configs are unchanged.
A disabled crew is never drawn from a complexity pool; a pool whose members are
all disabled routes the task to `default_crew`, like an empty pool.

A task
`crew`, explicit crew or `default_crew` that
lands on a disabled crew refuses dispatch with an error naming the crew — no
substitution. Config still loads; `orbit doctor` warns when a lane key names a
disabled crew. Enable one with:

```bash
orbit config set crews.argon.enabled true
```

A crew can also opt out of image input with `vision = false`; see
[Executor image input](configuration-image-input.md#executor-image-input-image_input).

A crew can run on its own provider account through `env`, a list of variables
its agents receive on top of `execution.env.pass` (names only, never values):

```toml
# ~/.orbit/config.toml only
[crews.opus-credits]
provider = "claude"
model = "claude-opus-5-5"
env = ["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]   # or a bare "NAME"
```

`NAME=SOURCE` gives the agent `NAME` with the value of `SOURCE` from the
launching shell (or `~/.orbit/clock.env` for clock-started runs); a bare `NAME`
passes that variable to this crew only. `NAME=file:/abs/path` (or `file:~/path`)
reads the value from an owner-only file the host opens when it starts the agent,
so no shell, service or `clock.env` has to export it (a symlink, a file owned by
another user, a group- or world-readable, missing or empty file leaves the crew
unavailable).

The entry wins over `pass`, and setting
one Claude credential removes the other from that crew. `env` is global-only:
a workspace `config.toml` that sets it fails to load, and `orbit config set`
needs `--global`. With the source unset, drain, ship and job starts warn, the
drain leaves the crew out of its window, the crew's activities are refused
rather than run on the default login, and `orbit doctor`'s `env-pass` row
reports it.

A crew with `env` is its own account: its usage readings, failure
holds, pull-drain auth exclusions and probes are its own, and
`workflow.provider_limit_budgets` / `provider_limit_overrides` may name it
(`opus-credits:200usd/30d`).

`orbit init` writes every built-in crew (Claude: `opus`, `sonnet`, `haiku`, and `fable` pinned to `claude-fable-5-2`;
Codex: `astra`, `sol`, `terra`, `luna`; Antigravity: `gemini-flash`, `argon`, `neon`, `hydrogen`, `carbon`;
Grok: `grok`; Copilot: `copilot`; Muse: `muse-spark`, `muse-wonder`, `muse-wonder-full`;
no built-in Gemini CLI crew;

the `cursor`, `pi`, and `opencode` lanes were [retired](providers.md#provider-identity-and-resolution)), with `enabled = true` on the crews whose
agent CLI it detects and `enabled = false` on the rest, and points
`workflow.default_crew` at the preferred family's default (`opus` on a Claude
host, `gemini-flash` on an Antigravity host). Interactive init offers the enabled crews by name; `--non-interactive`
writes the recommendation.

Init does not write a `custom` or `system` crew
table, nor a system-crew key. The four
`workflow.*_complexity_crews` pools are seeded from the detected families
(Claude only: `haiku` / `sonnet` / `opus` / `fable`; Codex only: `luna` / `terra` /
`sol` / `astra`; both: `haiku, luna` / `terra, sonnet` / `sol, opus` / `fable, astra`;
grok joins `medium`; a detected `agy` appends `gemini-flash` to `low`, `hydrogen` to `medium`, `argon` and `neon` to `hard`, and `carbon` to `xhard`;

a detected `muse` appends `muse-spark`, `muse-wonder`, and `muse-wonder-full` to `low`, `medium`, and `hard`;
Copilot and other families leave them `[]`). A user-authored
legacy `qa` crew remains loadable, but init never creates it.

Init also writes the
`workflow.purpose_crews` pools as fixed lists filtered to the enabled crews: `review` from
`sol, sonnet, muse-wonder, grok, opus@medium`; `task_pilot` from `luna@high, haiku@high,
muse-spark@high`; `failure_recovery` from `luna, haiku, muse-spark, grok`; `auto_fix` and
`pre_landing_review` from `luna, haiku, muse-spark`; `final_recovery` from `sol, muse-wonder,
opus@medium`; `baseline_red_repair` from `terra, sonnet, muse-wonder, grok`.

A pool left empty
is omitted, except `final_recovery`, written as `[]` (off). Editing
`workflow.*_complexity_crews` later does not move those purpose pools; to move system
work, edit its `workflow.purpose_crews.<purpose>` pool directly. `sweep_tasks` and
`pilot_warning_triage` are not written: until an operator sets
`workflow.purpose_crews.pilot_warning_triage`, pilot-warning triage stays on the
hard-tier route and draws from `workflow.hard_complexity_crews`.

Creating this
machine's identity also sets `machine.auto_concurrency.mode = "enforce"` (an existing machine keeps
its mode), and the seed sets `experiments.enabled = true`,
`workflow.crew_selection = "static"`, `workflow.mid_run_steering.enabled = true`,
`workflow.task_pilot_freshness.material_fields = ["description", "criteria"]` and
`workflow.unlocked_context_paths = ["**/*.json", "**/*.txt", "**/*.md", "**/mod.rs", "**/*.toml"]`.

With no supported
agent CLI, every crew is written disabled and no default crew is set, so nothing
dispatches until you enable a crew. Which provider CLIs
this machine can launch, and each executor's sandbox mode:

```bash
orbit doctor providers
```

Executors are how a provider is invoked. You choose crews; executors are
infrastructure.

For provider IDs, dispatch precedence and refusal diagnostics, see
[Provider identity and resolution](providers.md#provider-identity-and-resolution).
To add a lane or fix its login, follow [Provider setup](providers.md#provider-setup).
