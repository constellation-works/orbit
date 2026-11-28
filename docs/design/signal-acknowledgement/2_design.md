---
title: Signal Acknowledgement — Design
owner: claude
created_date: 2026-11-23
updated_date: 2026-11-28
last_validated: 2026-11-24
status: Accepted
feature: signal-acknowledgement
doc_role: design
type: design
summary: "Design: an operator-only `orbit ack` that marks an attention item, degraded source, doctor warning or run warning as expected, with reason, author and a required expiry, shown as acknowledged and left out of counts"
tags: [signal-acknowledgement, operator-attention]
---

# Signal Acknowledgement — Design

This doc holds the mechanisms behind [1_overview.md](./1_overview.md), which states the problem and the goal.

## Shape

### Signature (question 1)

A signature names one signal, and matching is exact. There are no wildcards, no prefixes and
no message text.

| Surface | `signal` | `subject` |
|---|---|---|
| Attention item | `attention:<kind>` | the item `id` (`att_<kind>_<hex>`), required |
| Attention degraded source | `degraded:<kind>:<code>` | the entry's `scope` (`workspace:<name>` or `host:<machine_id>`), plus `degraded[].subject` when present |
| Doctor row | `doctor:<check_name>` | optional; empty matches the check on its scope |
| Drain, ship or job-start warning | `warning:<code>` (`unset_env_pass`, `unset_crew_env`, `claude_worker_credential`, `task_pilot`, `validation_env`, `host_os_backlog`, `resource_throttle`) | optional; empty matches the warning on its scope |

- An attention item `id` already hashes kind, host, workspace and subject ids, so the
  signature names one condition and never a whole kind.
- An attention acknowledgement also records the item's `fingerprint` at acknowledgement time.
  When the fingerprint changes (new state, or a severity raise), the acknowledgement no longer
  matches and the item is shown again. A material change always brings the item back.
- Doctor rows and run warnings carry no fingerprint. Their acknowledgement matches the code on
  its scope until it lapses. Their `warning:<code>` names become a closed, documented set:
  adding a warning means adding its code.
- Message wording is never part of a signature, so a reworded warning stays acknowledged and
  a different condition with similar text does not.

### Scope and authority (question 2)

- **`workspace` scope** is held by the workspace's owner store, like a fleet notice, and read
  by every host that reads the workspace. It matches attention items and doctor rows for that
  workspace.
- **`host` scope** is held in the host registry beside the `orbit host offline` mark. It
  matches signals that host produces: host-local attention sources, doctor rows on that host
  and its drain and run warnings.
- **No fleet scope in v1.** One record that silences a signal in every workspace is too
  broad, and the fleet-wide case so far (a host that is down) has `orbit host offline`.
- **Operator only.** Writing (`orbit ack add`, `orbit ack lift`) requires the Operator
  capability, as `orbit host offline` does. An agent or resident session cannot acknowledge.
  An acknowledgement hides a signal from every other agent, so it is authority, not
  coordination. Every session can read acknowledgements, because they appear inline on the
  surfaces they affect and in `orbit ack list`. Letting residents acknowledge through a mandate
  right is deferred (Open questions).

### Lapse (question 3)

An acknowledgement stops matching at the first of:

1. **`--until`**, required, at most 30 days out. To keep it longer, the operator acknowledges
   it again. An expired record is not deleted; it stops matching at read time, like
   `HostOffline::active_at`.
