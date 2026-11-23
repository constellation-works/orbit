# Resolve task-pilot readiness holds

## Contents

- [Task-pilot readiness holds](#task-pilot-readiness-holds)
  - [`start_prerequisites`](#start_prerequisites)
  - [Owner/operator-only acceptance evidence](#owneroperator-only-acceptance-evidence)

## Task-pilot readiness holds

The task-pilot assessment can find a condition that makes implementation
premature or an acceptance check that the implementation lane cannot reach.
These are readiness holds, not work for a leaf to bypass.

### `start_prerequisites`

When the task contract names an explicit, unresolved condition that forbids
work from starting, task-pilot reports it in the typed `start_prerequisites`
assessment field, an array of evidence objects. For example, an assessment may
return:

```json
{
  "start_prerequisites": [
    {"evidence": "Work starts after the package owner provides its signed compatibility statement; source: the release record."}
  ]
}
```

Each entry always holds promotion and has no `holds_promotion` opt-out. A prior
leaf's refusal is a failure to handle, not a start prerequisite. Use an existing
`not_before` value for a known earliest start time, or a real dependency edge
when another task must finish. Do not encode either case as free-text evidence
or invent a task ID. Keep other explicit start conditions in the typed finding.

An authorized task owner or operator should update the contract or record the
real prerequisite. Clear a finding only when authoritative evidence establishes
that it is satisfied, or an authorized rescope removes the condition. Then have
an authorized operator request a fresh assessment explicitly if the change did
not already trigger one:

```bash
orbit task repilot <task-id>
# Wait for the requested pilot run to finish before reading the new assessment.
orbit task show <task-id>
orbit run readiness --json
```

Read the newest `task-pilot` receipt comment, then fetch its `assessment_artifact`
with `orbit.task.artifact` action `get` (`id` and `path`). Compare that assessment
with the readiness hold; older receipts contain the assessment JSON inline. After re-pilot, confirm the new assessment no
longer reports the unresolved condition and that readiness no longer holds the
task for it. A standalone pilot does not promote a task.

`no-auto-approve`
prevents automatic promotion, and `no-drain` also opts a task out of
drain-window approval. Neither changing a tag nor requesting a pilot supplies
evidence that a condition is satisfied.

### Owner/operator-only acceptance evidence

Assess evidence reachability from the implementing leaf's granted tools and
its available owner-workspace bridge. If a criterion requires owner-only or
operator-only evidence that the leaf cannot obtain through either route, record
a holding `utility_warnings` finding so admission remains held: name the
criterion, quote the evidence it requires,
identify the inaccessible record or authority boundary, and ask for a criterion
rewrite or separate owner/operator validation. For example:

```json
{
  "utility_warnings": [
    {
      "text": "Criterion 2 requires an authoritative audit-store rescan; the leaf cannot read that record or use the reserved audit tool. Rewrite the criterion or assign a separate operator validation.",
      "holds_promotion": true
    }
  ]
}
```

An authoritative audit-store rescan is not reachable just because the leaf
happens to run on the owner host; host placement does not grant access or
operator authority. Do not ask a leaf to invoke a reserved tool, widen its
permissions, or substitute operational logs or follower data.

Prefer criteria that the implementation lane can verify with repeatable
candidate checks when those checks meet the intended outcome. Reads of task
records through the bridge are not automatically infeasible. A separate
owner/operator check should name the authoritative evidence to inspect and how
its result will be recorded; once that evidence is available, update or rescope
the criterion and explicitly re-pilot as needed using the authorized operator
path.

Verify the resulting
assessment artifact (or legacy inline comment) and `orbit run readiness --json` as above. An owner-side
assessment still must use its actual granted authority; it does not inherit
operator authority merely from its host.

Claimed workers reach the owner's `orbit.task.show`, `orbit.search`,
`orbit.task.list`, `orbit.task.add`, `orbit.friction` action `add`,
`orbit.task.artifact` action `get` / `orbit.task.artifact` action `put`, and `orbit.notice.post` /
`orbit.notice.show` / `orbit.notice.close` through their run's coordinator.
Notices are stored only on the owner, and the claim binding supplies the
posting task and run.

Search and task listing read only the claimed workspace's owner
records; federated search scope (`workspaces`, `all_workspaces` and aliases)
is refused. Task show and artifact get may read any task in the claimed
workspace; artifact put stays on the claimed task, another task's `review-*`
artifacts and another run's delivery of the claimed task are refused, and
finding creation retains its `spawned_from` requirement.

Use search/list for
owner-side deduplication and return dedupe evidence in the output or the new
finding. `orbit.task.update` stays denied: a task requiring it remains an
`owner_hold` naming that tool and needs owner execution. Other mutating tools
remain outside the closed bridge.

Inspect any of them with `orbit job show <id>` before invoking — the step list is
the contract.
