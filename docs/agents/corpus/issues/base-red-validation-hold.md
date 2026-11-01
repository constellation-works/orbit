---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-15206]
---

# Required validation fails on the base, so delivery is held

## Signature

`[baseline_red] ... make ci-test-affected` fails on `origin/agent-main`; the verdict repeats `lifted: false` as the base moves.

## Mechanism

A red base makes every candidate's required validation fail, so the system holds delivery rather than blaming the candidate.

## Resolution

Wait; the hold lifts when a base passes. Do not weaken the command.

## Fix task

No fix task: red base is repaired on the base.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-10 23:09 | ORB-15206 | jrun-20261010-2230-c14 | baseline_red_hold on agent-main a91fa8b23 | hold lifted 2026-10-11T02:19Z when agent-main 9fc33494c passed |
