# Task fields: placement, schedule and provenance

## Behavior-affecting optional fields: placement and provenance

- `requires_labels: ["<label>", ...]` — capability labels a host must carry,
  **all of them**, to run the task. A host carries its `machine.labels` setting
  plus its OS as an implicit label (`linux`, `macos`, `windows`); `orbit doctor`
  prints the effective set. `os:` tags keep their any-of meaning and both
  apply. Labels are opaque, trimmed and lowercased.
  - Add and update refuse a
    requirement no known serving host carries with the task's `os:` tags, naming
    the missing labels: this host, plus followers that pulled from the workspace
    in the last seven days. Update replaces the list; `[]` (CLI
    `--requires-labels ""`) clears it.
  - A host lacking a label skips the task
    (`host_labels_missing`), and while no live host carries them task show/list
    and `orbit run readiness` name the wait in `placement_wait`. Use it for a
    real capability (a GPU, a signed toolchain), not to pin a hostname.
    `workflow.tag_requires_labels` adds configured labels for every matching
    task tag (trimmed, case-insensitive).
  - Admission uses their union with this
    field, including for existing tasks; the owner applies its map on pulls.
    Derived labels are not persisted here, and clearing this field cannot remove
    a tag requirement. A workspace can map `ui = ["visual-fixture"]` and declare
    that label only on hosts whose managed worker can run its fixture backend;
    the built-in browser-kit check still applies.
- `not_before: "<RFC 3339 instant>"` — the earliest time a dispatcher may
  start the task, stored in UTC and shown only when set. Give an instant with
  an offset (`2026-11-06T09:00:00Z`); the CLI's `--not-before` also takes a
  local `YYYY-MM-DDTHH:MM` in the host's zone and echoes UTC and local time.
  A malformed time is refused naming it.
  - On `orbit.task.update` an empty
    string (CLI `--clear-not-before`) clears it; `null` leaves it unchanged, as
    for any field. Until then a `backlog` task waits `scheduled`, with the time
    in `detail`, in drain listings, `orbit run readiness` and `run show`. No
    local or pull drain starts it, and `orbit.workflow` action `ship` refuses it
    with `task_scheduled`.
  - The hold lifts by itself on the first admission pass
    after that time, which can be later than the time itself. Only an
    operator's explicit ship (`orbit run ship <id>` or the dashboard's Ship)
    starts it early, recording `schedule_overridden` and `readiness_overridden`
    history events; changing `not_before` reinstates the hold. Other statuses ignore it.
- `origin` records why the task was filed, from a closed set:
  `operator_request` (a human operator asked for the work),
  `orchestrator_initiative` (the orchestrator judged it necessary on its own)
  or `automation` (a system filer). Orchestrators set it on every filing; the
  operator's request usually reaches you in chat, so Orbit cannot infer it.
  - Tasks minted by auto-task definitions, routines and other system filers, and
    sweep findings tagged `auto-task:*`, `qa-sweep`, `code-review`,
    `security-review` or `friction-curation`, record `automation` when no origin
    is given. Any other filing without one stays unset, which never reads as a
    value. `orbit.task.update` can correct an origin but not clear it, an unknown
    value is refused with the allowed list, and `orbit task list --origin`
    (`origin` on `orbit.task.list`) filters by it.
- `rule_fingerprint` is the rule-result fingerprint the ux-rules filer
  (`file_rule_findings`, now removed) stamped on the tasks it
  filed. Those tasks keep it; no task gains one now. It is not settable through
  task create or update, and is readable through the `rule_fingerprint` show
  projection.
- `covered_by` names a task or GitHub PR covering an archived/rejected CI
  sweep finding. Its target is a task ID or an external key such as
  `github-pr:NUMBER` (this checkout's repository) or
  `github-pr:https://github.com/OWNER/REPO/pull/NUMBER`. Preserve existing
  relations and append it in the same update that archives/rejects the finding;
  see [CI recovery](../../orbit-orchestrate/references/recovery-ci-findings.md).
