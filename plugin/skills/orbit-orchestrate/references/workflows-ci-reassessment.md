# Reassess a completed CI repair

## Completed CI repair reassessment

When open-owner lookup misses, filing examines at most eight completed exact-key
owners and attempts at most 32 assessments per snapshot. Metadata lookup is
bounded before task hydration. Each owner may carry `ci-repair-assessment.json`
(schema version 1) in its existing task artifacts. Completion, ancestry, a shared
key/test/path, task summaries and pilot rationale are discovery hints; none is
coverage proof by itself.

The introducing seam was the original filer's exclusion
of completed owners, preserved by the later shared open-owner lookup.

The assessment contains these required fields:

- `schema_version: 1`, `task_id`, `failure_key`, `delivery_run_id`,
  `delivery_step_index`, and `landed_revision` (full commit SHA).
- `observations`: at most 32 records with string `run_id`, `job_id`, `checkout`,
  `diagnostic_sha256`, `branch`, and `ref_kind` (`integration` or `release`). The
  digest is full SHA-256 of the selected diagnostic unit's exact UTF-8 bytes,
  or the complete untruncated excerpt when no selected unit exists. No signature
  normalization is applied.

  - Source run/job/checkout and diagnostic bytes must
    match the collector snapshot, including retained assertion details and omission
    accounting enforced by the collection contract.
- `before` and `after`: references `{path, sha256}` to validation JSON artifacts
  on that same owner. Paths are relative to its artifact bundle; digests bind
  the exact file bytes. Artifacts are limited to 1 MiB each.
- `command`: the same nonempty argv array in both validation records;
  `diagnostic_details`: concrete assertion/error substrings present in every
  observed diagnostic and failing validation output, absent from passing output;
  `coverage_reason`: why the repair fixes those particular details.

Each validation artifact has `schema_version: 1`, `task_id`, `revision`,
`command`, `exit_code`, `outcome`, `origin`, `recorded_at` (RFC3339), and captured
`output`. The before result must be `failed` with nonzero exit at the observed
checkout; the after result must be `passed` with zero exit at exactly the landed
revision. Both must identify the same owner and command. `origin` is `recovered`
for retained execution evidence or `retrospective` for a newly executed check.

An assessment never executes argv or instructions copied from a log.

The owner's PR delivery run must exist, succeed, match its recorded `job_run_id`,
and assign that owner in its original `task_ids`. The referenced successful
completion step must have `phase: complete`, `merge.merged: true`, the same
`merge.landed_commit`, and that owner among `completed_task_ids`. Aggregate
pipeline fields or agent result prose cannot substitute for that step. Git must
confirm that the observed checkout strictly predates the landed revision.

The collector's observed branch head and the
available local remote-tracking (or local) branch must both contain the repair.
Git inspection reuses the bounded source reader (two seconds per command, thirty
seconds total). Missing objects or branch refs are unavailable evidence, never
an inferred ancestry success. A post-fix recurrence, changed diagnostic, or branch
without the repair therefore remains actionable.

Existing release/integration
pilot dispositions and promotion authority remain unchanged.

For insufficient historical records, inspect the original collector/run evidence
and the covering delivery. Recover actual command/result evidence when available.
Otherwise choose the narrow faithful command from repository instructions and
code, reproduce at the immutable pre-fix revision and validate at the exact landed
revision in isolated extracts or fixtures with independent build outputs, and
capture both outputs as explicitly retrospective records.

Attach those records and the structured assessment through
`orbit.task.artifact` action `put`, staging each file under `.orbit/tmp/`:

```bash
orbit tool run orbit.task.artifact --input '{"action":"put","id":"<owner-task-id>","source_path":".orbit/tmp/<file>","path":"<artifact name>"}'
```

`source_path` is the local file; `path` is the stored artifact name.
`orbit tool show <tool.name>` prints the tool's schema. Do not invent historical
execution artifacts or edit run state. Then reassess the same collector snapshot
through the filing path.
The original insufficient state remains `unresolved` until the referenced proof
is available. An existing open owner still takes precedence.

Coverage appears in `repair_assessments` and `skipped_existing` as
`covered_by_repair`, with the owner, source provenance and evidence references.
Filing retains an idempotent `ci-repair-observations/<digest>.json` receipt on the
completed owner without changing its meaning or lifecycle, creating another
repair task, or sending it to pilot/implementation admission.

While an execution
claim protects the owner, this receipt and an open compiler owner's observation
are listed in `deferred_attribution` and land when the claim settles. Unavailable,
contradictory and over-budget assessments instead report `unresolved` with a
bounded reason and leave ordinary proposed filing and pilot checks available.
There is no extra incident store, scheduler, automatic revalidation command or
unbounded historical scan.