2. **A fingerprint change** (attention items only), as above.
3. **`--task <id>` reaching a terminal status**, when a fix task is named ("until the fix
   lands"). This is the only lift condition in v1.
4. **`orbit ack lift <ack_id>`**.

There is no lift-condition language such as "until the host answers". A predicate the ack
evaluates would be a second source of truth for the condition. "Until the host answers" is
already `orbit host offline` plus `orbit host online`.

### Where it shows (question 4)

The common rule: a matching signal leaves its count and is listed once as acknowledged, with
`reason`, `set_by`, `since` and `until`. A `critical` attention item, or a doctor `error`
row, is never hidden. It stays in its list, its count and the exit status, and only gains an
`acknowledgement` annotation that carries the explanation.

- **Attention.** Matching items move from `items` to a new envelope field `acknowledged_items`:
  `[{item, ack: {ack_id, reason, set_by, since, until, task_id?}}]`. They are left out of
  `counts.matching`, `returned`, `by_kind` and `by_severity`, and `counts.acknowledged` gives
  their number. `acknowledged` keeps its current host-offline shape, so an older federated peer
  never meets a record it cannot decode. Matching `degraded` entries move to
  `acknowledged_degraded` with the same `ack` object. The empty message names both, for example
  `Nothing needs attention (2 acknowledged, 1 acknowledged source not read)`, so an empty list
  is still never read as all clear. In the cursor, an acknowledged item's hash is dropped from
  the next seen set, so the item comes back once in a delta feed when its acknowledgement
  lapses. The table output ends with an `Acknowledged (N)` block of one line each:
  kind, ref, reason, by, until.
- **Doctor.** A matching `warning` row keeps `status: warning` in JSON and gains
  `acknowledgement`. The table shows it as `acknowledged` with the reason. The summary counts
  it as acknowledged, not as a warning. `error` rows are annotated only.
- **Drain, ship and job-start warnings.** A matching warning leaves `warning` and is listed in
  `acknowledged_warning` as one line, `<code>: <reason> (by <set_by>, until <until>)`, in the
  CLI, MCP and dashboard receipts that `SubmissionNotices::insert_into` already feeds. An agent
  reading a drain receipt therefore sees the answer where it sees the warning.
- **`orbit ack list`** shows active acknowledgements, plus those that lapsed in the last 7
  days marked `lapsed` and why. One whose signal is not raised right now is marked `idle`, so
  stale acknowledgements can be seen and lifted.

### Relation to notices, switches and the sentry (question 5)

- **Fleet notices** announce a fact that the fleet should know for a while, and they are items
  themselves (`notice_open`). An acknowledgement attaches to a signal and changes how it is
  counted. The two do not merge. A notice about an incident may link the same fix task that an
  acknowledgement's `--task` names.
- **Dedicated switches win.** `execution.env.login_auth` stays effective while configured.
  `orbit host offline` changes probing as well as attention display. Without `--until`, its
  mark is indefinite; with its optional RFC 3339 `--until` deadline, it lapses when that time
  passes. If the host is still unreachable, normal attention behavior returns (ORB-27761 corrected this
  description of the expiry): the
  `host_unhealthy` item and `unreachable_destination` degraded source appear again instead of
  the host being listed under `acknowledged`. When a recurring signature deserves a permanent
  declaration, the fix is a switch or a source change, and the acknowledgement is the stopgap
  until then. An active host-offline mark is listed in the existing `acknowledged` area of
  attention.
- **The formation sentry's local `mute`** filters one session's alerts outside Orbit. For
  Orbit signals, a sentry reads the `ack` objects on the surfaces it already polls and keeps no
  mute rule of its own. Its mute remains for non-Orbit signals such as alert email.

### Record and surfaces

```json
{ "ack_id": "ack_…", "scope": "workspace", "signal": "attention:ci_red",
  "subject": "att_ci_red_…", "fingerprint": "fp_…", "reason": "flaky upstream runner, fix in ORB-…",
  "set_by": "daniel", "since": "2026-11-24T04:00:00Z", "until": "2026-12-01T00:00:00Z",
  "task_id": null, "lifted_at": null }
```

- CLI: `orbit ack add <signal> [<subject>] --reason <text> --until <rfc3339|duration>
  [--task <id>] [--host]`. Plus `orbit ack list` and `orbit ack lift <ack_id>`. `orbit
  attention` prints the exact `orbit ack add …` line for each item in `--json` `detail`, so
  nobody hand-builds a signature.
- MCP: `orbit.ack.list` is read-only and in `tasks-read`. `orbit.ack.add` and `orbit.ack.lift`
  are Operator-capability writes.
- Validation: the reason must be 1 to 240 characters with no control characters. `until` must
  be in the future and at most 30 days out. An unknown `signal` code is refused.
  Acknowledging a signal that is not raised now is allowed (planned noise), and the result says
  it is `idle`. Each write is recorded in the store's history with its author.
- The workspace store gains one table through a forward migration. The host registry gains one
  list beside `offline`.

## Concerns and honest limitations

- **A new store.** This reverses "no acknowledgement table" for attention. It is bounded by
  the 30-day expiry and by exact signatures. It is still a second record that can be wrong, so
  `idle` and `lapsed` are shown.
- **Doctor and warning signatures do not see changes.** With no fingerprint, a changed
  condition under the same code stays acknowledged until it lapses. Optional subjects (a
  variable or crew name) narrow it, but the warning fields must first carry those names in
  structured form.
- **Owner-held reads.** A follower whose owner is unreachable cannot read workspace
  acknowledgements. It then shows the signals unacknowledged, which is the safe side.

## Phasing

1. **Record, CLI and MCP tools, attention.** Covers both scopes, attention items, degraded
   entries, counts, cursor handling and the critical rule. Filed as ORB-27754, parked in
   `someday` after the scope's approval and not started.
2. **Doctor and run warnings.** Adds the closed warning-code set, doctor annotation and
   summary counts, and receipt fields. Not filed yet.
3. **Skill and sentry guidance.** Changes the `orbit` skill's attention reference and the
   formation sentry to read `ack`, not their own lists. Not filed yet.

## Open questions

1. **Resident right.** May a resident acknowledge inside its mandate's workspaces, with a
   shorter expiry cap? This scope defers it and keeps writes operator only.
2. **Expiry cap.** Is 30 days right, or should it be shorter?

## Task References

- ORB-27754 — implements phase 1; parked in `someday` after this scope's approval
- ORB-27761 — corrected this scope's description of host-offline expiry

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
