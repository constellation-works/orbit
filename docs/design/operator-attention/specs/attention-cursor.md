---
type: design
summary: "Spec: attention fingerprints and the stateless --since cursor: format, delta semantics, validity, expiry and the consumer-hook protocol"
last_validated: 2026-11-01
title: Spec — Attention fingerprint and cursor
owner: claude
status: Draft
feature: operator-attention
tags: [operator-attention, spec]
related_features: [operator-attention]
related_artifacts: [ORB-21164, ORB-21165]
last_updated: 2026-11-01
---

# Spec: Attention fingerprint and cursor

Each item carries a **fingerprint** that changes only when the item materially changes. The
response carries a **cursor**: an opaque, self-contained token recording which `(item,
fingerprint)` pairs the caller has already been shown. Passing it back as `--since` returns
only items that are new or changed. The cursor is stateless: Orbit stores nothing, any process
can mint or read one, and a cursor that cannot be used degrades to the full list with a flag.

## Why This Exists

A hook that re-injects the same eight items into every prompt makes the agent ignore all of
them. The hook needs "what is new since I last told you", and the system must not grow an
inbox, acknowledgement table or per-session store to answer it
([4_decisions.md](../4_decisions.md#the-delta-cursor-is-stateless-and-carries-what-the-caller-has-seen)).

## Fingerprint

`fingerprint = "fp_" + first 16 hex of SHA-256(canonical JSON of material)`, where `material`
is `{id, severity, state}` and `state` is the kind's discriminator below. Canonical JSON means
sorted keys and no insignificant whitespace.

| Kind | `state` (changes ⇒ the item is "changed") |
|---|---|
| `pilot_warning` | operation id of the latest pilot receipt, and the sorted finding kinds and texts |
| `pilot_hold` | hold classification, the hold's history operation id, the folded decision request id |
| `blocked` | `blocked_at`, `basis`, the escalation comment's time, the folded decision request id |
| `decision_open` | request id (a re-raised request is a new item) |
| `review_waiting` | `waiting_on` and the settling review's id |
| `run_failed` | run id and state |
| `run_stuck` | run id and whether the cause is a dead owner or the wall-clock bound |
| `claim_stale` | claim id, phase and the set of diagnostics that hold |
| `ci_red` | the failing run's id, per workflow |
| `friction_untriaged` | friction id |
| `host_unhealthy` | the sorted failing check ids and reachability state |

The fingerprint never covers `since`, `age_seconds`, `generated_at`, the `expires_at`
countdown, the wording or truncation of `reason`, or list position. Two consequences follow:

- Time passing changes nothing, **except** that `severity` is part of the material, so an item
  that ages from `medium` to `high`, or whose decision request enters its final 6 hours,
  re-surfaces once. That re-surfacing is deliberate: the item got worse.
- Re-wording a source message in a later Orbit release does not re-inject every item.

`id` is part of `material`, so equal states on different items never collide.

## Cursor format

```
atc1.<base64url, unpadded>
```

The decoded bytes are: one version byte (`0x01`), `issued_at` as a big-endian `u64` of Unix
seconds, a big-endian `u16` count `n`, then `n` entries of 8 bytes each. Each entry is the
first 8 bytes of `SHA-256(id || 0x00 || fingerprint)` for one item the caller has seen.
`n` is at most 1000, so a cursor is at most about 10.7 KB of text, within argv and
environment limits.

The cursor is not signed and not secret. It contains only hashes, and tampering can only change
what the *caller itself* is shown; it grants nothing and reads nothing. The prefix `atc1`
names the format version; a future change mints `atc2`.

## Delta semantics

Given a cursor with seen-set `S` and the call's filtered, ordered matching items `M`:

1. `returned = [item ∈ M where hash(item.id, item.fingerprint) ∉ S]`, then `--limit` applies.
2. `counts.unchanged = |M| − |returned before limit|`. Delta mode says "No new or changed
   items (N unchanged)" rather than "Nothing needs attention", so quiet and clear stay
   distinguishable.
3. The **next cursor's** set is `{hash(item) : item ∈ M and (hash(item) ∈ S or item was
   returned)}`. Unchanged items stay; items that resolved drop out; items cut by `--limit` stay
   unseen, so the next call returns them. An item that resolves and later returns with the same
   fingerprint is new again.
4. When `degraded` is non-empty, the next cursor also carries forward every hash of the cursor
   supplied, so a flapping host or a skipped source does not make its items re-surface as new
   each time it recovers. Carried hashes age out with the cursor's TTL or when a clean pass
   follows.
5. If the set would exceed 1000 entries, the lowest-severity, newest items are left out of the
   cursor and re-surface; `cursor.truncated: true` says so.
6. Resolved items are not reported. The cursor holds hashes, which cannot be turned back into
   ids. A consumer that wants "this was resolved" compares two full lists itself
   ([3_vision.md §1](../3_vision.md#1-open-questions)).

Filters apply before the cursor. A cursor minted under one filter set and used under another
is valid: items outside the first filter simply were not seen and appear as new, which is
correct for a caller that has not been shown them.

## Validity and expiry

- A cursor is valid for `attention.cursor_ttl_hours` (default 24) after `issued_at`. Every
  response mints a fresh cursor with a fresh `issued_at`, so a hook that calls at least daily
  never expires; expiry measures the *gap* between calls.
- Validity is judged by the process that serves the call, against its own clock. An
  `issued_at` more than 5 minutes in the future is `invalid`.
- An unusable cursor is **never an error**. The call returns the full filtered list with
  `cursor.status` set, and mints a fresh cursor:

| `cursor.status` | Cause | Result |
|---|---|---|
| `none` | no `--since` supplied | full list |
| `accepted` | decoded and within TTL | delta list |
| `expired` | past `cursor_ttl_hours` | full list, flagged |
| `invalid` | not `atc` prefixed, bad base64, wrong length, future `issued_at` | full list, flagged |
| `unsupported` | a version this build does not read | full list, flagged |

  Falling back to the full list is the safe direction: a consumer may see an item twice but
  never misses one. A full-list response with a non-`none`, non-`accepted` status is what a
  hook shows with a "list restarted" note.

## Consumer-hook protocol

Orbit ships the feed and cursor; the embedding project owns the hook. [ORB-21165] is
constellation's session hook, filed in that repository. A hook uses the contract like this:

1. **Session start.** Run `orbit attention <scope + filters> --json` with an explicit scope
   (for example `--all --severity medium`). Inject the `items` (their `reason` and
   `next.command`) into the session context. Persist `cursor.next` for this session.
2. **Later injections** (per prompt or every N minutes, rate-limited by the hook). Run the same
   command with `--since <stored cursor>`. Inject only the returned items. Replace the stored
   cursor with `cursor.next`. If `cursor.status` is `expired`, `invalid` or `unsupported`,
   treat the response as a restarted full list and say so.
3. **Failure.** If the command fails or times out, inject nothing and keep the old cursor. A
   missed poll costs nothing: the next poll returns what accumulated.
4. **Keep one cursor per session and per filter set.** Sessions sharing a cursor each get the
   items once between them. The cursor is not a lock; two pollers using the same one may both
   inject the same item, which is harmless.
5. **`degraded` is part of the injection.** If sources were blind, the hook must say so; an
   empty list with `degraded` entries is not an all-clear.

The hook, its trigger names and its state file belong to the consumer. Core contains nothing
constellation-specific and no hook ([4_decisions.md](../4_decisions.md#orbit-ships-the-feed-the-consumer-ships-the-hook)).

## Cost

A call recomputes every selected source. Task-level kinds are local reads of already-indexed
task, decision and review records; runtime kinds add run, claim and host probes; `ci_red`
adds a provider call cached for `attention.ci_cache_seconds` (default 60) per workspace. A
hook polling faster than once a minute buys nothing for `ci_red` and little for the rest; the
design assumes minutes. Per-source timing is a slice-1 measurement ([2_design.md §6](../2_design.md#6-slices)).

## Task References

- [ORB-21164] — specified the fingerprint and cursor
- [ORB-21165] — consumer hook that follows the protocol above

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
