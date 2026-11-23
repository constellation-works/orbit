# Read the backlog forecast

## Contents

- [Backlog forecast](#backlog-forecast)

## Backlog forecast

The drain card's **Backlog forecast** line estimates how long the workspace's
startable backlog could take to drain and what the full backlog could cost. It resamples delivery
history and simulates the drain's admission behavior: dependencies, file locks,
dispatch order, CPU and memory fit, blocked, failed and retried runs, review
modes, and expected regression rework. The line shows p50–p90 time to drain and
estimated cost.

Open **Analytics › Forecast** for token and exact-cost ranges,
backlog and in-progress counts, assumptions, and how earlier forecasts scored
against actual outcomes.

Time to drain excludes tasks whose effective `not_before` is still in the
future and tasks admission excludes with `no-drain`. The Forecast task table
shows scheduled work as **starts YYYY-MM-DD**, and **Outside this drain** lists
scheduled starts and exclusion reasons separately; the drain card summarizes
those holds too. Their own work hours, tokens and cost remain in spending
totals, without occupying simulated slots or extending time to drain.

Their
regression follow-up is outside this drain. Already-running tasks remain in
the drain even if their tags or scheduled date later change. Operator schedule
overrides are respected. A dependency on held work remains unforecastable
until that work becomes startable or finishes. Model 19 separates these drain
and spending populations; calibration keeps earlier model versions apart.

This is separate from the [`machine.auto_concurrency` forecast](orchestration-dispatch.md#drain-admission-and-capacity). That
admission forecast checks resource fit for the next admission and can affect
which work proceeds when enforcement is enabled. Backlog Forecast estimates
the whole drain's time and cost; it does not decide or change live admissions.

Exploring scenarios or switching tabs starts, stops, prioritizes, or admits
no work: dispatch authority stays with the ship and drain commands named in
[Entry points](orchestration-dispatch.md#entry-points), and admission stays with the drain's
ceiling and [readiness](orchestration-readiness.md).

A switch above the panel picks **Backlog forecast** (this section) or
**Resource forecast** ([resource view](orchestration-resource-forecast.md#resource-forecast-view)). Each view reads only
when first shown and on its own refresh, so switching never refetches the
other. The Backlog forecast has three tabs, each with its own address.
Switching tabs only shows another tab: it never refetches and changes nothing.

- **Forecast** (`#analytics/forecast`): the totals, the scenario switch, the
  task table in dispatch order, scheduled and excluded work with its reasons,
  and the collapsed wait breakdown.
- **Accuracy** (`#analytics/forecast/accuracy`): how earlier forecasts scored.
  The calibration card compares actuals against p50 and p90 for task hours,
  task cost, task tokens, and the whole backlog (about half of actuals under
  p50 and nine in ten under p90 when well calibrated), with the newest earlier
  model's task-hour score below the table. Residual charts, the stored
  forecasts, and the method note (which tasks were sampled, since when) sit
  behind it.
- **Data gaps** (`#analytics/forecast/data-gaps`): one sentence on where the
  figures stand and why, then only the coverage checks that are not zero,
  grouped into **This backlog** and **History** cards with a count and a
  definition behind each info icon. The tab label carries the open-check
  count, and the header says how many other checks are clear.

  - A collapsed
    **Details** section holds the unavailable-forecast note with its evidence,
    a partial-history note when historical checkpoints could not be read, and
    the full coverage checklist of every check.

The **Scenario** switch on the Forecast tab chooses which tasks the figures
forecast:

- **Current backlog** (default): the backlog and in-progress tasks, assuming
  no new tasks arrive.
- **With N proposed**: the same tasks with the N proposed tasks approved, from
  the response's if-approved evidence. The time-to-drain tile notes the p50
  change against the current backlog. The choice is disabled while no
  proposed-work forecast exists (for example, nothing is proposed in this
  workspace), and a proposed selection the response cannot back reads as the
  current backlog.

  - Under **With new arrivals** the time-to-drain tile still
    carries the if-approved figures, since that read includes them.
- **With new arrivals**: adds new tasks arriving at the recent rate, refetching
  for that assumption. The info note beside the switch states the assumption
  with the recent filing and completion rates. An arrival sample missing
  tokens or cost leaves that simulation's total for that measure unavailable
  (never zero); time to drain and the other measure stay independent. A
  simulation that reaches a bound withholds its totals and points back to
  Current backlog.

Every total is one p50–p90 range: a missing percentile reads **unknown**, a
total with neither reads **Unknown**, never zero. When the regression-adjusted
forecast is unavailable, the tiles show the labelled pre-regression baseline
instead, with one warning line and a **Why** link into Data gaps; with a
proposed scenario the warning names that scenario's reason.

A task that cannot
be scheduled says why in place of its start, and unforecastable dependencies
withhold the full totals in favor of the schedulable subset.

To verify the card, use a single-workspace dashboard, open its Tasks drain card,
then follow the forecast link to **Analytics › Forecast**. The header names the
source: **Stored** with the snapshot's age when the default view is served from
the newest stored snapshot, or **Live** when computed for the read (hover for
the computation time); **Refresh** recomputes live. The card uses the
default read: a 30-day history, no new arrivals, and the drain's leaf-run
ceiling.

The default read may be served from the newest stored snapshot while
it is under 120 minutes old, so that ceiling is the one the drain had when the
snapshot was stored. `refresh=1`, other inputs, or no snapshot that recent
compute the read live, with the live ceiling. Only this default read is stored
and scored: the **History** select and **With new arrivals** refetch, and the
**With N proposed** figures come from the same response's if-approved evidence
without refetching.

The switch's pressed button names the displayed scenario,
with the proposed count in its label. The diagnostics read is available
at `GET /api/diagnostics/backlog_forecast`; `lookback_days`, `concurrency`, and
`arrivals=on` can change its assumptions, so omit them to compare with the
card. The aggregate dashboard has no
workspace-specific forecast.
