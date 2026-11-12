# Model ranking

Use `orbit agents rank` when selecting or comparing crews and you need
evidence about the implementer models behind them. The ranking is an
observation about past work. It grants no authority to change crew
configuration, task crew assignments, complexity mappings or dispatch
policy; make any such change only within the operator's existing
authorization and record the reason as for any other crew exception.

## Not the agent scorecards

The ranking and the scorecards answer different questions over different
populations. Do not quote one as the other.

|  | Model ranking | Agent scorecards |
|---|---|---|
| Command | `orbit agents rank` | `orbit agents scorecard --role <role>` |
| One row per | Model, with every crew and recorded effort level that ran it pooled | Model and recorded effort, labelled `model @ effort` |
| Population | Implementer work only | The selected role: `implementers`, `reviewers` or `pilots` |
| Ordering | Tie bands on a qualified board, highest score first within each band; cost and model name break exact score ties | Ranked rows, then below-support or unjudged rows without a rank |

A pooled ranking row still names its contributing crews and, in JSON, an
`effort_split` of completed implementation invocations by recorded effort.
That split describes the pooled row; it does not split the ranking by
effort. To compare one model at two efforts, or one role other than
implementers, use the scorecard instead.

The dashboard reads the same ranking payload at
`GET /api/agents/rank`, which accepts the same `window`, `until`,
`settle_lag`, `execution` and `live` selections as the CLI. There is no
separate ranking to reconcile between the two surfaces.

## Read the board

Start with the settled board, then narrow the selection only when the
question requires it:

```bash
orbit agents rank
orbit agents rank --json
orbit agents rank --window 90d --json
orbit agents rank --window 30d --until 7d --json
orbit agents rank --window 30d --execution all --json
orbit agents rank --live --json
orbit agents rank --help
orbit agents rank calibrate --help
```

| Selection | Default | Meaning |
|---|---|---|
| `--window <WINDOW>` | `30d` | Length of the ranked window, such as `7d` or `30d`. A time window is required; a count window such as `last500` is refused. |
| `--until <WHEN>` | now | The reference end: an RFC 3339 instant or a duration ago such as `7d`. The ranked window is `[until - settle-lag - window, until - settle-lag)` unless `--live` is set. |
| `--settle-lag <DURATION>` | `14d` | How far before `--until` the ranked window ends, so late escaped regressions have had time to arrive. |
| `--execution <SCOPE>` | `managed` | Which implemented tasks count by derived execution: `managed`, `all` (managed plus interactive) or `interactive` alone. Unclassified tasks count in no scope. |
| `--live` | off | Rank the window ending at `--until` instead of the settled cutoff. The result is unsettled and provisional. |

Execution is derived from a task's runs; it is never set by an agent.
The default `managed` scope is the board to quote for delivery work.
Widen to `all` only when interactive implementation work is part of the
question, and say so when reporting the result.

Settled is the default because escaped regressions surface after
landing. Treat a `--live` board as an early look: it includes work that
has not had the settle lag to show its regressions, and JSON marks it
`"live": true` and `"provisional": true`. Never present a live rank as
settled evidence, and never calibrate from it.

## What each row measures

Every measure compares a model's actual count with what the fleet
achieves on the same task tiers, divides by the season's noise scale,
and weights the result. The weighted composite is shrunk toward the
other eligible models (`K = 30`) for `score`. The unshrunk `raw_score`
is 100 plus the sum of the measured per-measure contributions, subject
to rounding. Measure columns and JSON `contribution` values are points
before shrinkage; `score` is the shrunk value and can differ from
`raw_score`.

| Measure | Weight | Units | Reading |
|---|---|---|---|
| Escaped regressions | 30 | Implemented tasks | Severity-weighted regressions that escaped, deduplicated against pre-landing review charges, against the size-adjusted expectation. Fewer than expected helps the score. |
| Review outcome | 20 | Reviewed deliveries | `+1` per first-pass accept, `-1` per reject and `-0.5` per accept-with-fixes, against the same combination of tier expectations. |
| Implementer CI failures | 10 | Implemented tasks | Required-check failures on the model's candidate heads classed as code failures. |
| Implementer-fault final recoveries | 10 | Implemented tasks | Final-recovery decisions whose fault is the implementer. |
| Crew-caused rework | 5 | Implemented tasks | Rework caused by the crew, already leaving out review verdicts. |
| Tool-call errors | 5 | Tool calls | Tool calls in the model's implementing activities classed expected or denied. |

A measure Orbit does not record yet is `unmeasured`: it is shown with
its reason, contributes no column to the text board, has a null
`contribution` in JSON, and leaves the weight denominator — it neither
helps nor harms any model. A recorded measure that never varied in the
calibration sample is `no_scale` and also leaves the denominator. Read
`weights_measured` on each row before comparing scores across boards:
two boards whose measured weights differ are not scoring the same
evidence. Which measures are unmeasured can change as recording lands,
so read the current board's `measures` list rather than assuming the
set above is still the measured one.

Failure evidence is charged once, in this order: reviewer verdict,
CI failure on a candidate head, final recovery, then crew-caused
rework. The ranking regroups the implementer scorecard's counts and
expectations; it never re-attributes them.

## Eligibility, sparse evidence and board qualification

A model is eligible only when all of these hold:

- at least 30 implemented tasks in the window;
- at least one assessed tier with at least 5 tasks in it
  (tasks in the `unassessed` tier do not count toward this);
- at least one measure with units and a noise scale, so a composite
  exists.

An eligible model spanning exactly one assessed tier is single-tier: its
row's `single_tier` names that tier (`single tier: <tier>` in the text board
and beside the model name in the dashboard Rank table, the tier name or
`null` in JSON). It is a flag, not a scoring change.

