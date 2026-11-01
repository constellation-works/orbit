---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-20368, ORB-19773, ORB-19781, ORB-20547, ORB-20409]
---

# Crew credential variable missing from the launch environment

## Signature

`activity refused: its crew's env sets ANTHROPIC_API_KEY (from ANTHROPIC_CREDITS_API_KEY_1) ...` or `crew opus lacked ANTHROPIC_CREDITS_API_KEY_1, required to populate ANTHROPIC_API_KEY`.

## Mechanism

The launcher that started the pipeline did not export the source variable, so the crew's env overlay cannot resolve. Recovery's login shell had it but its parent processes did not, so remediation is unverified from inside the run.

## Resolution

Relaunch with the variable exported in the actual launch environment; for clock launches provision it in `~/.orbit/clock.env` (mode 600). Never print that file. An agent cannot repair this.

## Fix task

No open fix task found. ORB-20547 (read a credential from a file) and ORB-20409 (name an unset crew env source) are landed and by search related, not verified to cover the occurrences.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29 08:19 | ORB-19781 | jrun-20261029-0819-t1 | escalation-triage run refused: crew env source variable unset | codex moved to backlog at 2026-10-29T08:41Z |
| 2026-10-29 08:47 | ORB-19773 | jrun-20261029-0847-t1 | escalation-triage run refused: crew env source variable unset | codex moved to backlog at 2026-10-29T08:50Z |
| 2026-10-31 03:23 | ORB-20368 | jrun-20261031-0322-c3 | crew lacked ANTHROPIC_CREDITS_API_KEY_1 | human:daniel moved to backlog at 2026-10-31T03:31Z |
