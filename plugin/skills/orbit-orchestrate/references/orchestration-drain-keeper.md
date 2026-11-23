# Operate the standing-drain keeper

## Contents

- [Standing drain intent and keeper](#standing-drain-intent-and-keeper)
  - [The keeper](#the-keeper)
  - [Attention items](#attention-items)
  - [Mixed versions](#mixed-versions)

## Standing drain intent and keeper

Each workspace records one drain intent: whether its drain was last started or
stopped by hand. Only the optional standing-drain keeper reads it. With the
keeper off (the default), the intent changes no drain behaviour.

| Intent | Recorded by | Meaning |
|---|---|---|
| `off` | nothing yet | No drain has been started or stopped by hand since the intent existed. |
| `paused` | `orbit run auto --stop`, the dashboard's Stop, the desktop app's Stop, the `orbit.workflow` auto stop | The last hand action was a stop. Records who stopped it, from which surface, and the reason if one was given. |
| `on` | `orbit run auto` from the CLI, the dashboard or MCP, and `orbit run auto --pull` on a replica | The last hand action was a start. A drain a routine submits records nothing. |

- A stop records `paused` even when no drain was live.
- Only the `orbit.workflow` auto stop (and the desktop app's Stop, which uses the same handler) takes `force`, which also cancels live drains. `force` never changes whether the pause is recorded. `orbit run auto` has no `--force`, and `orbit run cancel --force` records no pause.
- If the intent write fails, the stop or start still succeeds and the failure is logged.

### The keeper

Enabling the keeper is the operator's decision, made in configuration:
[Standing drain keeper](../../orbit-setup/references/configuration-standing-drain.md#standing-drain-keeper).
Do not turn it on for the operator, and do not turn it on to test on a live host.

With the keeper enabled, each clock tick on the owner host decides whether to
submit one bounded drain. It submits only when all of these hold:

- the intent is `on`, never `paused` or `off`;
- this checkout is the owner, because a replica never submits;
- no workspace auto-pipeline run is pending or running, and the newest is not held;
- a `backlog` or `proposed` task is waiting;
- `min_resubmit_interval` (default 10 minutes) has passed since the newest drain ended;
- the crash breaker is closed.

The drain goes through the same submission path and admission ladder as
`orbit run auto`, with flags from configuration. Its trigger is
`standing-drain-keeper`, which `orbit run history` shows. Submitting it leaves
the intent unchanged.

What this means for an orchestrator:

- **A hand stop pauses the keeper until a hand start.** The keeper never
  resumes a pause. After `orbit run auto --stop`, no keeper drain starts until
  someone starts a drain by hand with `orbit run auto`.
- **A window end or a crashed drain does not pause.** The keeper resubmits
  after the interval.
- **The crash breaker latches.** When `max_failed_resubmits` (default 3) keeper
  drains end `failed` or `interrupted` within an hour of each other, counted
  since the last hand start, the keeper submits nothing however much time
  passes. Upgrade interruptions do not count. Only a hand start closes it.
- A live keeper drain runs until its window ends or it is stopped by hand.

To verify, read `orbit run readiness --json`:

- `drain_intent`: `intent` (`off`, `on` or `paused`), and once recorded `by`,
  `source`, `at` and `reason`. An unreadable row reports `intent: "unknown"`
  with `error`, never `off`. The human output prints a `Drain intent:` line.
- `drain_keeper`: `{"enabled": false}` while the keeper is off. While it is on:
  `flags` (`window_seconds`, `concurrency`, `complete`, `approve_proposed`,
  `auto_fix`), `min_resubmit_interval_seconds`, `breaker` (`open`,
  `failed_in_last_hour`, `threshold`) and `last_decision` (`decision`,
  `detail`, `at`). The human output adds a `Standing-drain keeper:` line.

Each keeper decision leaves an audit event: `drain.keeper.submit`, or
`drain.keeper.skip` with a `reason` of `intent_off`, `intent_paused`,
`drain_live`, `drain_held`, `min_resubmit_interval`, `no_work`,
`breaker_open` or `submit_refused`. A skip is recorded only when its reason
differs from the keeper's previous decision.

### Attention items

Both kinds appear only while the keeper is enabled. Detail is in
[attention](../../orbit/references/attention-kinds.md#standing-drain-keeper).

- **`drain_paused`** (medium): the intent has been `paused` for over 12 hours
  while `backlog` or `proposed` work waits. First, read `drain_intent` (`by`,
  `source`, `reason`). A stop is a user intervention: if it was deliberate,
  report it and leave the drain paused. Start a drain by hand
  (`orbit run auto --for <window>`; the item's `next` suggests `--for 24h`)
  only when the operator has asked for work to run again.

  The start lifts the
  pause, and the keeper resumes on its next eligible tick.
- **`drain_breaker_open`** (high): the keeper has stopped submitting because
  its recent drains kept failing, and it stays open until a hand start. First,
  find why those drains failed: `orbit run history` shows the
  `standing-drain-keeper` runs, `orbit run show <run-id> --json` gives each
  one's state and error, and [run debugging](run-debugging-triage.md) matches them to
  a remedy. Start a drain by hand only after the cause is fixed or reported to
  the operator.

  - The hand start closes the breaker, and a failure that recurs
    can trip it again within the hour.

### Mixed versions

Drain intent needs Orbit 0.52.0 or later on the owner and on every host that
starts or stops drains (`orbit --version`). Older binaries read the store but
ignore the drain-intent table. A stop from an older host records no pause, so
the owner's keeper keeps submitting, and an older binary's readiness output has
no `drain_intent` or `drain_keeper`. Check every host's version before relying
on a pause.