Tasks whose runs recorded no model are unattributed: they count in the
fleet expectation only, are reported in the payload's `unattributed`
count, and never form a row.

An ineligible model still appears with its task count, score and
`ineligible_reason`, but with no rank and no uncertainty interval. That
is deliberate: sparse or narrow evidence is shown, not ranked. Do not
promote an ineligible row by its score, and do not treat a missing
model as a bad model — it may have too few tasks, too narrow a tier
spread, or only unattributed work in the window.

The board itself is qualified only with at least three eligible
models. Below that, JSON reports `"qualified": false` with the eligible
count, and the text board states that no bands or ranks are awarded.
Scores on an unqualified board are descriptive, not a ranking.

## Tie bands, uncertainty and cost

On a qualified board, models whose scores are not distinguishable
share a band. A model starts a new band only when the model above it
beats it in at least 95% of the paired bootstrap draws
(`paired_probabilities` in JSON). Bands take dense ranks — `1`, `2`,
`3` however many models share a band — and a shared band is labelled
with `=`, such as `2=`. Resampling is within each model's own tiers,
with expectations and scales held fixed, over 1000 draws.

Each eligible row's `interval` is the bootstrap `p10`–`p90` score
range, printed as `P10-P90`. Overlapping intervals are consistent with
a shared band; the paired probability, not the interval overlap alone,
decides the band. An ineligible row has no interval.

Within each band, models are ordered by score, highest first. When scores
are exactly equal, known cost ratios sort cheapest first, then model name;
unknown costs follow priced rows and sort by model name. Cost remains
visible per row, and band order is no longer a cost recommendation. The
JSON `cost` object reports:

- `per_task`: the model's recorded cost per priced task;
- `expected_per_task`: the fleet's cost per task on the same tier mix;
- `ratio`: `per_task ÷ expected_per_task`, so `1` costs what the fleet
  costs on that mix, below `1` less and above `1` more;
- `coverage`: the share of the model's tasks with a recorded cost;
- `cost_unknown`: true when coverage is below 0.8 or no ratio exists.

Cost never enters the score and never changes band membership. A model
with a higher score can appear before a cheaper model in the same band;
the row order does not make it a routing recommendation. A cheaper model
in a lower band has not beaten the band above it.

## Verify before quoting JSON

Run with `--json` and check the header before quoting any row:

```bash
orbit agents rank --json
```

- `since`, `until` and `requested_until`: the window actually served.
  `until` is the settled cutoff (or `--until` itself when live);
  `requested_until` is the `--until` value before the settle lag.
- `settle_lag`, `live` and `provisional`: confirm a quoted board is
  settled (`live` false) unless you are deliberately reporting a live
  one.
- `execution`: the derived-execution scope the counts used.
- `qualified` and `eligible_models`: whether ranks exist at all, and
  how many models earned them.
- `scales_source` and `season`: `season` with a season number when a
  calibrated season's scales are in force, or `window_estimate` with a
  null `season` when the scales were estimated from this window.
- `measures`: each measure's `status` (`measured`, `unmeasured` or
  `no_scale`), `weight`, `scale` and `reason`; then each row's
  `weights_measured`, per-measure `status`, `actual`, `expected`,
  `residual` and `contribution`.
- Per row: `eligible`, `single_tier` and `ineligible_reason`, `units`,
  `tasks_by_tier`, `crews`, `score` against `raw_score`, `rank`,
  `rank_label` and `band_size`, `interval`, and the `cost` object.
- `unattributed.tasks`, `execution_split` and `not_scored`: work the
  board saw but did not score, which bounds what the ranking can say.

If a field disagrees with the story — a rank quoted from a board with
`"qualified": false`, a settled claim from a `"provisional": true`
payload, or a comparison across different `weights_measured` — the
payload is right and the story is wrong.

## Calibration is a separate operator write

```bash
orbit agents rank calibrate
orbit agents rank calibrate --window 90d --until 7d --json
```

`rank calibrate` freezes a new season's noise scales from the settled
window and persists it in the workspace's state. It appends a season;
it never edits an earlier one. Later boards use the newest season
calibrated under the current measure definition and report it as
`"scales_source": "season"`. Calibrating again appends the next
season rather than replacing the last.

Calibration is not a read-only prerequisite and is never needed before
reading a board: with no season on file, the ranking estimates scales
from the ranked window and says so. Run it only when an operator
authorizes that persistent write — for example to hold scales steady
across a period whose boards will be compared. It refuses a `--live`
selection and a window with no implemented work; widen `--window` or
move `--until` and retry only under the same authorization.

There is no `rank --freeze` and no snapshot command. `calibrate` is
the only ranking write, and the settled/`--live` selection is how a
board's window is chosen. Do not teach or script any other spelling.

## Using a board to select a crew

1. Read the settled, default-scope board first; note its window,
   measured weights, qualification and scales source before any row.
2. Map models back to crews through each row's `crews` counts. A model
   pooled across crews is evidence about the model, not about any one
   crew's configuration or effort choice.
3. Prefer a model in a higher band over one in a lower band only when
   the board is qualified and both are eligible. Within a band, treat
   the models as indistinguishable on quality and reliability. Read each
   row's cost as a visible trade-off; the within-band row order is not a
   cost recommendation.
4. When the preferred model is ineligible, unmeasured on the measures
   that matter, or absent, say the evidence is insufficient rather
   than substituting its score. Fall back to the workspace's configured
   complexity mapping and the operator's cost and provider constraints.
5. Record the board you read (window, execution scope, settled or
   live, scales source) with any crew exception, so a later reader can
   reproduce it. The observation alone does not authorize the change.
