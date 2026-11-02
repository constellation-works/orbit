---
type: design
summary: "Spec: the resident.checkpoint, resident.request and resident.answer comment shapes, the stale-cycle rule, the synthesized incomplete checkpoint, and brief reconstruction on resume"
last_validated: 2026-11-02
title: Spec — Checkpoint and resume contract
owner: claude
status: Draft
feature: resident-orchestrator
tags: [resident-orchestrator, spec]
related_features: [resident-orchestrator, activity-job, mid-run-steering]
related_artifacts: [ORB-21229, ORB-21243, ORB-21246]
last_updated: 2026-11-02
---

# Spec: Checkpoint and resume contract

Every resident cycle ends with exactly one `resident.checkpoint` record on its mandate's
assignment task, written by the agent through `orbit.resident.checkpoint` or, failing that,
synthesized by the cycle's finalize step. The next cycle builds its brief from the last *K*
checkpoints and Orbit state; it never reads the previous agent's final message. Questions the
resident escalates and words the manager relays use two more shapes, `resident.request` and
`resident.answer`. All three are task comments whose message is one JSON object, validated by core
on write.

## Why This Exists

The checkpoint is the resident's whole memory between cycles. If its shape were free prose, every
cycle would re-interpret the last one, metrics could not count decisions or reversals, and a crash
would leave no trace of what was half done. A validated record with action keys makes recovery
mechanical and the evolution and metrics passes possible. The keys here are the cycle's
`resident:<mandate>:<cycle>:<n>` keys. Slice 3 ([ORB-21244]) adds them as caller-supplied keys on
the cycle's mutating tools. Until then, the dedupe Orbit already has (automation attempt, landing and
visual follow-up keys, and guarded `request_id`s) does not take a resident key on those tools, so a
retried cycle action gets no dedupe before that slice lands.

## `resident.checkpoint` v1

```json
{
  "kind": "resident.checkpoint",
  "schema": 1,
  "mandate": "ORB-21300",
  "cycle": "rcy-20261102-1010-ab12",
  "run_id": "jrun-…",
  "crew": "opus",
  "recovery_of": null,
  "wake": {"reasons": ["task_changed:ORB-21180", "decision_answered:DR-…"], "digest": "sha256:…"},
  "authority": {"rights": ["dispatch", "promote"], "observe_only": false, "narrowed_by": null},
  "actions": [
    {"key": "resident:ORB-21300:rcy-…:1", "kind": "dispatch", "subject": "ORB-21181", "result": "ok", "ref": "jrun-…"}
  ],
  "decisions": [
    {"id": "d1", "summary": "Promoted ORB-21182 before ORB-21183: no file overlap", "class": "routine", "reversible": true, "p_wrong": "low", "rework_units": 2, "precedent": null}
  ],
  "requests": [{"id": "q1", "decision_request": "DR-…", "task": "ORB-21190", "blocking": false}],
  "costs": [
    {"kind": "manual_step", "what": "Cleared a stale pilot hold by comment", "minutes": 4, "retries": 0, "slot_minutes": 0, "wished": "Pilot re-checks a hold when its cause task lands", "signature": "manual-pilot-hold-clear"}
  ],
  "notes": "ORB-21183 waits on DR-…; recheck CI on ORB-21181 next cycle.",
  "next_wake": {"reason": "heartbeat", "not_before": "2026-11-02T12:10:00Z"},
  "continuity": "resumed",
  "outcome": "progress",
  "probe": false
}
```

| Field | Rule |
|---|---|
| `cycle` | The live cycle id issued by `resident_observe`. A checkpoint for any other id is refused (`resident_cycle_stale`) |
| `recovery_of` | The dead cycle's id when this is a recovery cycle |
| `wake.digest` | The digest observe computed; the next due check compares against it |
| `actions[]` | Every mutating action, with its action key. `result`: `ok`, `refused`, `failed`, `already_done` |
| `decisions[]` | Choices the resident made itself. `class` is one of the escalation classes or `routine`/`reversible`; a non-escalating class with `reversible: true` is listed in the digest for veto |
| `requests[]` | Escalations raised this cycle or still open |
| `costs[]` | Cost events ([evolution-ledger](./evolution-ledger.md)); an empty list when none |
| `notes` | ≤ 2,000 characters, for the next cycle and the human |
| `continuity` | `resumed`, `rebuilt` (conversation expired or absent) or `off` |
| `outcome` | `progress`, `idle` (woke, nothing to do), `waiting` (only open requests), `incomplete` (synthesized), `handoff`, `ended` |

