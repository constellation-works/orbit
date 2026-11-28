---
type: design
summary: "Spec, approved and not built: a hold record that states why a task is tagged `no-drain` and what releases it, the refusal of the tag without one, and the `held_ready` attention kind for a backlog task only the tag holds"
last_validated: 2026-11-28
title: Spec — Task hold
owner: claude
status: Draft
feature: operator-attention
tags: [operator-attention, spec]
related_features: [operator-attention, standing-drain, distributed-drain]
related_artifacts: [ORB-29623, ORB-29656, ORB-29657, ORB-29658, ORB-29662]
last_updated: 2026-11-28
---

# Spec: Task hold

A task tagged `no-drain` carries a **hold**: a short reason, a short release condition, who
set it and when. The tag cannot be added without one. A backlog task that the tag alone keeps
out of a drain becomes one attention item of kind `held_ready`, which shows the hold or says
that none was recorded. The operator approved this design on 2026-11-28 ([ORB-29623]). Nothing
in this spec is built, and the kind is not in the
[item spec](./attention-item.md#kinds-and-sources-of-truth) until its build task adds it.

## Why This Exists

`no-drain` excludes a backlog task from every automatic drain and follower. The tag says
nothing else. It names no reason, no owner and no condition for its removal, and no surface
lists the tasks it holds: `orbit run readiness` reports `no_drain_tag` only to a reader who
asks about the backlog.

Two different things therefore look the same. One is a deliberate hold: a session owns the
task and will run it itself, or the work must wait for something Orbit cannot see. The other
is a leftover: the tag was added out of habit, or by a session that has since ended. On
2026-11-27 a workspace had ready tasks idle for hours for that reason, and a deliberate hold
on neighbouring tasks could be told from the leftovers only by asking the session that filed
them.

## The hold record

A task has at most one hold:

| Field | Type | Meaning |
|---|---|---|
| `reason` | string, required | Why no drain should take the task. One line of plain text, 1 to 240 characters |
| `until` | string, required | What releases it, in words: "the design is approved", "the owning session closes". One line of plain text, 1 to 240 characters |
| `set_by` | string, required | The writer, from the write's provenance, as task history records an actor |
| `set_at` | RFC 3339 string, required | When the hold was set or last replaced |

- **The record and the tag move together.** Setting a hold adds `no-drain`. Removing
  `no-drain` clears the hold. For a task written after this ships, the tag is present exactly
  when the record is.
- **The tag stays the switch.** Admission, drain-window approval, scheduled pilots, the
  resident guard and follower admission keep reading the tag, and none of them reads the
  record. A hold changes nothing about what `no-drain` excludes.
- **`until` is text, not a trigger.** Orbit never evaluates it and never releases a hold on
  its own. A hold whose release Orbit can check does not belong in a hold: a date is
  `not_before` and another task is a dependency, and both lift without anyone acting.
- **Status changes never touch a hold.** A held task that is claimed, fails and returns to
  `backlog` is still held.
- **A hold is not pilot input.** Setting, replacing or releasing one has the effect of the
  tag edit it contains and no other: the text queues no pilot and makes no assessment stale.

## Setting and releasing

`orbit.task.add` and `orbit.task.update` gain one input, `hold`:

| Input | Effect |
|---|---|
| `hold: {reason, until}` | Sets the hold, or replaces the one there, and adds `no-drain` when it is absent |
| `hold: {}` (update only) | Releases: clears the record and removes `no-drain`. It also removes a tag that has no record |
| `tags` without `no-drain` | Releases, as today's documented way of removing the tag does |

The human CLI carries the same three as `--hold-reason` with `--hold-until`, and
`--release-hold`.

**Refusals.** Each is one line that names the `hold` input and changes nothing:

- A write whose `tags` adds `no-drain` to a task that has no hold, with no `hold` in the same
  write. This applies to every writer, the operator's CLI and the dashboard included.
- A `hold` with a missing, empty, multi-line or over-long `reason` or `until`.
- A write that sets a `hold` and sends `tags` without `no-drain`.

**Recorded.** Task history gains `hold_set`, carrying the reason and the release condition,
and `hold_released`. Each names its actor like any other entry.

**Who may write.** Whoever may edit the task's tags today. A hold adds no authority and
protects nothing: any such writer can release a hold another writer set, and history shows
who did.

## Tasks tagged before this ships

A stored task that carries `no-drain` and no hold is left as it is.

- It stays excluded, exactly as today.
- A write that keeps the tag is not adding it and is not refused.
- Every readout shows it as held with no reason recorded.
- Its hold is stated by a `hold: {reason, until}` write, or its tag is removed.

Nothing rewrites stored tasks, and nothing invents a reason for one.

## Reading a hold

- **`orbit.task.show`** returns `hold`, and **`orbit.task.list`** returns it when `fields`
  asks for it.
- **`orbit task show`** prints a `Hold:` line with the reason, the release condition, the
  setter and the time.
- **`orbit run readiness`** and the backlog-exclusion listing add the reason and the release
  condition to the `no_drain_tag` detail.
- **The dashboard** shows the hold on a task's detail as plain text, and its tag control asks
  for the two texts when `no-drain` is added.

## The `held_ready` kind

| | |
|---|---|
| Level | task |
| Source of truth | The predicates automatic admission already applies, read in observation mode: `no_drain_exclusion`, `task_dependencies_ready_with_index`, `human_crew_exclusion` and `scheduled_exclusion`. The adapter calls them and re-implements none, so the item and `orbit run readiness` cannot disagree about why the task waits |
| Item exists while | Status is `backlog`; the task carries `no-drain`; every dependency is ready; no human-crew or scheduled exclusion applies; and all of that has held for two hours |
| `since` | When the task became ready but for the tag: the latest of its move to `backlog`, its last dependency becoming ready, its `not_before`, and the hold's `set_at` |
| Derived on | The workspace's owner only, as for every task-level kind |

A `proposed` task is never an item: its approval is a separate queue, and the tag holds
nothing until the task is approved. A task in any other status is never an item either.

**Only the tag is judged.** A footprint conflict, a busy crew, host routing or a missing
browser kit can make a drain wait on the task after the hold is released. The item does not
check them. It says that the tag is a reason the task is not running, and it does not promise
that releasing the hold starts it.

**Severity.**

| Hold | Base | Ceiling | Age rule |
|---|---|---|---|
| Recorded | low | low | none: a stated hold never rises |
| None recorded | medium | high | the shared ladder, from `since` |

**Item fields.**

- `reason`: the hold's `reason` followed by its `until`, trimmed to 240 characters. With no
  hold it is the fixed text `tagged no-drain with no hold recorded`.
- `detail`: `task_hold` (the record, or `null`) and `ready_since`.
- `refs`: `task_id`.
- Fingerprint state: whether a hold is recorded, its `set_at`, and a digest of its `reason`
  and `until`. Severity joins it by the shared rule.

**`next`.**

- Primary: `orbit.task.update` with `hold: {}` ("Release the hold so a drain can admit the
  task").
- `also`: `orbit run ship <id>`, which runs the task once and leaves the hold; and
  `orbit.task.show`.
- With no hold recorded, `also` adds `orbit.task.update` with a `hold`, whose `needs` are
  `reason` and `until`. Orbit never writes either for the caller.

**The two-hour bound is a fixed constant.** Attention's thresholds stopped being
configuration when the `[attention]` keys were fixed at their defaults
([design §5](../2_design.md#5-configuration)), so that two operators' lists mean the same
thing. This kind follows that rule and adds no key. The bound exists so that a session which
files a task for itself and starts it within the hour never sees an item.

**It is a default kind.** `held_ready` joins the default set and the task-level set, so the
usual filters apply: `--kind`, `--exclude-kind`, `--severity`, `--tag`, `--older-than`. A
reader who wants only the unexplained holds asks for `--kind held_ready --severity medium`.

## Honest limitations

- **A stated hold never rises.** A reason written a month ago keeps its item at `low`. The
  item shows the setter and the date, and the reader judges whether the reason still holds.
- **A workspace with many deliberate holds gets that many `low` items,** up to the
  per-workspace cap. They stay until each hold is released.
- **The refusal breaks every writer of the tag.** A call that adds `no-drain` and worked
  before is refused after. The shipped skill text, the [configuration reference](../../../config/workflow-tags.md#the-no-drain-task-tag) and the dashboard's tag
  control change in earlier phases, so they pass a hold before the refusal exists. An
  adopter's own skills, briefs and scripts that add the tag have from phase 1 until the
  refusal to do the same, and fail after it if they have not.
- **Between the first phase and the refusal a hold is optional.** A tag added in that window
  without one reads as held with no reason recorded, like a tag from before the change.
- **A required reason can be a useless one.** Orbit checks that the two texts are present,
  not that they say anything.
- **Not yet read from the code:** whether any configuration surface files tasks with
  configured tags that could include `no-drain` (auto-tasks, sweep filing), and how a host on
  an older Orbit reads a task that carries the new field. The first build task settles the
  second before the field is added, and the refusal's task settles the first.

## Phasing

The refusal ships last, on its own. An Orbit that predates the `hold` input rejects it as an
unknown field, and an Orbit with the refusal rejects the tag without it. Shipped together they
would leave a caller no write that works on both versions.

1. **The record, with an optional input** ([ORB-29656]). The `hold` field, the `hold` input on
   add and update, the CLI flags, the history events, the readouts in `orbit.task.show`,
   `orbit.task.list`, `orbit task show` and readiness, and the skill and config text that
   tells a writer to pass a hold. A tag added without a hold is still accepted and reads as
   held with no reason recorded.
2. **The kind** ([ORB-29657]). `held_ready`, its row in the item spec, its severity and
   `next`, and the skill's attention reference.
3. **The dashboard** ([ORB-29658]). The hold on a task's detail, and the two texts in the tag
   control.
4. **The refusal** ([ORB-29662]). Turned on once every host runs an Orbit that accepts the
   input and the known writers of the tag pass a hold: the shipped skills from phase 1, the
   dashboard from phase 3, and the adopter's own skills. This is the breaking change.

## Acceptance

- Given a write that adds `no-drain` with no `hold`, the write is refused in one line naming
  `hold`, and the task is unchanged.
- Given `hold: {reason, until}` on a task without the tag, the task gains the tag and the
  record, and history shows `hold_set` with the writer.
- Given `hold: {}`, or `tags` without `no-drain`, the task has neither the tag nor the record,
  and history shows `hold_released`.
- Given a task tagged before the change, admission excludes it as before, a write that keeps
  the tag succeeds, and `orbit.task.show` returns no `hold`.
- Given a backlog task whose dependencies are done and whose hold was set three hours ago,
  `orbit attention` returns one `held_ready` item at `low` whose reason is the hold's text.
- Given the same task tagged before the change, the item is `medium`, and `high` once it is a
  day old.
- Given the same task set one hour ago, with an unfinished dependency, with a future
  `not_before`, on a human crew, or still `proposed`, there is no item.
- Given the hold is released, the item is gone on the next read.
- Given any held task, automatic admission, drain-window approval and follower admission
  decide exactly as they do today.

## Task References

- [ORB-29623] — designed the hold record, the refusal and the `held_ready` kind.
- [ORB-29656] — builds the hold record and the optional `hold` input.
- [ORB-29657] — builds the `held_ready` kind.
- [ORB-29658] — shows a hold on the dashboard and asks for it in the tag control.
- [ORB-29662] — turns on the refusal of `no-drain` without a hold.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
