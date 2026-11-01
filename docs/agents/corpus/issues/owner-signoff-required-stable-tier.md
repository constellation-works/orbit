---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-18313, ORB-18751, ORB-19593, ORB-19594, ORB-19595]
---

# Criterion requires owner sign-off the lane cannot record

## Signature

`owner_signoff_required` / `Acceptance criterion 2 requires owner sign-off recorded before implementation` for a stable-tier public API deletion or serde change (ARCHITECTURE.md stable tier); the task supplies none and `orbit.decision.list` returned 0.

## Mechanism

Stable-tier breaking changes need an owner decision and no agent lane can record one, so an implementer stops before editing.

## Resolution

Stop and name the sign-off needed. Observed: A human commented `I approve it` (ORB-19593) and `I approve the change` / `I approve` (ORB-19594) on 2026-10-28T21:38-21:41Z, and Daniel forced both from blocked to proposed. ORB-18751 was cleared with `approve-anyway` carrying an operator-delegated sign-off (2026-10-27T01:07Z). Never infer approval from a comment you wrote.

## Fix task

No fix task: a deliberate gate.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 20:25 | ORB-18313 | jrun-20261025-2019-t1 | stable serde break needs owner sign-off | claude moved to rejected at 2026-10-25T20:39Z |
| 2026-10-25 20:33 | ORB-18313 | jrun-20261025-2028-c1 | stable serde break needs owner sign-off | claude moved to rejected at 2026-10-25T20:39Z |
| 2026-10-27 00:52 | ORB-18751 | jrun-20261027-0043-c1 | public TaskDecision variant needs sign-off | system moved to done at 2026-10-27T01:44Z |
| 2026-10-28 21:30 | ORB-19595 | jrun-20261028-2121-t1 | criterion 2 requires owner sign-off before implementation | system moved to done at 2026-10-28T21:49Z |
| 2026-10-28 21:35 | ORB-19593 | jrun-20261028-2131-c14 | owner_signoff_required, stopped before edit | human forced blocked->proposed at 2026-10-28T21:38Z |
| 2026-10-28 21:35 | ORB-19594 | jrun-20261028-2132-c5 | owner_signoff_required, stopped before edit | human forced blocked->proposed at 2026-10-28T21:39Z |
