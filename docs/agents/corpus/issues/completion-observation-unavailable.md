---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-20628, ORB-16209, ORB-19689, ORB-19721, ORB-19845, ORB-19981]
---

# PR completion cannot observe GitHub, then source repair is refused

## Signature

`pr_complete` fails with `completion_observation_unavailable`: `Post "https://api.github.com/graphql": net/http: TLS handshake timeout`, or the private VCS commit-runs reader dies with SIGPIPE / `stdout capture limit exceeded`. Resume is then refused: `candidate-changed ... source repair refused: native validation candidate or effective policy changed` or `rebase recovery checkpoint at sync_base requires governed candidate recovery`.

## Mechanism

A transient provider or reader failure stops completion after publication. The unchanged-completion reuse path then rejects the candidate because the stop left a checkpoint it did not classify as reusable, so a mechanical retry is refused and a human must admit current-policy source repair.

## Resolution

If GitHub reads recover and the PR is unchanged, requeue through candidate reuse (ORB-16209: final recovery requeued 1 of 2 per 24 h after `github.pr.list` and `github.run.list` succeeded again). Otherwise the operator runs `orbit.run` action `resume` inspect then submit on the stopped run, as triage named for ORB-19981. Never guess at the binding.

## Fix task

ORB-20628 (done): classify the refreshed `delivery_preparation_refresh` checkpoint at `sync_base_preparation` as reusable (commit 70f6164d8, PR #7373). Effectiveness unmeasured.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-15 07:33 | ORB-16209 | jrun-20261015-0733-t2 | GitHub TLS handshake timeout stopped completion | operator moved to review at 2026-10-16T00:57Z |
| 2026-10-29 04:58 | ORB-19689 | jrun-20261029-0437-c18 | VCS reader SIGPIPE, base re-check unreadable | codex moved to backlog at 2026-10-29T07:51Z |
| 2026-10-29 07:41 | ORB-19721 | jrun-20261029-0558-c19 | VCS reader SIGPIPE, base re-check unreadable | merged outside Orbit at 2026-10-29T08:43Z, settled done |
| 2026-10-29 14:56 | ORB-19845 | jrun-20261029-1333-c3 | GraphQL TLS timeout then source repair refused | merged outside Orbit at 2026-10-29T15:37Z, settled done |
| 2026-10-30 08:02 | ORB-19981 | jrun-20261030-0653-c5 | GraphQL TLS timeout then source repair refused | merged outside Orbit at 2026-10-30T08:44Z, settled done |
| 2026-10-29 15:00 | ORB-19845 | jrun-20261029-1333-c3 (final_recovery) | completion observation unavailable / candidate-changed | blocked_task_triaged human_only in ORB-21505 resume pass |
| 2026-10-30 08:06 | ORB-19981 | jrun-20261030-0653-c5 (final_recovery) | completion observation unavailable / candidate-changed | blocked_task_triaged human_only in ORB-21505 resume pass |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 3 source occurrence(s) on 3 task(s); route `run_failure`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29T04:58:43.043387100Z | ORB-19689 | jrun-20261029-0437-c18 | PR completion cannot observe GitHub/VCS, then completion reuse refused; source ORB-19689 history:final_recovery@2026-10-29T04:58:43.043387100Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |
| 2026-10-29T15:00:01.321517927Z | ORB-19845 | jrun-20261029-1333-c3 | PR completion cannot observe GitHub/VCS, then completion reuse refused; source ORB-19845 history:blocked_triage@2026-10-29T15:00:01.321517927Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-30T08:06:16.179969110Z | ORB-19981 | not recorded in triage comment | PR completion cannot observe GitHub/VCS, then completion reuse refused; source ORB-19981 history:blocked_triage@2026-10-30T08:06:16.179969110Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
