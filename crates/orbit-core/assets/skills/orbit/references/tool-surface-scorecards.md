# Tools: agent scorecards and CI failures

## Agent scorecards

`orbit_agents_scorecard` ranks one agent role's models by recorded reasoning effort over the window: one row per model and recorded effort, labelled `model @ effort`. Over
MCP it is in the `analytics` toolset.

- **Score.** Each term's count per 100 units of work is weighted and summed.
  Implementers start at 100 and score actual counts against fleet expectations
  for the same tier mix; other roles sum weighted actual rates. A row above the
  fleet average is pulled down toward it. Tasks tagged `no-diff-expected`, when
  an attempt was admitted, are not implementer work and count nowhere on the
  implementers scorecard.
- **Ranking.** Each row lists the crews that contributed its units (`crews`, with `crew` kept as the first of them). Two efforts of one model are two rows, however many crews ran them; one crew that ran a model at two recorded efforts contributes units to both rows and is listed under each.
  - Rows below the minimum support are listed last without a rank. A
    reviewer row none of whose work is judged yet is `unjudged`: no rank, null
    `raw` and `score`, listed after every other row and left out of the fleet
    average. A role whose terms are not computed yet answers
    `status: not_computed`, with its terms and no rows.
- **Switches.** `by_type` returns one cell per (model, recorded effort, tier, type) with
  `n`, `raw`, `crew_mean` and `score`, plus the type list used; each cell carries the row's `crews`, `model`, `reasoning_effort`, `effort_label` and `label`.
  - `per_task` returns one row per implemented task
    and the model-and-effort row that worked it, with its crews, model, `reasoning_effort`, tier, configured type, tags, context-file selectors, path
    prefixes and per-term counts, plus the type list used. Without these switches,
    the ranking is returned.
- **Execution.** Each task's execution is derived from its runs, never set by
  an agent: `managed` when a job (a local delivery run or a pulled claim)
  worked its final implementation attempt, `interactive` when it reached review
  or done without one, `unclassified` otherwise. Implementer counts read managed
  tasks by default; `execution: all` adds interactive tasks and
  `execution: interactive` shows them alone.
  - Unclassified tasks count in
    neither. Cost and run time count managed tasks whatever the scope, and the
    implementers header's `execution_split` lists each model-and-effort row's managed,
    interactive and unclassified tasks (with the row's contributing crews and the interactive ones' landed and
    regressed counts). Other roles ignore the scope.
- **Recorded effort.** Each ranking/detail row includes `effort_breakdown` of
  completed invocations by immutable dispatch crew, model and reasoning effort,
  with counts, linked tasks, duration, tokens and provider-reported cost.
  The dashboard detail header and `orbit agents show` label that list as
  completed invocations and name each contributing crew, so another effort's
  invocations are not read as the selected row's scored units.
  - A rendered effort is its wire name (`low`, `medium`, `high`, `xhigh`, `max`);
    a present dispatch identity with no rendered effort is `null` and is labelled **provider default**;
    a historical invocation with no dispatch identity is `"unknown"` and is labelled **effort unknown**.
    Units with unknown effort form a separate effort-unknown row per model and are never folded into a known effort.
  - Scores retain their model-and-recorded-effort population. The top-level breakdown covers
    completed workspace invocations in the selected window; `effort_population`
    identifies that separate descriptive population. Never infer
    historical effort from the crew's current configuration or an audit event.
  - To follow a row to its tasks, pass its crew with `crew` and narrow an ambiguous
    crew-and-model selection with `row_effort` (`--effort` on the CLI, the `effort`
    query parameter on the dashboard API: a wire name, `provider_default` for a null
    effort, or `unknown` for unrecorded work); `row_model` (`--model` on the CLI)
    narrows by model.
  - Experiment arms break down invocations linked to their enrolled tasks.
    Run show exposes `agent_executions` on the run and displayed steps, captured
    before launch so interrupted runs remain inspectable.
- **Window end.** Optional `until` is an RFC 3339 instant or a duration ago (for example `30d`; units `m`, `h`, `d` or `w`). The window is then `[until - window, until)`, and a count window (`last50` and the other lastN values) takes the newest that many attempts before `until`.
  - Omitted, the window ends now. A future instant is refused. The payload's `since` and `until` are the served bounds; when `until` was omitted, `until` is the instant used as now. Reviewer provisional counts stay measured against now.
- **Cost.** Each call scans the workspace once (`cache_ttl_seconds` is 0).
- The dashboard's Agents view reads this payload.

## CI failures by cause

`orbit_agents_ci_failures` (`orbit agents ci-failures`, `GET /api/agents/ci-failures`)
lists the required-check failures the owner's landing recorded on candidate heads: one
record per head and check with the task, the run that produced the head
(`producing_run_id`), the check, its error signature and a cause `class`:

- `code`: the failure cites a file the head changed (its log, or the failure
  annotations of a check outside a workflow), or the same head failed the same
  check again on a rerun;
- `flaky`: the same head later passed the same check, with no new commit;
- `infrastructure`: the runner, the network or a declared
  `workflow.ci_infrastructure_signatures` entry failed it, or the base fails
  the same check;
- `unknown`: none of these was established.

A candidate failure with no evidence of its cause, on a head that has had no rerun, is
rerun once on the unchanged head before the landing repairs it, so the second result
can class it. Later evidence upgrades only an `unknown` class; each change is listed in
the record's `changes` with its evidence, and is a `ci_failure_reclassified` event in
the task's history.

Filters: `window` (`all` or a duration such as `14d`; default
`30d`, by when the failure was first recorded), `until`, `task` and `class` (the
current class). The response lists the records newest first, with per-class `counts`
over the window and task whatever `class` selects.
