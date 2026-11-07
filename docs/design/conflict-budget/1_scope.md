---
title: "Scope: conflict budget for footprint admission"
owner: claude
last_updated: 2026-11-07
last_validated: 2026-11-07
status: Draft
stage: parked
next: "Parked for free time (Daniel, 2026-11-02); on pickup settle open questions, file phase 1"
feature: conflict-budget
doc_role: scope
type: design
summary: "Scope: let footprint admission run overlapping tasks together when the expected cost of repairing their landing conflicts fits an operator budget that can be high by day and low at night, scored per path from recorded conflict history"
tags: [conflict-budget, footprints, admission, locks, conflict-recovery]
---

# Scope: conflict budget for footprint admission

Status: proposed.
Bearing: admission holds a context lock for every declared `context_files` selector and defers a
candidate on any overlap with a held claim, reservation or wave member
(`footprints_block`, defined in [`lock_mode.rs`](../../../crates/orbit-common/src/fs/lock_mode.rs), is consulted by [`coordination/admission/footprint.rs`](../../../crates/orbit-store/src/repository/task/coordination/admission/footprint.rs),
via the `overlaps` closure in `selection.rs`, and by `owner_wave.rs` and the runtime lock index;
a footprint-less editing task was treated as a whole-tree lock in multi-slot drains by [ORB-15191]). Two settings already relax
that per path: [`workflow.unlocked_context_paths`](../../CONFIG.md#unlocked-context-paths)
(globs, default `["**/*.md"]`) takes no lock, and
[`workflow.generated_paths`](../../CONFIG.md#generated-paths-are-regenerated-not-merged)
([ORB-16784]) regenerates a conflicting generated file at landing instead of merging it, and
[`workflow.lock_modes`](../../CONFIG.md#footprint-lock-modes) ([ORB-18157], done) waives an
overlap at a path whose mode is `shared`. A
conflict that reaches landing is repaired by `pr_conflict_recovery`, whose crew comes from
`workflow.purpose_crews.failure_recovery` ([ORB-15663]); ws_orbit draws it from
`gemini-flash, haiku, luna, muse-spark`. This scope makes the lock a priced decision instead of a
static allow-list.

## Problem

On 2026-10-24 eleven `auto-task-catalog-2026-10-24` tasks entered ws_orbit's backlog. Each adds
one shipped auto-task: a YAML file of its own, one line in the `DEFAULT_AUTO_TASK_FILES` list in
`crates/orbit-core/src/application/auto_tasks/mod.rs`, and a regenerated
`crates/orbit-core/assets/auto_tasks/body-history.json`. Their own files never overlap, yet they
run one at a time, because all eleven declare those two shared files. The same happens wherever
work fans into a registry, a generated index or a hub config such as
`crates/orbit-config/assets/default-config.toml`.

Run one at a time, the eleven take eleven delivery wall-times. Run together, the ten that land
second or later each rebase, and a conflict on the registry line costs one cheap recovery
round. The operator wants to make that trade **while at the desk** — their time costs more than a
haiku rebase — and to keep the conservative behaviour **overnight**, when nobody is waiting and a
broken `agent-main` would sit until morning.

Orbit has no way to express that trade today:

- The lock is static. `unlocked_context_paths` and `workflow.lock_modes` are static lists that
  apply all day, however rarely a given path actually conflicts.
- Orbit does not know what a conflict on a given path costs, or how often overlapping work on it
  conflicts. What it records today:

  | Outcome | Where the conflicting paths are recorded | Read by codebase facts |
  |---|---|---|
  | Conflict recovery failed, local run (`pr_conflict_blocked`, or `pr_failure_handoff` with `failed_step=sync_base`) | Task history note text (`conflicts=a, b`) | Yes, `BlockCause::BaseConflict(paths)` in [`block_cause.rs`](../../../crates/orbit-core/src/application/codebase/facts/block_cause.rs) |
  | Conflict recovery failed, claimed leaf (`ClaimFailureClass::BaseConflict`) | Nowhere on the owner: the settlement carries the class only | As a conflict with no paths |
  | Conflict recovered by `pr_conflict_recovery` | The run's `PipelineState::rebase_recovery_checkpoints` (`RebaseRecoveryCheckpoint::conflicting_paths`), on the host that ran it | No |
  | Overlapping work that rebased cleanly | Nowhere directly; derivable from landed paths and in-flight intervals | No |
  | Admission refused for a lock conflict | `orbit.task.locks.reserve` denials (`list_task_lock_denials`) | Yes, `LockDenial` |

  The commonest and cheapest outcome — a recovered conflict — is the one the owner cannot see,
  and nothing records what a recovery cost.

## Goal

The operator sets how much expected conflict-repair work Orbit may take on to run overlapping
tasks together, and can set it higher during working hours than overnight. Admission admits an
overlapping candidate when the expected repair cost of its overlaps, estimated per path from
recorded history, fits that budget. With the budget at its default of zero, admission is exactly
today's: every locked path is exclusive and `unlocked_context_paths` still takes no lock. Landing,
conflict recovery and review are unchanged; the budget changes only which work runs at the same
time.

## Shape

### 1. Record every landing conflict on the owner

Each synchronization that met a conflict records one typed **conflict record** on the owner's
task: the run, the conflicting paths, how it resolved (`recovered`, `regenerated`, `blocked`), the
recovery crew and its cost (recovery rounds and wall minutes, plus provider usage where the run
recorded it). A local run writes it from its rebase recovery checkpoint. A claimed leaf carries
the same fields in its handoff or failure settlement; an owner or follower that predates the
field reads it as "paths unknown", as today, within the 30-day compatibility window.

Codebase facts read these records beside the existing block causes. For each path they then
know:

- `co_edits`: landed tasks that changed the path while another task that also changed it was in
  flight — the denominator, derived from landed paths and in-progress intervals, with no new
  record.
- `conflicts`: conflict records naming the path.
- `repair_cost`: the mean cost of the recoveries that resolved a conflict on it.

### 2. Score each path

```text
p(path) = (conflicts + α·p₀(class)) / (co_edits + α)      # smoothed conflict rate
c(path) = repair_cost when support ≥ min_support, else c₀(class)
risk(path) = p(path) · c(path)                             # expected repair cost per extra holder
```

`class` is a cheap, deterministic prior from the path itself: `generated` (matches
`workflow.generated_paths`; the repair is the regenerate command), `docs`, `config`, `code`.
The priors (`p₀`, `c₀`) are configuration, so a fresh workspace starts from them, and history
replaces them as support grows, following the window and minimum-support pattern of
[task riskiness](../../../crates/orbit-core/src/application/codebase/riskiness.rs).

A conflict rate measures textual conflicts only. Whether a clean merge of two edits still breaks
behaviour depends on the code, which is where an optional **complexity weight** helps: a periodic
low-pool auto-task scores files for logic complexity and writes a durable weights artifact that
scales `c₀(code)`. Admission reads the artifact, never an agent's message, and works without it.

### 3. Admit within the budget

When a candidate's locked selectors overlap paths already held, by claims, reservations or wave
members ahead of it, its **overlap set** is the paths where they overlap: for a `dir:` against a
`file:` selector, the file. Then:

```text
overlap_risk(candidate) = Σ over path ∈ overlap set: risk(path) · holders(path)
admit when overlap_risk(candidate) ≤ budget(now)
       and no path in the overlap set is pinned exclusive
       and holders(path) < max_holders_per_path for every path in it
```

`holders(path)` counts the tasks already holding it, because the k-th task to land on a busy
path rebases against k−1 changes. Paths matching `unlocked_context_paths` keep taking no lock
(`risk = 0`); paths matching `workflow.exclusive_context_paths` are never shared at any budget.
Owner pull admission applies the same rule, which also closes today's gap where it checks a
claim's full footprint, unlocked paths included, against in-flight work.

Worked example, ws_orbit's eleven catalog tasks at a daytime budget of 15 and the default cap of
4 holders, if neither shared file were unlocked:

| Path | Class | p | c (agent-min) | risk |
|---|---|---|---|---|
| `crates/orbit-core/src/application/auto_tasks/mod.rs` | code | 0.6 | 6 | 3.6 |
| `crates/orbit-core/assets/auto_tasks/body-history.json` | generated | 1.0 | 2 | 2.0 |

The second task's overlap risk is 1 × 5.6 and the third's 2 × 5.6 = 11.2; both admit. The
fourth's would be 16.8, over budget, so it waits until one lands. At a budget of 30 the fourth
admits too and the fifth stops at the holder cap, whatever the budget. Overnight the budget is 0,
so the same tasks run one at a time, as today. The numbers are illustrative; phase 1 measures
them.

### 4. Schedule the budget

The budget is a host-local schedule in the owner's workspace configuration, not a routine:
routines target catalog jobs and carry no inline commands, and changing admission settings is not
a job.

```toml
[workflow.conflict_budget]
default = 0
windows = [{ days = "mon-fri", from = "09:00", to = "19:00", budget = 30 }]
```

`orbit run auto --conflict-budget <n>` overrides it for the length of one drain, for the "I'm at
the desk right now" case. Admission captures the budget it used in its receipt, so a decision can
be explained later.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `workflow.conflict_budget` | `0` (or a `{default, windows}` table) | Expected repair cost, in recovery agent-minutes, that admission may take on per candidate by running it beside overlapping work. `0` keeps today's exclusive locks. |
| `workflow.exclusive_context_paths` | `[]` | Path globs (`unlocked_context_paths` syntax) that are never shared at any budget. |
| `workflow.conflict_budget_max_holders` | `4` | Most tasks that may hold one path at once. |
| `workflow.conflict_risk.window_days`, `.min_support`, `.priors` | `30`, `5`, per class | Scoring window, support needed before history replaces a class prior, and the priors; following the task riskiness model's window and support. |

### Surfaces

- Admission diagnostics name the overlap set, its risk and the budget for each candidate that was
  admitted beside overlapping work or deferred over budget, beside today's `deferred_conflicts`.
- `orbit codebase show` gains a conflict section: per path `co_edits`, `conflicts`, `p`, `c`, and
  the minutes tasks waited on it.
- `orbit run show` reports the conflict records of a run's synchronization.

### 5. Optimistic, symbol-level admission

Daniel (2026-11-02) asked for optimistic locking: "since now we are locking at file and symbol
level, as long as no two symbols are overlapping or that symbol hasn't been updated in any of the
tasks in "review state"". He asked for it to be recorded here and parked: "we don't have to
implement this now. something to pick up on when we have free time".

**Today's lock is pessimistic, and file-grained even for symbols.** Admission defers any candidate
whose footprint overlaps a held claim, reservation or wave member. A `symbol:` selector locks its
whole file: `symbol:a.rs#one:function` overlaps `symbol:a.rs#two:function`
([`selector.rs`](../../../crates/orbit-common/src/fs/selector.rs), `overlaps`). Two tasks editing
different functions of one file therefore run one after the other.

**Landing is already optimistic.** Landing rebases onto the moved base. `pr_conflict_recovery`
repairs text conflicts, and candidate validation re-runs on the rebased tree. What is missing is
admitting work on that assumption. The proposal treats three kinds of other task differently:

| Other task's state | What its footprint is | Rule |
|---|---|---|
| `review` (a candidate diff exists) | **Actual**: the symbols its candidate changed | Pessimistic. Defer a candidate whose declared symbols intersect them. |
| `in-progress` / claimed | **Declared**: its `context_files`, a prediction | Optimistic. Admit when the declared `symbol:` selectors are disjoint, and verify at landing. |
| Landed since the candidate's base | **Actual** | Verified at landing (below). |

- **Symbol-granular overlap.** A file stops being a single lock only when every selector on both
  sides of it is a `symbol:` that resolves to a source range. Any `file:`/`dir:` selector, or an
  unresolvable symbol, falls back to today's file lock. Symbol resolution is language-specific,
  so it lives in a plugin (orbit-graph or the [symbol-risk-index](../symbol-risk-index/1_scope.md)
  parser), never in core.
- **Verify actual diffs at landing, not declarations.** Pilot-declared footprints are predictions,
  and implementing agents routinely edit beyond them (imports, `mod.rs`, shared helpers, tests).
  At landing, compute the symbols the candidate actually changed and intersect them with the
  symbols landed since its base. On an intersection, take the normal rebase and recovery path,
  and record the outcome as a conflict record (Shape §1) tagged `optimistic`.
- **Price it with this budget.** An optimistic admission is an overlap at file level with zero
  symbol overlap, so it enters `overlap_risk` with its own risk term: the measured rate at which
  symbol-disjoint pairs still conflicted or broke the build, times the repair cost. A budget of
  `0` keeps it off.

**Risks this must answer:**

- **Clean merges can still break the build.** If task A changes a signature and task B adds a
  caller in a different symbol, git merges cleanly and the build breaks. Post-rebase validation
  catches it, but the cost is a re-run, not a rebase. Call-edge overlap from the symbol index
  narrows this risk without removing it.
- **Wasted runs cost more than rebases.** A failed optimistic bet can discard a top-tier crew's
  whole run. The repair cost should be weighted by the crew and complexity that would be redone,
  not by recovery agent-minutes alone.
- **Declarations get finer and wronger.** The finer the declared lock, the more often it is wrong,
  so the landing check must stay authoritative.

**Gate.** First run the replay: for landed task pairs that were serialized on a file, measure
whether their actual diffs touched intersecting symbols or call edges, and whether running them
together would have conflicted or broken the build. Build this only if the replay shows a lot of
parallelism that broke nothing.

## Non-goals

- **Changing landing or conflict recovery.** The budget only decides what runs at the same time;
  recovery keeps its crews, rounds and generated-path regeneration.
- **Language-aware locking in core.** Symbol resolution and call-graph analysis stay in a plugin (Shape §5),
  per Orbit's generic-core rule.
- **An agent in the admission path.** Scores come from recorded facts and an optional durable
  weights artifact; admission never waits on, or trusts, an agent's answer.
- **Auto-tuning the budget.** The operator sets it. Tying it to usage pressure or queue depth is a
  later question.

## Concerns and honest limitations

- **Textual conflicts are not the whole cost.** Two edits can merge cleanly and still break the
  build or the behaviour. That lands as a red `agent-main` and a repair task, not as a conflict
  record. `c` only proxies it through the class prior and the optional complexity weight; a
  conflict record linked by `regression_from` would show it, but slowly.
- **Exclusive paths never generate data.** A path held exclusively never sees concurrent edits, so
  its `p` stays at the prior. Raising the budget is how data arrives; there is no deliberate
  exploration.
- **Small samples.** Most paths will sit on their class prior for a long time. The smoothing
  keeps one unlucky conflict from swinging a path, and also keeps estimates slow to move.
- **Units are approximate.** "Agent-minutes" mixes cheap and expensive crews and ignores the
  re-review a rebase can trigger before landing. The budget is a dial, not an accounting.
- **Followers' history.** Until claimed leaves carry conflict records, a pull-heavy workspace
  under-counts its recovered conflicts.

## Phasing

0. **Configuration only, today (ws_orbit).** Add `**/mod.rs` and
   `crates/orbit-config/assets/default-config.toml` to `workflow.unlocked_context_paths`, and
   map `crates/orbit-core/assets/auto_tasks/body-history.json` to
   `python3 scripts/update-auto-task-body-history.py` in `workflow.generated_paths`. This unblocks
   the catalog tasks with no code change and is the baseline the later phases are measured
   against.
1. **Record and show.** Conflict records on the owner for local runs and claimed leaves
   (Shape §1), the `orbit codebase show` conflict section, and wait minutes per path. Before
   anything is built on them, verify whether drain-wave deferrals are persisted; if they are not,
   record them here.
2. **Score and admit at a fixed budget.** Shape §2 and §3, with `workflow.conflict_budget` as a
   number, default `0`, and `exclusive_context_paths`.
3. **Schedule.** Budget windows and the `--conflict-budget` drain override.
4. **Complexity weights (optional).** The low-pool auto-task and its weights artifact.
5. **Symbol replay (research).** The Shape §5 gate, in a plugin; continue only if the numbers earn it.
6. **Optimistic symbol-level admission.** Shape §5: symbol-granular overlap; actual-diff
   footprints for tasks in `review`; disjoint declared symbols admitted beside `in-progress`
   work and verified at landing; outcomes recorded as `optimistic` conflict records and priced by
   the budget.

## Open questions

1. **Sum or max?** Summing over the overlap set prices a candidate by its total expected repair
   cost; the maximum is simpler and stricter on one hot path but ignores breadth. Sum is proposed.
2. **Where do the complexity weights live?** A task artifact on the auto-task's run, or a
   workspace-local file under `.orbit/`? Either is durable; the second is easier for admission to
   read.
3. **Does a generated path need a lock at all?** Its repair is mechanical, so
   `workflow.generated_paths` could imply "unlocked" by default instead of being listed twice.
4. **Glob or regex?** [ORB-18157] landed regex lock modes as `workflow.lock_modes`. Every path
   setting here uses `unlocked_context_paths` globs (`**/mod.rs` already covers every `mod.rs`);
   one pattern syntax across settings is proposed, and this budget would layer on the landed
   `workflow.lock_modes` rather than replace it.

5. **Optimistic admission and the budget.** Should an optimistic (symbol-disjoint) admission
   always spend budget, or be a separate switch that is on by day and off at night? Spending
   budget keeps one dial, and a separate switch makes failures easier to attribute.

## Acceptance (feature level)

- With `workflow.conflict_budget` unset or `0`, every admission decision is identical to today's,
  for local waves, owner pull admission and lock reservations.
- After a conflict is recovered on any host, the owner's task holds a conflict record naming the
  paths, the resolution and the recovery cost, and `orbit codebase show` counts it for each path.
- Given a positive budget and two backlog tasks overlapping only on a path whose risk fits it,
  both are admitted together, and the diagnostic names the overlap set, its risk and the budget;
  over budget, the later one is deferred with the same detail.
- A path matching `exclusive_context_paths` is never shared, and no path ever has more than
  `conflict_budget_max_holders` holders.
- Inside a configured window the window's budget applies; outside it the default does; a drain
  started with `--conflict-budget` uses that value for its whole window, and every receipt records
  the budget it used.

## Task References

- [ORB-15191] — treated a footprint-less editing task as a whole-tree lock in multi-slot drains
- [ORB-15663] — routed conflict recovery through the failure-recovery purpose pool
- [ORB-16784] — regenerated configured generated paths at conflict recovery instead of merging them
- [ORB-18157] — landed per-path regex lock modes as `workflow.lock_modes` (exclusive/shared)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
