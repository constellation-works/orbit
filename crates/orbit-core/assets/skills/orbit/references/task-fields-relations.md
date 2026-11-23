# Task fields: context locks, dependencies and relations

Contents:

- [Behavior-affecting optional fields](#behavior-affecting-optional-fields)

## Behavior-affecting optional fields

- `context_files: ["file:<path>", "dir:<path>", "symbol:<path>#<name>:<kind>"]`
  — optional selectors used for context-lock serialization. An empty list
  holds no context lock. A single-slot run, an explicit ship and owner pull
  admit such a backlog task on the next pass; a drain or ship with more than
  one slot waits for the task pilot (`awaiting_footprint`), then runs it only
  alone (`awaiting_exclusive_slot`), unless an operator has tagged the task
  `readiness-waived`.
  - That waiver admits the task like ready work: it holds no
    file lock, reserves no tree and may run beside other editing work. Only an
    operator adds the tag; an agent's write that adds it is refused, and a new
    task cannot be filed with it. Declared selectors keep their existing
    conflict checks; undeclared edit conflicts surface at landing.
- `dependencies: ["<task-id>", ...]` — prerequisites must reach a satisfying
  status first. `orbit.task.add` takes it too (guarded creation with
  `request_id` does not), with the same validation as `orbit.task.update`.
  - Unlike `resolves`, task IDs are global: a prerequisite owned by
    another workspace registered on this machine is read from its owner, and
    completing it there satisfies the dependency here. A prerequisite this
    machine has never registered stays explicitly unverifiable — it is never
    treated as satisfied from here, and it is never restored or edited from the
    depending workspace.
- `relations: [{"type": "resolves", "target": "<friction-id>"}]` — auto-resolves
  that friction when this task reaches `done`, **only in the same workspace**.
  Friction IDs are workspace-local; an unqualified target is never a global
  lookup.
  - Completing a task whose `resolves` target exists only in another
    workspace on this host is rejected with `friction_not_local` — resolve the
    friction from its owning workspace instead: `orbit friction resolve <id>` is
    the operator CLI path, or an agent runs `orbit tool run
    orbit.friction --input '{"action":"update","id":"<id>","status":"resolved"}'` (same
    resolution metadata); a covering task there also counts.
  - Other types
    (`produces`, `blocked_by`, `child_of`,
    `spawned_from`, `regression_from`, `supersedes`, `related_to`) are tracked
    but inert. Only `produces`/`resolves` accept non-task artifact IDs; `covered_by`
    also accepts GitHub PR external keys as described in [`task-fields-placement.md`](task-fields-placement.md). The remaining
    relation types require a task ID. A dangling target (unknown in every
    workspace this host can see) succeeds but emits a `TaskRelationDangling` audit event.
- A `regression_from` relation may carry
  `"regression": {"nature": "<nature>", "escape": "<escape>", "severity": "<severity>", "sites": [...]}`
  (`nature` and `escape` required, and `severity` on a new link;
  no other type accepts it). Choose `unclassified` rather than guess; an
  omitted payload reads back as absent, not as `unclassified`. Unknown values
  are refused with the allowed list.
  - `severity` — how much the defect matters, on the same scale review
    findings use ([severity rubric](task-fields-review.md#severity-rubric)). A new classified link
    without one is refused. A link the task already holds may be re-sent
    without it and keeps its recorded severity and source.
    - Every link reads
      back with `severity_source`: `filer` (set by the writer, the default),
      `verified` (finding verification confirmed or adjusted it; that job was
      removed with finding verification, so only older links carry it), `operator` (an operator override), `backfill` (a
      backfill of older links), or `default` (stored before severity existed;
      reads as `medium`). Send `"severity_source": "operator"` only for an
      operator's override.
  - `sites` (optional, up to 8) — where the defect lives, not everything the
    culprit touched: `symbol:<path>#<name>:<kind>` of the function, method or
    type that is wrong, `file:<path>` for a file without symbols, or
    `dir:<path>` only when the defect spans a directory and no narrower site
    can be named.
    - Never tests, goldens, docs or wiring the culprit only
      touched alongside. Codebase analytics attribute the culprit's regression
      to these sites, and a link without sites to no path. An
      absolute path outside the workspace refuses the write. A relations
      replacement that omits `sites` clears them, so preserve an existing list.
  - `nature` — what kind of regression the link records:
    - `broke_existing`: the culprit broke behavior that worked before it landed.
    - `defect_in_new`: the defect is in code the culprit itself introduced.
    - `incomplete`: the culprit shipped without part of what its task required.
    - `merge_reintroduced`: a merge restored code or assets an earlier change had removed.
    - `contract_drift`: docs, schemas or a contract drifted from the behavior the culprit shipped.
    - `unclassified`: the evidence does not settle the nature.
  - `escape` — why the delivery gates missed it:
    - `no_test`: no test exercised the broken behavior at the culprit commit.
    - `test_not_selected`: a covering test existed at the culprit commit but its gates did not run it.
    - `platform_only`: it shows only on a platform or host the gates did not exercise.
    - `review_missed`: it was visible in the diff and review did not flag it.
    - `spec_ambiguous`: the task or spec left the expected behavior open to the shipped reading.
    - `nondeterministic`: it depends on timing, ordering or environment and passed when gated.
    - `unclassified`: the evidence does not settle the escape.
    Choose the escape by inspecting the tests and gate selection at the culprit
    commit: `no_test` when no test exercises the broken behavior there, and
    `test_not_selected` when a covering test exists but the culprit's gates did
    not run it.
      - Choose `unclassified` only when the evidence does not settle
        the escape, and state the unresolved evidence in the finding's description.
        Mere visibility in the diff is not evidence for `review_missed` when no test
        exercises the broken behavior.
- `parent_id` is not an `orbit.task.add` input and is refused as an unknown
  field, like `plan` and `status`; use a `child_of` relation
  in `relations` when creating a subtask. For bug tasks, set `source_task_id`
  at creation on `orbit.task.add` (or `--source-task` on the CLI), or after
  creation with `orbit.task.update` (which also accepts an empty string to
  clear it). `tags` (reuse existing before inventing new).
