---
title: "Scope: performance-weighted crew selection"
owner: claude
status: Draft
feature: crew-performance
doc_role: scope
type: design
summary: "Scope: weight crew pool draws by each crew's implementer score on the task's type within its complexity tier, either toward the crew that does that type best (absolute) or toward the type each crew does best relative to its own work (comparative)"
tags: [crew-performance, crews, crew-pools, agent-scorecards, admission]
last_updated: 2026-10-25
last_validated: 2026-10-25
---

# Scope: performance-weighted crew selection

Status: phases 1–4 implemented (type list, classification, the by-type cell report, the persisted hourly snapshot with its readiness line, performance-weighted draws with their provenance, and shadow modes with the share comparison report).
Bearing: complexity and purpose pools (`workflow.<complexity>_complexity_crews`,
`workflow.purpose_crews.*`) decide which crews may run a task, and their weights express the
operator's preference among them. [Usage pressure](../usage-pressure/1_scope.md) scales those
weights by how much provider capacity is about to expire. The implementer scorecard
([agent scorecards §5.2](../agent-scorecards/2_design.md#52-implementers), [ORB-17779]) already
scores every crew against the fleet's rates for the same complexity tier. This scope adds a
second multiplier, from that score split by task type, without changing which crews a pool may
draw.

## Problem

A pool draws on fixed weights whatever the task asks for. The low pool sends a review partition,
a dashboard change and a config refactor to the same crews in the same proportions. The
scorecard says crews are not equally good at all of them, but nothing reads it at draw time.

Two different questions hide in "use the scorecard":

1. **Absolute advantage.** Which crew in the pool does this type of task best? Sending more of
   the type to it raises the quality of that type.
2. **Comparative advantage.** Which type of task is each crew best at **relative to its own
   other work**? Luna may not be the best reviewer in the low pool, but if review is what luna
   does best, giving luna the reviews frees the absolute leader for the work where its lead is
   larger.

Absolute advantage alone tends to send every type to the same crew. That concentrates usage on
one provider, which works against usage pressure and trips the limit gate sooner. It also starves
the other crews of new evidence, so the ranking can no longer notice a crew improving or the
leader slipping. Comparative advantage keeps every crew busy and moves only which tasks it gets.

## Goal

Raise delivered quality per task by matching task types to crews on measured evidence, while
pool membership and configured weights remain the operator's. Performance only scales weights,
as pressure does, and the two compose.

## Shape

### Task type

A **task type** is one name per task, from an ordered list in workspace configuration:

```toml
[[workflow.crew_performance_types]]
name = "review"
tags = ["no-diff-expected"]

[[workflow.crew_performance_types]]
name = "ui"
tags = ["ui"]
paths = ["web/**"]
```

- A task takes the **first** type whose `tags` it carries (any of) or whose `paths` match any of
  its `context_files` (any of). A task matching none is `other`. One type per task keeps the
  evidence from being counted twice.
- The type is read when the task is drawn, and the scorecard classifies historical tasks with the
  same list at scan time. Editing the list therefore re-sorts history, which is intended: the
  list is a lens, not a record.
- The list is configuration because the useful split is workspace-specific. Orbit ships no
  types beyond `other`, which keeps it generic. Auto-concurrency's work class
  ([§3.1](../auto-concurrency/2_design.md#31-work-class)) is a resource grouping
  (`change`, `no-diff`, `auto-task:<template>`) and too coarse for quality: a dashboard change
  and a refactor are both `change`.
- A type earns its place by a visibly different score in the report, the same rule
  auto-concurrency applies to new work classes. Every type divides the evidence further.

### Cell scores

A **cell** is `(crew, model, tier, type)`. Its raw score uses the implementer formula unchanged,
with the fleet expectation taken from the same `(tier, type)` cell instead of the tier alone:

```
expected(cell, term) = n(cell) × fleet_count(term, tier, type) / fleet_tasks(tier, type)
raw(cell) = 100 + Σ term [weight(term) × (actual − expected) × 100 / n(cell)]
```

100 means the crew matches the fleet on that type in that tier. Sparse `(tier, type)` fleet
cells, under 30 observations, fall back to the tier's rate, as sparse tiers already pool today.

Cells are small, so the draw uses a **two-sided, two-level** shrinkage:

```
crew_mean = (n_tier × raw_tier + K × 100) / (n_tier + K)        # crew's tier score
score     = (n_cell × raw_cell + K × crew_mean) / (n_cell + K)  # cell score
K = 30
```

This differs from the ranking's one-sided `min(raw, …)` on purpose. The ranking only needs to
stop small rows rising on luck. A draw also needs to stop small rows **falling** on luck: three
bad reviews must not cut a crew's review share. A cell with no evidence scores its crew mean,
and a crew with none scores 100.

Rows follow the crew's **current** model. Evidence from a crew's earlier model does not carry
over when a crew is repointed, so a renamed lane starts neutral.

### Performance factor

`k` is `workflow.crew_performance_strength` (default 3) and `cap` is
`workflow.crew_performance_cap` (default 4):

| Mode | Advantage | Factor |
|---|---|---|
| `absolute` | `score − 100` | `clamp(exp(k × advantage / 100), 1/cap, cap)` |
| `comparative` | `score − crew_mean` | same |

```
effective_weight = weight × pressure_factor × performance_factor
```

- The exponential handles any score, including below zero, and is symmetric: +10 points is
  ×1.35 at `k = 3`, and −10 is ×0.74.
- **Comparative needs no fleet term.** Balassa's revealed comparative advantage compares a
  crew's share on a type with its own overall share, relative to the fleet's. Cell scores are
  already fleet-normalised per cell (the fleet scores 100 everywhere), so the fleet ratio is 1
  and the index reduces to the crew's type score against its own tier mean.
- **Absolute moves share between crews. Comparative moves work within a crew.** Comparative
  advantages average about zero over a crew's own task mix, so its total share stays near its
  configured weight while its types change. That is why comparative composes cleanly with
  usage pressure: pressure decides how much each account works, and comparative decides on what.
- Pool weights already carry the operator's absolute preference (opus at 50 in the hard pool).
  Comparative re-allocates inside that preference, which is the case where comparative
  advantage is the right rule. Absolute applies the scorecard's level on top of the operator's
  level, which double-counts unless the pool is flat.
- A weight of `0` stays `0`, the factor never adds a crew outside the pool, and the usage-limit
  gate still removes crews first.

### Worked example

Illustrative numbers, not measurements. Low pool `haiku:40, luna:30, gemini-flash:30`, a
`review` task, `k = 3`:

| Crew | Review score | Tier mean | Static | Absolute | Comparative |
|---|---|---|---|---|---|
| haiku | 115 | 114 | 40% | ×1.57 → 51% | ×1.03 → 37% |
| luna | 108 | 98 | 30% | ×1.27 → 31% | ×1.35 → 37% |
| gemini-flash | 90 | 92 | 30% | ×0.74 → 18% | ×0.94 → 26% |

Haiku is the best reviewer, so absolute gives it half the reviews. Review is luna's strongest
type (+10 over its own mean against haiku's +1), so comparative gives luna the largest gain and
leaves haiku's capacity for the types where its lead is wider.

### Where it applies

The factor applies to the same draws as usage pressure: task creation, complexity re-rate and
the admission re-draw of a system-chosen crew. It does **not** trigger an admission re-draw by
itself. Scores move over days, so the creation draw is fresh enough; when both factors are on,
the pressure re-draw carries both.

An explicit crew, a parked crew and a task outside a pool are never touched.

### Evidence snapshot

A draw never scans. The scorecard's cold scan takes about 5 seconds on `ws_orbit`
([§8](../agent-scorecards/2_design.md#8-scan-cache-and-budget)), which is too slow for task
creation and every admission pass.

- The scan writes a **performance snapshot** to the workspace store: one row per cell with its
  raw score, `n`, the shrunk score and the crew mean, plus the window, the type list's hash and
  `computed_at`.
- A seeded routine refreshes it hourly. A dashboard scorecard scan refreshes it too.
- A draw reads the snapshot. A missing snapshot, one older than 24 hours, or one built from a
  different type list is ignored, and every factor is 1 with a recorded reason.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `workflow.crew_performance` | `off` | `off`, `absolute`, `comparative`, `shadow_absolute` or `shadow_comparative`. Independent of `workflow.crew_selection`. Shadow computes the same factor as the live mode, records it with `applied: false`, and draws on the weights `off` would use. Only `off` skips the snapshot refresh. |
| `workflow.crew_performance_strength` | `3` | `k`; at least `0`, and `0` disables the factor. |
| `workflow.crew_performance_cap` | `4` | Factor clamp `[1/cap, cap]`; at least `1`. |
| `workflow.crew_performance_window_days` | `30` | Evidence window, at most the scan's 90 days. |
| `workflow.crew_performance_types` | `[]` | Ordered type list; empty means every task is `other`, so the factor uses tier scores only. |

A separate key rather than more `crew_selection` values keeps the two factors composable:
`crew_selection = "usage_pressure"` with `crew_performance = "comparative"` is the intended
combination.

### Surfaces

- `crew_selection` in run input and `orbit run show` gains, per eligible member, a
  `performance` object: mode, `applied`, type, cell score, crew mean, `n`, factor and snapshot
  time. `applied` is true for `absolute` and `comparative`, false for either shadow mode.
  History's `crew_redrawn` carries the factor only when it was applied.
- Readiness and doctor show the snapshot's age and type list, and say when it is ignored.
- `orbit agents scorecard --role implementers --by-type` (and its `--json`) prints the cell
  matrix. With `--shadow` as well, the same command prints each crew's static, drawn and
  would-be share per pool and type over `--window`, and lists draws with no performance
  record separately as static. The dashboard gets no new section: the scorecard tab stays as
  it is, and a call without the switches keeps the ranking.

### Offline type discovery

`orbit agents scorecard --role implementers --per-task --json` exports one row per
implemented task in the window: its crew, model, tier, currently configured type, tags,
context-file selectors and top-level path prefixes, and its per-term counts under the
existing term definitions and weights. Summed per `(crew, model, tier, type)`, those rows
reproduce the `--by-type` cells' `n` and raw values on the same scan. The export is
read-only: it writes no snapshot and refreshes no cache beyond the scan the scorecard
already makes.

The standalone script in `tools/crew-type-discovery` (not part of the workspace build)
reads that export from a file or stdin and, per `(crew, model, tier)` (or per tier with
`--group-by tier`), fits a shallow regression tree on each task's score relative to its
crew's own tier mean — the comparative-advantage target above. Features are tags and
top-level path prefixes. It prints each leaf with its `n`, mean relative score and the
predicate that reaches it, then a ready-to-paste `[[workflow.crew_performance_types]]`
block for each leaf that passes its thresholds: at least `--min-leaf` tasks (default 30,
the sparse-cell floor) and a mean at least `--margin` points from its parent (default 8).
`--max-depth` (default 3) bounds the tree, and `--json` emits the same report as JSON.

Read a suggested type as a hypothesis, not a finding: check it in the `--by-type` report
before adopting it. The report suggests candidate splits and never applies them — it
changes no draws, pools, weights or configuration, and no routine runs it.

## Non-goals

- **Changing pool membership or complexity routing.** A hard task never goes to a crew outside
  the hard pool.
- **Choosing types automatically.** No clustering of tasks into types and no automatic
  adoption. The offline discovery report suggests candidate splits; the operator writes
  the list and the report shows whether each type earns its place.
- **Scoring reviewers, orchestrators or pilots for routing.** Only implementer draws.
- **Cost.** Cost stays shown, never scored
  ([§4.5](../agent-scorecards/2_design.md#45-cost-is-shown-never-scored)).
- **Optimal assignment.** Draws happen one task at a time, so comparative advantage is a greedy
  approximation of the assignment problem, not a batch solver.

## Concerns and honest limitations

- **Evidence is thin.** Ten-odd crews × three tiers × a few types gives many cells under 30
  tasks. Most factors will sit near their crew mean for weeks. Expect `comparative` to do little
  until a type has a few hundred tasks a month.
- **The policy shapes its own evidence.** Absolute starves losing crews of a type and comparative
  concentrates each crew on its strong types. The cap's `1/cap` floor and the shrinkage toward
  the crew mean bound this: a cell that stops receiving work drifts back to neutral, not to
  zero. A cell that never gets work can never prove itself, which is accepted.
- **Review terms depend on the reviewer.** First-pass accepts and rejects are verdicts by
  another crew. If one reviewer crew sees most of a type, its strictness reads as implementer
  quality. Reviewer assignment is not controlled for.
- **Tags drift.** The type is read from tags at draw and at scan. A task re-tagged after landing
  moves its evidence. Accepted for simplicity: recording the type per delivery is possible
  later.
- **Absolute double-counts.** Pool weights already express "opus is better at hard work". The
  absolute factor adds the scorecard's view of the same thing. It suits flat pools, not tuned
  ones.
- **Followers draw on the owner's snapshot.** A pull leaf's crew is drawn on the owner, and a
  follower's pressure re-draw reuses the recorded performance factor rather than computing its
  own. A follower has no scorecard of its own.

## Phasing

1. **Type list and cell report.** `workflow.crew_performance_types`, classification, cell scores
   with two-level shrinkage, and `orbit agents scorecard --by-type`. Selection does not change.
   This lets the operator see whether any type differs enough to route on.
2. **Snapshot.** The store table, the hourly routine, staleness rules, and readiness and doctor
   lines.
3. **Weighted draws.** The `crew_performance*` keys, the factor beside pressure in the shared
   draw helper (`pressure_weighted_candidates` becomes the general weighting seam), run
   provenance and history. Default `off`.
4. **Shadow trial.** `shadow_absolute` and `shadow_comparative` record the factor the draw
   would have used, with `applied: false`, and do not apply it. `orbit agents scorecard
   --role implementers --by-type --shadow` compares static, drawn and would-be shares.
   Running the trial and choosing a default stay with the operator.

## Open questions

1. **Default mode.** After the trial, should `ws_orbit` run `comparative`, or `absolute` on the
   flatter low pool only?
2. **Per-delivery type.** Should the type be recorded on the delivery at admission, as work class
   is, so that history no longer moves when tags change?
3. **Activity draws.** Should review, finding-verification and explore pools take a factor from
   their own role's scorecard through the same seam?
4. **Batch admission.** When a drain admits many tasks in one pass, should it assign them jointly
   so comparative advantage is applied across the batch rather than greedily?

## Acceptance (feature level)

- With `crew_performance = "absolute"`, the low pool and snapshot of the worked example, a
  `review` task's `eligible_pool` shows haiku ×1.57, luna ×1.27 and gemini-flash ×0.74, and
  `orbit run show` names the mode, type and cell scores.
- With `crew_performance = "comparative"` on the same snapshot, the factors are ×1.03, ×1.35 and
  ×0.94.
- With `crew_selection = "usage_pressure"` as well, each member's effective weight is the product
  of both factors and the record shows both.
- A cell with no evidence gets its crew mean, a crew with none gets 100, and a missing, stale or
  mismatched snapshot gives every crew factor 1 with a reason in readiness.
- With `crew_performance = "off"`, draws are byte-for-byte what they are today for the same seed.
- `orbit agents scorecard --role implementers --by-type --json` returns every cell with raw score,
  `n`, shrunk score and crew mean.

## Task References

- [ORB-17118] — scores orchestrators on unit landings instead of complexity-weighted ones
- [ORB-17779] — scores implementers against fleet rates for the same complexity tier

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
