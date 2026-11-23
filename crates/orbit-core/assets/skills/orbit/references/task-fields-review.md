# Task fields: review verdicts, severity and scorecard weights

## Review verdict

`orbit.task.update` accepts `verdict` as an object. MCP advertises the object
without repeating its fields; the host still checks them.

- `decision` is `accept` or `changes_requested`.
- `rationale` is a string.
- `criteria` is an array of `{criterion, met, evidence}`. `criterion` is a
  string, `met` is a boolean, and `evidence` is an array of strings.
- `evidence` is an array of strings.
- `expected_run_id` and `expected_head` are a string or null.

`decision`, `rationale`, `criteria` and `evidence` are required. A review
write also requires `request_id` and `expected_revision` from task show with
`snapshot` true. Snapshot reads accept `fields` or `field` over the snapshot,
including `revision`; a supplied `limit` is ignored and noted. Guarded review
writes also accept `fields` or `field` over a successful result. A revision
conflict, pre-mutation refusal, or accepted write whose refresh failed returns
the complete outcome receipt regardless of projection. Inspect its `conflict`,
`mutation_applied`/`refusal`, or `accepted`/`refresh_error` fields before
interpreting the result or retrying.

Relation types and `regression_from` attribution are in
[Behavior-affecting optional fields](task-fields-relations.md#behavior-affecting-optional-fields).
Status transitions are in [Task lifecycle and listing](task-fields-projections.md#task-lifecycle-and-listing).
Context-file creation intent is in [Context selectors](task-fields-projections.md#context-selectors).

Granted tools, required tools, capability labels and OS tags are in
[Behavior-affecting optional fields: tools and grants](task-fields-tools.md#behavior-affecting-optional-fields-tools-and-grants)
and [Behavior-affecting optional fields: placement and provenance](task-fields-placement.md#behavior-affecting-optional-fields-placement-and-provenance).
Field projection is in [Task projections and delivery evidence](task-fields-projections.md#task-projections-and-delivery-evidence).

## Severity rubric

Review findings and `regression_from` links rate on one scale. Rate by the
worst real impact the evidence shows, not by the size of the fix; when torn
between two levels, take the lower and say why.

- `critical` (weight 8): data loss or corruption, a security hole, every
  drain halted, or silently wrong results. Examples: a migration drops task
  history; a sandbox profile lets an agent write outside its worktree; a
  store lock deadlock stops every drain; a scorecard ranks rows from the
  wrong window without saying so.
- `high` (4): a core workflow broken with no workaround. Examples: every
  delivery of a task tagged `ui` fails at handoff; `orbit task update`
  refuses all relation writes; the dashboard task list never loads.
- `medium` (2): a feature broken or wrong in a real path, with a
  workaround. Examples: `--json` drops a field the plain output shows; a
  filter ignores one of its values; a retry needs a manual requeue.
- `low` (1): an edge case, a misleading message or a minor UX defect.
  Examples: an error names the wrong flag; a 375px layout wraps a label; an
  empty state reads as a failure.
- `nit` (0.25): wording or doc drift, cosmetic, no behavior impact.
  Examples: a stale sentence in a reference doc; a misspelled help string;
  an off-by-one-pixel border.

## Scorecard weights

The grade weights above feed the agent scorecards. The scorecards apply
coefficients to regression rates, not fixed score-point penalties. A held review
finding earns its severity weight.

For Implementers, escaped regressions have a
coefficient of -0.5 on the actual-minus-expected rate, compared with the expected
rate: the rate among the other rows for the same tier and expected-size bucket,
falling back to other rows in the tier, then other rows overall, or zero without
peers. Each escape is weighted by severity. For Reviewers, missed regressions
have a coefficient of -2 on the actual rate; each missed link is weighted by
severity.
