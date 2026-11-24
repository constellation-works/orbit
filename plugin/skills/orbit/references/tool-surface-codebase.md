# Tools: codebase analytics

Contents:

- [Codebase analytics and path detail](#codebase-analytics-and-path-detail)

## Codebase analytics and path detail

Use `orbit codebase analytics` (or MCP `orbit_codebase` action `analytics`,
opt-in via `--toolset codebase` or `machine.mcp.toolsets` in the operator
profile and included in `agent` and `reader`) to compare repository paths across
the codebase risk rankings.

Pass a `path` returned in a ranking row to
`orbit codebase show <path>` (or `orbit_codebase` action `show`) to inspect that
path's standing in every ranking and follow its counts to tasks, regression
sites, lock waiters, or coupled paths.

Both tools use the same selection: `window` (`7d`, `30d`, `90d`, or `all`,
default `90d`), `min_support` (default `15`: minimum denominator for the rate
rankings, minimum shared-reservation count for change coupling) and `rollup`
(`file`, parent `dir`, or `crate`, default `file`). `touched` means the task's
declared footprint (`context_files`, including widened selectors), not the
landed diff.

At file rollup, a path absent from the current checkout has `absent_at_head: true`
in its row and in `show`; coupling pairs list missing members in
`absent_paths_at_head`. CLI text marks those paths. Counts and history stay
unchanged; use `--rollup dir` to inspect directory history. Directory and crate
rollups have no absence markers.

Analytics returns six path rankings and an `overlap` summary. Each rate row
includes its numerator and denominator. Rankings use distinct tasks as their
counts, except lock contention, which counts distinct requesting identities,
and change coupling, which counts reservations.

The rate rankings keep rows
with a positive numerator and at least `min_support` denominator support, are
sorted by rate then numerator then path, and analytics returns at most 25 per
ranking; `blocked` additionally keeps a supported row whose attributed
numerator is zero when the path has unclassified blocks, ranking at rate zero
so that bucket stays visible per path.

Change coupling instead keeps pairs
with at least `min_support` reservations naming both paths, regardless of the
Jaccard `either` denominator, sorted the same way with the shared count
standing in for the numerator. `below_support` counts rows omitted for low
support (pairs below shared-count support for coupling). For the rate
rankings, `show` reports the path's 1-based rank over the full ranking and
whether its denominator is below support; a missing ranking row is `null`.

The ranking sections mean:

- `regressions`: done tasks with an incoming regression link whose recorded
  site attributes the task to the path, divided by done tasks whose declared
  footprint touched it. A task attributed by a site outside its footprint is
  added to that path's denominator, so its rate stays at most 1. Recorded
  directory sites cover their descendant keys. The response includes task
  counts, sites, and the link breakdowns described below.
- `footprint_misses`: tasks whose declared footprint was widened onto the
  path during the window, divided by active tasks whose footprint touched it.
  `widenings` counts widening events, while the rate counts each task once.
- `blocked`: tasks blocked by a run failure or recorded base conflict on the
  path, divided by active tasks whose footprint touched it (plus any
  conflicting paths outside that footprint). Unclassified blocks are reported
  separately, including per-path counts; infrastructure, manual,
  base-conflict, run-failure, and non-failure totals are also exposed.
- `rejected`: tasks rejected after a job run, divided by tasks with a run
  whose footprint touched the path. Proposal-stage rejections without a run
  are excluded.
- `lock_contention`: distinct requesters refused a lock on the path divided
  by distinct requesters with a reservation or refusal there. The response
  includes reservation and denial counts, wait summaries, and a second list
  ranked by total wait time.
- `change_coupling`: pairs of paths reserved together, ranked by Jaccard rate
  (`reservations naming both ÷ reservations naming either`). Pair support is
  the number of reservations naming both, and `min_support` gates that shared
  count rather than the `either` denominator. Reservations covering more than
  12 files are excluded.

`overlap` lists paths appearing in at least two of the first 25 rows of
`regressions`, `footprint_misses`, `blocked`, and `rejected`, plus each path's
rankings and its highest non-rejection rate. `ranked_paths` is the number of
distinct paths across those four lists.

Regression attribution and filters apply to `regressions`, its path detail,
and the crew and complexity rates. A link's `nature` and `escape` come from
its recorded classification; a missing classification is `absent`, distinct
from an explicit `unclassified`.

`layer` describes where the fix landed,
using landed paths or, when unavailable, its declared footprint, classified by
the workspace's `workflow.regression_layers` (`ci_tests` and `docs_skills`
unless configured); a path no row matches is `other`. `detected_by`
comes from the regression task's tags or its filer. Filters `nature`,
`escape`, `layer`, and `detected_by` combine: every selected dimension must
match. `absent` selects links without a recorded classification.

The regression section's `breakdowns` count links by nature, escape, layer,
and detector while retaining filters on the other dimensions. `cross_tabs`
contains link counts for nature × escape and escape × layer under the full
filter. `by_crew` and `by_complexity` are rates of done tasks with a matching
regression link within each crew or complexity group; missing values appear
as `unassigned` and `unset`.

Each path row also has per-crew numerator,
denominator, and rate. A regressed task entry includes its crew, complexity,
regression task IDs, matching regression links, and sites attributing it to
that path. `unsited` counts regressed tasks and links whose kept links have no
valid recorded site; they are not attributed across the task's footprint.

In `show`, each task-rate ranking entry carries its rate, numerator,
denominator, rank, and support status, plus the evidence behind its numerator.
The regression detail lists the recorded site selectors under the path and
their regressed and regression tasks. Its `unsited` task count is limited to
regressed tasks that declared the path but have no site attributing them to
any path.

Lock detail lists refused requesters and wait episodes.
`change_coupling` reports the path's reservation count and ranked pairs; each
pair includes its Jaccard rate, rank, reservation counts for both paths, and
requesters. The same four regression filters narrow regression counts and
drill-downs; they do not change the other rankings.

`orbit_run` action `show` and each `orbit_run` action `list` item are
operator-only (denied for agent sessions) and carry the same run projection
as `.run` in `orbit run show --json` and the dashboard; a session with a
shell on the owning host reads that projection with `orbit run show --json`:

- state and timing,
- `error_code`/`error_message` from the step that explains the
  outcome,
- `root_cause` for a run that failed only because a child did,
- `run_role`, crew, `trigger`, `child_dispatches` and drain controls.

They add
the submitted `input`, the worker's `pid`/`pid_start_time`, `drain_last_pass`,
`drain_approvals` and `recovery_attempts`; `show` also adds
`activity_provenance` and `execution_progress`.

`steps` are the steps every
run view displays, and `steps_source` says whether they came from the run
record or, when the record is empty or holds only a run-level step, its audit
trail. A step still open when its run ended reads as the run's terminal state
with `outcome: "interrupted"`, never `running`.

Provider/gateway prefixes are transport wrappers around these names. A connected
server may expose additional tools; use its advertised schema rather than
assuming every installation has that extension.

Bare `orbit mcp serve` and ordinary `orbit mcp init` integrations have agent
capability. `orbit workspace init --mcp` deliberately installs an operator
integration. Governed workflow and command operations need operator authority;
a managed worker cannot dispatch/resume another workflow or use operator command
execution. An allowlisted tool still has to pass runtime policy, filesystem,
subprocess, and external authentication checks.
