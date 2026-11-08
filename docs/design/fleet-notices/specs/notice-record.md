---
type: design
summary: "Spec: the fleet notice record, its effective state, expiry defaults, linked-task close and dedupe"
last_validated: 2026-11-05
title: Spec — Notice record and lifecycle
owner: claude
status: Draft
feature: fleet-notices
tags: [fleet-notices, spec]
related_features: [fleet-notices, human-decision-channel]
related_artifacts: [ORB-21207]
last_updated: 2026-11-05
---

# Spec: Notice record and lifecycle

A notice is one row in its home workspace's owner store. Its stored state changes only through
`post` and `close`; whether it is in force is its **effective state**, computed on every read from
the stored row, the clock and its linked task. A notice is never in force past its `expires_at`,
never in force once its linked task is `done` (unless it opted out), and never exists twice for
one open subject.

## Why This Exists

Managed agent activities that opt into brief delivery, operators and `orbit attention` readers
can consume notices, so one that outlives its truth can mislead people across the fleet. A stale
"hold off, binary update in progress" can idle orchestrators; a stale "bug X breaks review" can
make agents work around a fixed bug. Expiry and linked-task closure make the board safer to read
unattended, and dedupe keeps forty agents hitting one bug from producing forty notices.

## Record

| Field | Type | Meaning |
|---|---|---|
| `id` | string, required | `ntc_` plus 16 lowercase hex, minted by the owner. Stable for the notice's life, including updates. |
| `workspace` | object, required | `{id, name}` of the home workspace. Federated results add `selector`. |
| `scope` | enum, required | `workspace` or `global` ([notice-federation](./notice-federation.md#scope)). |
| `kind` | enum, required | `incident`, `maintenance` or `info`. |
| `title` | string, required | One line, 1–120 characters after trimming, no control characters. |
| `body` | string | Plain text (Markdown is stored verbatim and never rendered as HTML), at most 4096 UTF-8 bytes. Control characters other than newline and tab are refused. |
| `dedupe_key` | string, required | 1–128 characters of `[a-z0-9._:/-]`. Supplied or derived ([Dedupe](#dedupe)). |
| `author` | object, required | `{class, crew, model, task_id, run_id, machine_id}`; `class` is `operator`, `orchestrator` or `agent` ([notice-authority](./notice-authority.md#poster-classes)). `task_id` and `run_id` are present exactly when `class` is `agent`. `crew` is the orchestrator crew for `orchestrator`, the run's crew for `agent`, absent for `operator`. |
| `linked_task` | object | `{id, workspace_id}` of a task the notice is about, usually its fix. Same owner host as the home workspace ([notice-federation](./notice-federation.md#linked-tasks)). |
| `close_on_linked_done` | boolean | Default `true` when `linked_task` is set. `false` keeps the notice in force after the fix lands (for example "deployed, restart your drains"). |
| `evidence` | array | Up to 8 references, each `{type, value}` with `type` one of `task`, `run`, `friction`, `pull_request`, `url`, `commit`. Values are at most 256 characters. Stored and shown; never dereferenced by Orbit. |
| `hosts` | array | Up to 32 `machine_id`s a `maintenance` notice concerns. Display and filtering only in v1; reserved for the [dispatch-hold extension](../2_design.md#7-extension-point-maintenance-dispatch-hold). Refused on other kinds. |
| `expires_at` | RFC 3339, required | Set at post from `expires_in_hours` or an explicit instant, bounded by [Expiry](#expiry). |
| `opened_at` | RFC 3339, required | First post. |
| `updated_at` | RFC 3339, required | Last post that changed content or expiry. A [repeat](#retries) changes neither, so it never moves `updated_at`. |
| `revision` | integer, required | Starts at 1; each post that changes a content field (`title`, `body`, `kind`, `evidence`, `hosts`, `scope`) increments it. An expiry change alone does not. The brief records `id@revision` ([notice-delivery](./notice-delivery.md#delivery-record)). |
| `reports` | object, required | `{count, last_at}`: matching posts on this open notice, including the first, except [repeats](#retries). Each bumping post increments `count` and sets `last_at`. A corroborating post bumps it without changing content. |
| `closed` | object | Present once closed: `{at, by, cause}` plus `reason` for manual closes. `cause` is `manual` or `linked_task_done`. For `cause: manual`, `by` has the `author` shape and `reason` is required, 1–240 characters (supplied to `orbit.notice.close`). For `cause: linked_task_done`, `by` is exactly `{class: "system"}` — not an `author` value, carrying none of the author fields (`crew`, `model`, `task_id`, `run_id`, `machine_id`) and never valid as a poster class — and `reason` is absent; readers render `linked task done` from `cause`. |
| `status` | enum, required in output | The effective state, below. Never stored as `expired`. |

The owner store keeps the stored fields in a new table added by a store migration in slice 1
([2_design.md §2](../2_design.md#2-placement-in-the-code)). Closed and expired rows are kept for
30 days after they stop being in force, then purged by the existing
retention sweep. Purge is the only delete.

## Effective state

Computed per read, in this order:

1. `closed` is set → **closed**.
2. `linked_task` is set, `close_on_linked_done` is true and the linked task's status is `done`
   → **closed**, with `closed.cause: linked_task_done`, `closed.at` the task's `done` time,
   `closed.by` `{class: "system"}` and no `closed.reason`. The read reports it; the [materializer](#materializing-closure)
   persists it.
3. `now >= expires_at` → **expired**.
4. Otherwise → **open**.

Only **open** notices are delivered to briefs, listed by default, and shown by `orbit attention`.
A linked task that is terminal but not `done` (`archived` or `rejected`) does not close the
notice; the list output carries `linked_task.status` and attention raises the notice's severity
one step so a human or orchestrator decides ([notice-delivery](./notice-delivery.md#attention-kind)).
A linked task that cannot be read (deleted, or its workspace unreachable) leaves the notice open
and reports `linked_task.status: unknown`.

### Materializing closure

Effective closure on read is the correctness boundary; materialization only keeps the store
honest for consumers that read rows. Two writers persist `linked_task_done` closure:

- the owner's task-status transition to `done`, in the same store transaction, for notices whose
  linked task is in that store; and
- the existing scheduler sweep, for anything the transition missed (a linked task in another
  workspace on the same owner).

Neither writer changes what a reader sees, so a missed or late materialization is never visible.
Both writers, and `orbit.notice.close`, close through one store write. That write keeps
`reports.count` and `reports.last_at` from the notice as stored when the write begins, so a
corroboration that committed after the closer first read the notice is not discarded. Close does
not bump `revision`. A post that changed a content field still makes that write miss.

## Expiry

Every notice has an `expires_at`; there is no "never". Defaults and caps, all fixed
([2_design.md §6](../2_design.md#6-configuration)):

| | `incident` | `maintenance` | `info` |
|---|---|---|---|
| Default when omitted | 72 h | 12 h | 7 d |
| Cap, `operator` | 30 d | 30 d | 30 d |
| Cap, `orchestrator` | 7 d | 3 d | 7 d |
| Cap, `agent` (default = cap) | 24 h | — (refused) | 24 h |

A requested expiry beyond the poster's cap is refused (`notice_expiry_exceeds_cap`), never
clamped silently. An expiry in the past or less than 5 minutes ahead is refused. A non-repeat
re-post of an open notice may extend `expires_at`, measured from the re-post, within the
re-poster's cap; a [repeat](#retries) never does (see [Retries](#retries)).

Expiry is visible: list and show return `expires_at` and `status`; an expired notice is listed
with `--status expired` (and in `--status all`) for the retention window, and the dashboard shows
recently expired notices with their expiry time ([notice-delivery](./notice-delivery.md#dashboard)).
Expiry writes nothing; it is a read-time fact.

## Dedupe

The dedupe key names the subject. Within one home workspace, at most one **open** notice holds a
key. An expired or closed notice never matches, so a re-post after expiry opens a new notice with
a new `id` and the old one stays in history.

When the caller omits `dedupe_key` the owner derives one:

- `task:<linked task id>` when `linked_task` is set;
- otherwise `<kind>:` plus the title lowercased, with each run of characters outside
  `[a-z0-9]` collapsed to `-`, trimmed to 120 characters.

A post whose key matches an open notice is resolved by the [authority rules](./notice-authority.md#re-posting-an-open-subject):

| Outcome | When | Effect |
|---|---|---|
| `created` | no open notice holds the key | new row, `revision: 1`, `reports.count: 1` |
| `updated` | the poster may edit the matched notice | title, body, kind, evidence, `hosts`, scope (subject to scope rules) and expiry replaced as given; `revision` +1 only if a content field changed; `reports` bumped |
| `corroborated` | the poster may not edit it | content unchanged; `reports` bumped; the response returns the existing notice |

A [repeat](#retries) returns `updated` or `corroborated` by the same rights, and moves nothing —
not content, `expires_at` or `updated_at`, for an editor or a corroborator alike.

`corroborated` is how forty agents hitting one bug report it once: the first agent opens it, the
rest bump `reports.count`, and every brief shows how many runs hit it. A corroboration, like any
other matching post that is not a repeat, counts against the poster's post rate
([notice-authority](./notice-authority.md#limits)).

### Retries

A post is a **repeat** when its fingerprint equals the fingerprint of the notice's latest audited
write: the most recent write of any poster on the open notice the post matches. A lost-reply retry
is a repeat. The fingerprint is the resolved `author` object together with the request as given. Omitted
optional fields and the defaults of `scope`, `dedupe_key` and `close_on_linked_done` are resolved
first, and the expiry is compared as requested (`expires_in_hours` or `expires_at`), not as the
instant it resolves to.

A repeat changes no content field, no `expires_at` and no `updated_at`, bumps no `revision` and no
`reports`, counts against no post rate and no open-count limit, and is never refused by a limit (the
repeat check runs before them). This holds for an editor and a corroborator alike: neither repeat
applies its expiry request, whether it was given as `expires_in_hours` or as an absolute
`expires_at`, so a retry seconds or hours after the original leaves the stored `expires_at`
unchanged. The repeat check also runs before [expiry](#expiry) validation, so a repeat is never
refused with `notice_expiry_invalid` — including a lost-reply retry whose original absolute
`expires_at` is now less than 5 minutes ahead (or would resolve differently as a relative request):
it still returns `updated` or `corroborated` with the stored expiry untouched. A repeat writes
nothing and no audit row. A poster who wants to extend a notice sends a post whose expiry request
differs from the latest write; that is not a repeat, and it applies as a new post under the
[expiry](#expiry) rules, measured from that post.

Only the latest write is compared, so a post is never dropped because the notice changed after it.
An identical request sent after another poster has written applies as a new post: an orchestrator
re-asserting its notice after an agent's update still takes effect.

The owner stores a versioned request fingerprint in each post event's `detail_json`, in the same
transaction as the notice write. The posting snapshot reads the latest event by insertion sequence
for each open notice, so equal timestamps cannot hide an intervening write. The fingerprint hashes
the resolved author and request with defaults resolved, retaining the requested expiry form. Events
written before fingerprints were introduced cannot identify a repeat; their next accepted post
records one. An unchanged repeat also skips the posting transaction's retention purge.

## Invariants

- Stored state changes only through `post`, `close`, the closure materializer and retention purge.
- `status` is a function of the row, the clock and the linked task; two reads at the same instant
  agree.
- At most one effective-open notice per `(home workspace, dedupe_key)`. The store enforces it with
  a uniqueness check inside the posting transaction, not by readers.
- `id` never changes on update; `revision` never decreases and moves only with a content change.
- A close keeps `reports` committed at the notice's current `revision`. Corroboration does not bump
  `revision`, and closing does not either.
- A notice's `author` never changes. An update records its editor in history (the audit row), not
  in `author`.

## Agent Signature

claude (claude-opus-5-5), 2026-11-02, [ORB-21207].
