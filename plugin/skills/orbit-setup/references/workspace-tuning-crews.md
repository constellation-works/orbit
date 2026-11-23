# Workspace tuning: crew routing, review layers and analytics

## Contents

- [6. Crew routing](#6-crew-routing)
  - [Crews and the default](#crews-and-the-default)
  - [Complexity pools: `workflow.<tier>_complexity_crews`](#complexity-pools-workflowtier_complexity_crews)
  - [Purpose pools: `workflow.purpose_crews.<purpose>`](#purpose-pools-workflowpurpose_crewspurpose)
  - [Task types and type pools](#task-types-and-type-pools)
  - [Automatic effort: `<crew>@auto` members](#automatic-effort-crewauto-members)
  - [Draw policy: `workflow.crew_selection`](#draw-policy-workflowcrew_selection)
- [7. Review layers](#7-review-layers)
- [8. Analytics classification](#8-analytics-classification)
  - [Regression layers](#regression-layers)

## 6. Crew routing

Routing decides which crew runs each task. Decide it in this order, because
each layer falls through to the next.

```text
explicit crew → purpose pool → type pool → complexity pool → default_crew
```

### Crews and the default

Start from `orbit doctor providers` and `orbit config show`: only crews whose
provider is installed and authenticated on the executing hosts can run. Set
`workflow.default_crew` to the strongest generally available crew; it catches
every empty or fully disabled pool. Disable a crew (`enabled = false`) rather
than deleting it, so history and pools that name it still load.

### Complexity pools: `workflow.<tier>_complexity_crews`

The backbone. Rank the available crews by quality and cost for this
repository (the user's experience, then the implementer scorecard once it
has history) and assign:

- `low`: the cheapest crews that still follow instructions reliably;
- `medium`: mid-tier crews;
- `hard` and `xhard`: the strongest crews; `xhard` is the tier the pilot never
  demotes from, so keep it to the very best.

Weight members (`["opus:100", "sol:50"]`) to set the share each gets; a pool
is all bare or all weighted. Weight `0` parks a crew without removing it.
Spread each tier across providers where quality allows, so one provider's
usage limit or failure hold redraws inside the pool instead of stalling it.

### Purpose pools: `workflow.purpose_crews.<purpose>`

Orbit's own work: `task_pilot`, `failure_recovery`, `final_recovery`,
`auto_fix`, `review`, `pre_landing_review`, `baseline_red_repair`,
`sweep_tasks`, `pilot_warning_triage`. `orbit init` writes most of them as
fixed lists of the enabled crews (not `sweep_tasks` or
`pilot_warning_triage`), so editing a complexity pool later does **not** move
them: revisit each one whenever you retune the tiers. Guidance:

- `task_pilot`, `auto_fix`: cheap, fast crews; the pilot also
  needs crews whose executor declares `inspection_args`.
- `failure_recovery`: crews good at diagnosis; recovery sees failures from
  every tier.
- `review` and `pre_landing_review`: a different provider family from the
  usual implementers, so review is independent; the draw already prefers a
  crew that did not implement the work.
- `final_recovery`: one strong crew; `[]` turns final recovery off.
- `baseline_red_repair`, `sweep_tasks`: leave unset to route by type and
  complexity, or pin to crews that fix CI well.
- `pilot_warning_triage`: the triager of each held task-pilot assessment (a
  pilot warning). Triagers inspect the pinned source natively, so only crews
  whose executor declares `inspection_args` are drawn, and the crew that wrote
  the held assessment is passed over while the pool offers another.
  `orbit init` does not write this pool, so while it is unset or `[]` triage
  stays on the hard-tier route and draws from `workflow.hard_complexity_crews`.

  Leave it unset to keep that route, or pin the triagers to crews that declare
  `inspection_args` (the Claude and Codex executors do), for example:

  ```bash
  orbit config set workflow.purpose_crews.pilot_warning_triage '["sol", "opus"]'
  ```

  Verify the resolved pool with `orbit config show --all`. Its **Crew
  purposes** block shows `pilot_warning_triage` with your pool and its key, or
  `unset; falls back to workflow.hard_complexity_crews` while it is unset.

### Task types and type pools

`workflow.type_complexity_crews.<type>.<tier>` overrides one tier's pool for
one kind of work, such as routing hard UI work to the crew that renders and
checks UI best. Types come from `[[workflow.task_types]]`, which
also feed the implementer scorecard, so define them first:

- Keep the list short (about five to eight types) so each type and tier
  accumulates enough deliveries to score.
- A task takes the **first** type whose `tags` it carries or whose `paths`
  match one of its `context_files`. Put specific types before broad ones,
  and a `review` type keyed on `no-diff-expected` first so audits never
  classify as code work.
- Derive tags and paths from the backlog survey: the tags the repository
  already uses, and the source trees each kind of work lives in.
- Editing the list re-sorts the scorecard's history and re-routes open tasks
  on their next draw.

Then add type pools only where a kind of work measurably needs a different
crew, usually at `hard`. Every member must be a defined, enabled crew with a
positive weight; remove a key instead of leaving it empty. Untyped work and
unlisted tiers keep the complexity pool.

### Automatic effort: `<crew>@auto` members

A pool member written `<crew>@auto` (in the implementer, task-pilot and
review pools only) lets each draw pick the crew's effort from its
`workflow.auto_effort.bands` instead of fixing one effort per member. Reach
for it when one crew should run cheap on small, low-tier work and strong on
hard work, rather than splitting the pool into fixed `<crew>@<effort>`
members per tier.

Order bands from the strongest claim to the weakest: the
first band whose conditions the draw's signal meets wins, and unmatched
draws fall back to the crew's configured effort. Tune band cut-offs from the
implementer scorecard's size curves once deliveries accumulate; a fixed 10%
of draws explores another effort, so both sides of a cutover keep getting
data.

Bands, signals, refusals and readback
verification: [Automatic effort selection](configuration-auto-effort.md#automatic-effort-selection-crewauto).

### Draw policy: `workflow.crew_selection`

- **`crew_selection`**: `static` draws on the configured weights.
  `usage_pressure` scales each member by its provider's weekly usage pressure,
  so capacity about to reset is spent first. Use it when the pools span
  subscription providers that report weekly usage windows; leave `static` for
  pay-per-use accounts or a single provider.
- **Provider limits.** A window used at 90% or more stops new draws. Use
  `provider_limit_overrides` (`provider:100` to wait for exhaustion,
  `provider:off` to only record readings) for providers whose readings
  mislead, and `provider_limit_budgets` for providers that report no usage.
  `provider_limit_explicit_crews = "pool"` lets a pinned task draw elsewhere
  instead of waiting.

## 7. Review layers

`review.timing` picks at most one pre-landing review layer: `off` (default),
`before_pr` (a fresh reviewer fixes what it finds before the PR opens) or
`before_landing` (the PR opens and is reviewed while hosted CI runs).
`review.before_landing_hosts` turns before-landing review on for named
followers only. After-landing review is the `delivery-code-review` auto-task,
not a key. Set `review.minutes` above the time the required list takes,
because review reruns checks.

Pair a pre-landing layer with
`review.baseline_commands` ([section 2](workspace-tuning-policy.md#2-required-validation-workflowrequired_validation_commands)) and a `pre_landing_review` pool
(section 6).

Selective pre-landing review (`[review.selective_before_landing]`) was
removed: its keys load with a deprecation warning and are ignored
until 2026-12-08. Delete the table; use `review.timing = "before_landing"` or
`review.before_landing_hosts` instead.

## 8. Analytics classification

`workflow.test_paths` and `workflow.non_code_paths` class landed lines as
tests, other or code. The defaults suit most layouts. Override only when the
repository keeps tests or assets in unconventional places, and copy any
default entries you still want, since an array replaces the default wholesale.
Changes apply to deliveries recorded afterwards.

### Regression layers

`workflow.regression_layers` classifies each `regression_from` link by where
the regression task's fix sits. Orbit uses the fix's landed paths, or its
declared footprint when landed paths are unavailable. Each path matches the
first layer whose `prefixes` entry it starts with, whose `dirs` entry equals
one of its directory components, or whose `suffixes` entry it ends with. The
fix is assigned the layer most of its paths match; a tie goes to the earlier
layer.

`ci_tests` and `docs_skills` count only when the fix touches nothing
else. An unmatched path is `other`; a fix with no path is `unknown`.

When unset, the list has two repository-neutral defaults:

```toml
[[workflow.regression_layers]]
name = "ci_tests"
prefixes = [".github/"]
dirs = ["tests", "testdata", "fixtures"]

[[workflow.regression_layers]]
name = "docs_skills"
prefixes = ["docs/"]
suffixes = [".md"]
```

Setting the list replaces these rows. Repeat any defaults you want to keep
before adding repository-specific layers. Every row needs a unique name other
than `other` or `unknown`, at least one matching rule, and `dirs` entries must
be single directory components. An invalid list prevents config loading and
names the key. An explicit `workflow.regression_layers = []` classifies every
path as `other`.

Check the accepted key and its fields with `orbit config keys --json`, then
verify the loaded value with `orbit config show --all`. After changing the
list, run `orbit codebase analytics --layer <name> --min-support 1 --json` to
inspect regressions assigned to a layer; use `other` or `unknown` to inspect
those buckets. Follow a ranked path with
`orbit codebase show <path> --layer <name> --min-support 1 --json` to inspect
the regression details.

Layer classification is computed when analytics runs,
so the next analytics run reflects the updated list. A layer with no matching
regression links has no rows to show.

Continue with [Roll it out](workspace-tuning-rollout.md#roll-it-out).
