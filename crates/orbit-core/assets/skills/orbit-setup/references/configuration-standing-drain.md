# Configuration: the standing-drain keeper

## Contents

- [Standing-drain keeper](#standing-drain-keeper)

## Standing-drain keeper

The standing-drain keeper is an opt-in job on the workspace's owner host. While
it is enabled, each clock tick can submit one bounded `workspace_auto_pipeline`
drain, through the same path and admission ladder as `orbit run auto`. It admits
no task of its own, and its flags come from the `workflow.standing_drain.*` keys
below, never from the previous run. It is off by default on every workspace and
host: with it off, nothing is submitted and no keeper attention item appears.

Turning `enabled` on, and setting `approve_proposed`, is the operator's decision,
as creating a routine is. `enabled` lets the clock start drains with nobody
present, and `approve_proposed` lets those drains approve work. No agent session
sets either on a live host. Exercise the keeper in a disposable home instead.

Each tick submits only when all of these hold:

- The drain intent is `on`. A hand start (`orbit run auto` from the CLI, the
  dashboard or MCP) records it. A workspace whose intent was never recorded
  (`off`), or whose last hand action was a stop (`paused`), is not acted on, so
  enabling the keeper alone starts nothing.
- This checkout is the owner. A replica never submits.
- No auto drain is pending or running, and the newest one is not `held`.
- A `backlog` or `proposed` task is waiting.
- `min_resubmit_interval` has passed since the newest drain ended.
- The crash breaker is closed.

The keeper never resumes a pause. A hand stop (`orbit run auto --stop`, or the
dashboard's Stop) records the intent `paused`, and only a hand start clears it.
A window end or a crashed drain does not pause: the keeper resubmits after
`min_resubmit_interval`. The crash breaker opens once `max_failed_resubmits`
keeper drains end `failed` or `interrupted` within one hour of each other, since
the last hand start; an upgrade interruption does not count.

Once open, the
breaker stays open however much time passes, and the keeper submits nothing
until a person starts a drain by hand.

| Key | Default | Valid range | Effect |
|---|---|---|---|
| `workflow.standing_drain.enabled` | `false` | `true` or `false` | Lets the keeper submit drains on the owner's clock. |
| `workflow.standing_drain.window` | `"24h"` | `1m` to `24h` | Window of each keeper drain, the `run auto --for` value. Advanced. |
| `workflow.standing_drain.concurrency` | unset | `1` to `64` | Worker ceiling of each keeper drain. Unset uses the drain job's default, as `run auto` without `--concurrency` does; the forecast mode stays `machine.auto_concurrency.mode`. Advanced. |
| `workflow.standing_drain.complete` | `false` | `true` or `false` | Keeper drains land shipped tasks and mark them `done`, as `run auto --complete` does. Advanced. |
| `workflow.standing_drain.approve_proposed` | `false` | `true` or `false` | Keeper drains approve `proposed` tasks that pass the task pilot, as `run auto --approve-proposed` does. Operator decision. Advanced. |
| `workflow.standing_drain.auto_fix` | `false` | `true` or `false` | Keeper drains repair failing checks and conflicts on their pull requests, as `run auto --auto-fix` does. Advanced. |
| `workflow.standing_drain.min_resubmit_interval` | `"10m"` | `1m` to `24h` | Least time from the newest drain's end to the keeper's next submission. Advanced. |
| `workflow.standing_drain.max_failed_resubmits` | `3` | `1` to `100` | Keeper drains that may end `failed` or `interrupted` within one hour before the crash breaker opens. Advanced. |

Read back the effective values with `orbit config get`. Duration keys print in
seconds, so the defaults read `86400` (`window`) and `600`
(`min_resubmit_interval`), and an unset `concurrency` prints `null`. Compare
against the table above.

Enable the keeper, or change a value, from the owner's registered checkout.
`orbit config set` writes that workspace's configuration by default; add
`--global` to write the global file instead:

```bash
orbit config set workflow.standing_drain.enabled true
orbit config set workflow.standing_drain.window 12h               # optional, advanced
orbit config set workflow.standing_drain.approve_proposed true    # only if the operator decides
```

Settings are re-read on each tick, so a change takes effect at the next tick
without a restart.

To turn the keeper off:

```bash
orbit config set workflow.standing_drain.enabled false
```

The next tick stops submitting. A keeper drain already running continues until
its window ends or it is stopped. To stop it and record a pause the keeper will
not resume, run `orbit run auto --stop`; only a hand start (`orbit run auto`)
clears that pause.

Verify the keeper with these surfaces:

```bash
orbit config get workflow.standing_drain.enabled
orbit doctor --check standing-drain
orbit run readiness --json
```

- `orbit doctor --check standing-drain` prints the workspace's `standing-drain`
  row on the owner host (`skipped` on a replica). It names the drain intent and
  how long work waited with no drain admitting it over the last seven UTC days.
  While the keeper is enabled, the row also shows the effective flags, the
  breaker and the last decision.
- `orbit run readiness --json` has a `drain_intent` object (`intent` is `off`,
  `on` or `paused`) and a `drain_keeper` field. While the keeper is off,
  `drain_keeper` is `{"enabled": false}`. While it is on, `drain_keeper` gives
  `enabled`, `flags`, `min_resubmit_interval_seconds`, `breaker` (`open`,
  `failed_in_last_hour` and `threshold`) and `last_decision`. The human output
  adds a `Standing-drain keeper:` line only while the keeper is on.

Each submission leaves a `drain.keeper.submit` audit event. A skip leaves
`drain.keeper.skip` with a `reason` (`intent_off`, `intent_paused`, `drain_live`,
`drain_held`, `min_resubmit_interval`, `no_work`, `breaker_open` or
`submit_refused`), but only when the reason differs from the keeper's previous
decision, not on every tick. List them with
`orbit audit list --tool drain.keeper.submit` or `--tool drain.keeper.skip`.

Two attention kinds report the keeper while it is enabled: `drain_paused`
(medium) when the intent has been `paused` for over 12 hours while work waits,
and `drain_breaker_open` (high) while the breaker is open. Neither is raised
while the keeper is off. See [Attention](../../orbit/references/attention-kinds.md#standing-drain-keeper).