Limits: 64 KiB per record, 50 actions, 20 decisions, 20 costs. A cycle that reaches a limit does
not write a second checkpoint; it stops and leaves the rest to the next cycle. Bounded cycles are
the point.

### Synthesized checkpoint

When the agent wrote no valid checkpoint, `resident_finalize` writes one with `outcome:
incomplete`, `actions[]` rebuilt from audit rows under this cycle's action keys (the caller-supplied
keys slice 3 adds; with `result` from the audit row), empty `decisions`, `costs` and `notes`, and an
`error` field naming why (agent failed, budget exhausted, invalid record refused). If finalize
itself never ran (process killed), the next observe treats the cycle as dead and the recovery
cycle's checkpoint carries `recovery_of`.

## `resident.request` v1

Written by the resident beside the decision request it raises, so the assignment's thread shows the
question without opening the human-crew task:

```json
{"kind": "resident.request", "schema": 1, "mandate": "ORB-21300", "cycle": "rcy-…", "id": "q1",
 "decision_request": "DR-…", "task": "ORB-21190", "class": "architecture",
 "question": "…", "recommended": "a", "blocking": false, "blocks": ["ORB-21183"]}
```

The decision request is the authority for the answer; this comment is an index entry.

## `resident.answer` v1

Written by `orbit.resident.relay` (the manager), or by the human directly:

```json
{"kind": "resident.answer", "schema": 1, "mandate": "ORB-21300",
 "request_id": null, "reverses": null,
 "quote": "hold off on ORB-21183 until Monday, I want to look at the schema",
 "source": {"human": "daniel", "via": "manager-session", "session": "…", "said_at": "2026-11-02T09:58:00Z"},
 "relayed_by": {"crew": "opus", "model": "claude-opus-5-5"}}
```

`quote` is verbatim: the manager may add nothing to it. `request_id` links an answer to a
`resident.request` (an answer that also exists as a decision record is read from the decision
record). `reverses` names a checkpoint decision the human is undoing; the metrics count it. The
tool refuses any field naming rights or workspaces. A `resident.answer` is a wake event.

## Resume

The brief `resident_observe` builds, in order:

1. the mandate artifact and the effective authority, including `observe_only` and why;
2. the last *K* checkpoints (default 5), newest first; older ones only as their `notes`;
3. open `resident.request` entries with their decision state and answers;
4. `resident.answer` and other non-resident comments since the last checkpoint;
5. the wake reasons with the facts behind them (task status, run outcome, PR/CI state);
6. notices in scope, quoted as untrusted data ([notice-delivery](../../fleet-notices/specs/notice-delivery.md));
7. for a recovery cycle, the dead cycle's audit rows;
8. the workspace's standing goals and the resident-cycle skill reference.

**What an expired provider conversation reconstructs:** everything above. Nothing else is
reconstructed, because nothing else is authoritative. A conversation the adapter can resume
(`conversation_ref` in the previous checkpoint, with `continuity: prefer`) receives the same brief
as its next message. The checkpoint records `resumed` or `rebuilt`, and a resumed cycle must not
act on anything the brief contradicts.

## Agent Signature

Designed by claude (claude-opus-5-5) for [ORB-21229]; built by [ORB-21243] and [ORB-21246].

## Task References

- [ORB-21229] — designed the checkpoint and resume contract
- [ORB-21243] — implements the checkpoint tool and the request/answer shapes
- [ORB-21244] — adds the caller-supplied action key on the cycle's mutating tools (slice 3)
- [ORB-21246] — implements brief reconstruction and the continuity adapter

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
