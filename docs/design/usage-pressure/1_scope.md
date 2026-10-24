---
type: design
summary: "Scope: weight crew pool draws by how much of each provider account's usage window would expire unused, so Orbit spends the capacity that is about to reset before the capacity that is not"
tags: [usage-pressure, crews, crew-pools, provider-limits, admission]
last_updated: 2026-10-23
last_validated: 2026-10-23
---

# Scope: usage-pressure crew selection

Status: phase 1 pressure view and phase 2 task draws ([`workflow.crew_selection`](../../CONFIG.md#usage-pressure-draws)) implemented; phases 3–4 remain proposed.
Bearing: [provider usage limits](../../CONFIG.md#provider-usage-limits) already record every
window a provider reports ([ORB-14695] failures, [ORB-14696] post-run readings, [ORB-17644] Muse)
and gate crews that reach a threshold ([ORB-14697]). Weighted pools
(`workflow.<complexity>_complexity_crews`, `workflow.purpose_crews.*`) choose among the crews
that are left. This scope uses the readings to tilt those draws too, without changing which
crews a pool may draw.

## Problem

Subscription capacity disappears at its reset. On a Friday evening the Claude login on a host
reads `seven_day` 10% used, resetting in 6 hours. The Codex login reads its weekly window 0%
used, resetting in 7 days. Ninety percent of a Claude week is lost at midnight unless Orbit
spends it, and every Codex token spent tonight is still available next week. Orbit has both
readings and treats the two accounts the same: the hard pool `opus:50, sonnet:25, sol:25` sends
a quarter of the hard tasks to Codex, and Claude crews only lose share when the gate trips.

The readings only **remove** crews today. Nothing uses them to **prefer** one.

A second fact shapes the fix. A task's pool crew is drawn **once, when the task is created**
(`CreationCrewAssignment`, source `pool:<complexity>` or `purpose:<purpose>`, in
[`candidates.rs`](../../../crates/orbit-core/src/application/job/crew_pools/candidates.rs)).
Admission then keeps that member "while it can run", in `unheld_task_crew_candidates`, and only
re-draws when a hold or a limit removes it. A task filed on Tuesday runs on Friday with
Tuesday's draw. Weighting the creation draw alone would weight it with days-old readings.

## Goal

Spend the capacity that expires first, so that less of the accounts' combined capacity is lost
at resets. Quality routing is unchanged: complexity and purpose pools still decide which crews a
task may run as, and the configured weights still express the operator's preference among them.
Pressure only scales those weights.

## Shape

### Pressure of one window

For a live, gating reading with a known reset and window length:

```
headroom  = max(0, 1 − used_percent / 100)
remaining = clamp((resets_at − now) / window_length, ε, 1)
pressure  = headroom / remaining
```

Pressure is the headroom left per unit of window time left. `1` means the account is exactly on
pace to use the window. Above `1`, capacity will expire unless Orbit speeds up, and below `1`
the account is ahead of pace and should be spared.

| Reading | headroom | remaining | pressure |
|---|---|---|---|
| Claude `seven_day` 10% used, resets in 6 h | 0.90 | 6 / 168 = 0.036 | 25 |
| Codex weekly 0% used, resets in 7 d | 1.00 | 1.00 | 1.0 |
| Claude `seven_day` 95% used, resets in 6 d | 0.05 | 0.86 | 0.06 |
| Any window whose reset has passed with no new reading | 1.00 | 1.00 | 1.0 |

Dividing by window length makes accounts comparable without knowing their absolute token
capacity, which no subscription reports. Pressure therefore measures **waste risk**, not value:
it assumes every account's window is worth what its pool weight says it is.

### Pressure of a crew

Only **weekly** windows count: Claude `seven_day` and its model-scoped `seven_day_opus` /
`seven_day_sonnet`, Codex's 7-day window (`secondary`, `window_minutes = 10080`) and Muse
`weekly`. Every other window is ignored for pressure, the 5-hour ones (`five_hour`, Codex
`primary`, Muse `window`) included.

1. **The scarcest weekly window binds.** A crew's pressure is the **minimum** over the weekly
   windows that cover it: its account's, and the model-scoped one for its model, if any. Each
   token spent counts against both, so spending a near-empty `seven_day_opus` is not free just
   because `seven_day` has room.
2. **Unknown means neutral.** A crew with no live weekly reading, a provider that reports
   nothing (Gemini, Grok, Antigravity), a weekly reading with no reset, an `error` reading and a
   `ledger` budget all have pressure `1`.
3. **Accounts follow [ORB-17288].** A crew that sets `env` uses only its own account's readings,
   and the other crews use their provider's default login.

The existing usage-limit gate is untouched. It still removes a crew whose window of any length
reaches `provider_limit_max_used_pct`, before pressure weights what is left.

### Effective weight

```
effective_weight = weight × clamp(pressure ^ strength, 1 / cap, cap)
```

- A weight of `0` stays `0`: a parked crew is never revived.
- `cap` (default `8`) bounds how far pressure can move a draw. In the example, Claude's ×8
  against Codex's ×1 turns `opus:50, sonnet:25, sol:25` into 96% Claude (600 of 625 tickets). That moves
  most of the traffic while still exercising the Codex crew.
- `strength` (default `1`) is the exponent. `0` disables pressure, and values above `1` sharpen
  it.
- The draw keeps integer tickets: effective weights are scaled (×1000) and rounded, so
  `weighted_draw` and its seeded reproducibility are unchanged.

### Where pressure applies

Pressure is computed at the draw, from **the readings of the host whose login the run will
spend**, from the same provider-limit observations that gating reads.

1. **Admission re-draws automatic assignments.** With pressure on, a task whose crew came from
   an automatic source (`pool:<complexity>`, `purpose:<purpose>`, `default`) is re-drawn from
   its pool when its run is admitted (`capture_auto_task_selection`). The member chosen at
   creation is no longer kept just because it can run. An explicit crew (`explicit`, a legacy
   pin, an explicit run-input crew) is never re-drawn. The re-draw respects the task's OS
   requirement, the run's crew allowlist, holds and limit narrowing, exactly as today's
   narrowing does. When it changes the crew, it persists the new crew on the task with the same
   automatic source, so the task record, review crew choice and history agree with the run.
   [ORB-17638] already persists a reassignment at this seam for auto-task crews.
2. **Creation still draws.** The creation draw keeps its role: OS routing, `runs_on`, and a crew
   to show before the task runs. It uses pressure too, but admission makes the final choice.
3. **Other pool draws share the seam.** Activity pools, review crew pools, finding-verification
   partitions and agent explore all call `weighted_draw`. They take the same multiplier through
   one helper applied after limit narrowing, so no draw site computes pressure on its own.
4. **Pull drains.** A follower spends its own login ([ORB-16324]). The owner must never weigh a
   follower's run with the owner's readings. See Phasing and Open questions.

### Configuration

```toml
[workflow]
crew_selection = "usage_pressure"   # "static" (default until rollout) | "usage_pressure"
usage_pressure_cap = 8              # 1 disables in effect; bounds the multiplier both ways
usage_pressure_strength = 1.0       # exponent; 0 disables
```

These are `workflow.*` keys and can be overridden per workspace, like `provider_limit_*`.
Readings stay per host.

### Surfaces

- **Run provenance.** `crew_selection.eligible_pool` gains `pressure` and `effective_weight` per
  member, and `source` gains `; usage pressure`, so `orbit run show` explains a draw that went
  against the configured odds.
- **Readiness, doctor and the dashboard limit strip** show each account's pressure next to its
  readings ([ORB-14698]), e.g. `claude ×25.2 (seven_day 10%, resets 18:00)` (raw pressure,
  before any draw cap), plus which window set it.
- **Task history.** An admission re-draw that changes the crew records `crew_redrawn`, the event
  a complexity re-rate already uses, with the old crew, the new one and the pressures that
  decided it.

## Non-goals

- **Changing pool membership or complexity routing.** Pressure never sends a hard task to a
  crew outside the hard pool.
- **Admission order.** Pressure does not change which ready task is admitted next. See Open
  questions.
- **Concurrency.** Admitting more work while pressure is high belongs to
  [auto-concurrency](../auto-concurrency/1_overview.md), not here.
- **Absolute capacity.** No conversion of percentages into tokens across providers.

## Concerns and honest limitations

- **Readings lag.** A window is read only after a run of that account. A drain that admits 15
  Claude tasks in one pass draws all of them against the same reading. The existing limit gate
  bounds the overshoot, and the next reading corrects it.
- **Herding is intended but blunt.** A high-pressure account takes most draws until the limit
  gate removes it. Then the draw falls back to the pool's other members, as today. Under heavy
  load this alternates rather than spreading smoothly.
- **Weekly only.** Pressure ignores how fast the 5-hour windows let an account spend. Ninety
  percent of a week with 6 hours left may not all be spendable. Pressure still points the right
  way, but its size can overstate what is recoverable. This is accepted to keep the model to one
  window per account.
- **Re-drawing changes a long-standing behaviour.** Today a pool task's crew is fixed at
  creation. Operators reading `task.crew` before the run will see it change at admission when
  pressure is on.

## Phasing

1. **Pressure view.** Compute per-account and per-crew pressure from the existing readings, and
   show it in readiness, doctor and the dashboard strip. Selection does not change. This lets an
   operator check the numbers against the provider's own page before they steer anything.
2. **Pressure-weighted task draws.** The `crew_selection` / `usage_pressure_*` keys, the
   multiplier in the creation draw, the admission re-draw of automatic assignments, run
   provenance and history. Default `static`.
3. **Shared seam for other draws.** Activity, review, finding-verification and explore pools take
   the multiplier through the same helper.
4. **Pull drains.** The follower applies pressure from its own readings when it admits a pulled
   leaf, and reports per-crew pressure in its crew window. The owner then offers first the tasks
   whose pools contain the follower's highest-pressure crews.

## Open questions

1. **Admission order.** When the backlog is mostly hard tasks and the pressured account's crews
   sit only in the low pool, should pressure also promote low tasks ahead of hard ones? This
   trades priority for capacity and probably wants its own switch.
2. **Rolling budgets.** Should a `ledger` budget get a pressure, using the share of the budget
   that ages out within the next hour?
3. **Follower offers (phase 4).** Is it enough for the follower to re-draw after the pull, or
   must the owner know follower pressure to choose which task to offer?
4. **Default.** After a trial, should `usage_pressure` become the default in `ws_orbit` only, or
   everywhere?

## Acceptance (feature level)

- With `crew_selection = "usage_pressure"`, a hard pool `opus:50, sonnet:25, sol:25`, Claude
  `seven_day` at 10% resetting in 6 h and Codex weekly at 0% resetting in 7 d, the admission
  draw's `eligible_pool` shows Claude members at ×8 and `sol` at ×1, and `orbit run show` names
  usage pressure in the source.
- A task created with `crew = sol, crew_source = pool:hard` before those readings is re-drawn at
  admission, and its record and history show the new crew. A task with an explicit `sol` crew is
  not re-drawn.
- With Claude `seven_day` 95% used and resetting in 6 days, Claude members draw at ×1/8 and are
  not gated until a window crosses the threshold.
- A crew with no reading, and every crew under `crew_selection = "static"`, draws on its
  configured weight exactly as before. Seeded draws reproduce byte-for-byte.
- Readiness and doctor show each account's pressure and the window that set it.

## Task References

- [ORB-14695] — classifies usage-limit failures and records them in the per-host limit store
- [ORB-14696] — records each provider's reported usage windows after every run
- [ORB-14697] — gates crews at or near their usage limit out of pool draws and admission
- [ORB-14698] — shows provider limits and gated crews in readiness, run show, doctor and the dashboard
- [ORB-16324] — reads a pull executor's limits from its own login
- [ORB-17288] — makes a crew that sets `env` its own provider account
- [ORB-17638] — reassigns an auto-task's limited explicit crew from its complexity pool at admission
- [ORB-17644] — reads Muse usage windows into provider-limit readings

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
