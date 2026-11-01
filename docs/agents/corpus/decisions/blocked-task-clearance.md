---
type: decision
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20650, ORB-20629, ORB-20637]
---

# Blocked task clearance by humans (backfilled)

## Scope

Human answers and actions that cleared a blocked or pilot-held task in ws_orbit between 2026-10-02 and 2026-11-01, reconstructed from task history by the ORB-20650 backfill. These are not formal decision requests: none existed yet (`orbit.decision.list` was empty at the pass), so the question and options come from triage markers, final-recovery diagnoses and operator comments, and the note is the human's own words where the history holds them. Where no reason is recorded the entry says so; nothing is inferred. Entries are precedent for a recommendation, never a human answer to a new request, and grant no lifecycle, merge or bypass authority. Formal answers to decision requests append to `decisions/orbit-requests.md`.

## Entries

### ORB-18103: Landing review rejected on F3: criterion 6 requires QA of one successful real pi

- Date: 2026-10-25T05:51Z
- Task: ORB-18103
- Run: jrun-20261025-0454-c4
- Question: Landing review rejected on F3: criterion 6 requires QA of one successful real pilot, but the attached QA used an 8 s stub pipeline. Accept the stub QA or require a real pilot?
- Options: Option 1: keep criterion 6 and require one real dashboard pilot before landing. Option 2: amend criterion 6 so a stubbed pipeline is acceptable, covered by the HTTP success-path test, and land the existing candidate.
- Chosen: Option 2; Daniel forced the task blocked to done at 05:51Z.
- Human note: `let's go with option 2` (comment by `human`, 05:51Z). No further reason recorded.
- Source: blocked_task_triaged marker 2026-10-25T05:40Z; comment 05:51Z; forced event 05:51Z

### ORB-19593: Owner sign-off to delete the stable-tier public wrapper `workspace_relative_path

- Date: 2026-10-28T21:38Z
- Task: ORB-19593
- Run: jrun-20261028-2131-c14
- Question: Owner sign-off to delete the stable-tier public wrapper `workspace_relative_paths_overlap` (criterion 2 requires it before implementation)?
- Options: Record an explicit sign-off comment, or rewrite criterion 2 if the deletion is outside the stable-tier rule (triage human_action).
- Chosen: Sign-off given; Daniel forced the task blocked to proposed at 21:38Z.
- Human note: `I approve it` (comment by `human`, 21:38Z). No reason recorded.
- Source: final-recovery escalation 21:35Z; blocked_task_triaged marker; forced event

### ORB-19594: Owner sign-off to delete `TaskCommitBoundary::admit_task_with_experiments` and `

- Date: 2026-10-28T21:39Z
- Task: ORB-19594
- Run: jrun-20261028-2132-c5
- Question: Owner sign-off to delete `TaskCommitBoundary::admit_task_with_experiments` and `ExperimentCapture` (stable-tier orbit-store API)?
- Options: Record a sign-off comment, or rewrite criterion 2 (triage human_action).
- Chosen: Sign-off given; Daniel forced the task blocked to proposed at 21:39Z.
- Human note: `I approve the change` (21:39Z) and `I approve` (21:41Z), comments by `human`. No reason recorded.
- Source: final-recovery escalation 21:35Z; forced event

### ORB-18751: Add the public `TaskDecision::CiFailureIdentity` variant and type to the stable 

- Date: 2026-10-27T01:07Z
- Task: ORB-18751
- Run: not recorded (pilot hold)
- Question: Add the public `TaskDecision::CiFailureIdentity` variant and type to the stable tier (owner sign-off)?
- Options: approve-anyway with the stated shape, or rescope so the type stays crate-internal (triage).
- Chosen: `task-pilot-admission: approve-anyway`, by human:daniel.
- Human note: Stable-tier owner sign-off (operator-delegated): additive persisted shape mirroring `TaskDecision::CodeScanningAlert`, with fields failure_key, compiler_cause_digest, open_compiler_set, signature, tested_commit, sources.
- Source: admission comment 2026-10-27T01:07Z

### ORB-18197: Pilot hold: criterion 3 needs `orbit.workflow.run.show`, an operator-only tool t

- Date: 2026-10-25T09:47Z
- Task: ORB-18197
- Run: jrun-20261025-0841-t3
- Question: Pilot hold: criterion 3 needs `orbit.workflow.run.show`, an operator-only tool the lane cannot call.
- Options: clear, rescope, or approve-anyway.
- Chosen: `task-pilot-admission: clear`, by human:daniel (operator_validation_resolved).
- Human note: Criterion 3 is proved from the diff: the candidate must not touch the governance catalog or any capability allowlist, so nobody needs to call the tool. Cleared by opus on-call ORB-18172.
- Source: comment and operator_validation_resolved event, 09:47Z

### ORB-18911: Pilot hold: criterion 2 needs `orbit.experiment.assign`, reserved for the operat

- Date: 2026-10-28T00:36Z
- Task: ORB-18911
- Run: not recorded (pilot hold)
- Question: Pilot hold: criterion 2 needs `orbit.experiment.assign`, reserved for the operator capability.
- Options: clear, rescope, or approve-anyway.
- Chosen: `rescope` (operator_validation_resolved, human:daniel).
- Human note: No note recorded.
- Source: operator_validation_resolved event, 2026-10-28T00:36Z

### ORB-19240: Pilot hold: criterion 1 names `orbit.workflow.run.resume`, which the implementin

- Date: 2026-10-28T00:35Z
- Task: ORB-19240
- Run: not recorded (pilot hold)
- Question: Pilot hold: criterion 1 names `orbit.workflow.run.resume`, which the implementing lane does not grant.
- Options: clear, rescope, or approve-anyway.
- Chosen: `rescope` (operator_validation_resolved, human:daniel).
- Human note: No note recorded.
- Source: operator_validation_resolved event, 2026-10-28T00:35Z

### ORB-16193: Pilot inferred native Windows and Linux CodeQL from a consumer narrative; hold o

- Date: 2026-10-15T02:57Z
- Task: ORB-16193
- Run: not recorded (pilot hold)
- Question: Pilot inferred native Windows and Linux CodeQL from a consumer narrative; hold or clear?
- Options: clear or rescope.
- Chosen: `task-pilot-admission: clear`, comment by `codex` quoting a root-authorized continuation.
- Human note: Criterion 6 names four gates, public regressions and current review; Windows and complete CodeQL evidence stay with the consumer task (named `55` in the comment) after delivery. No `os:windows` tag, broad skip or falsely passed consumer check.
- Source: comment 2026-10-15T02:57Z

### ORB-19066: Re-scope one design-sync task covering 3 feature folders, 19 docs and 152 cited 

- Date: 2026-10-27T07:52Z
- Task: ORB-19066
- Run: jrun-20261027-0723-c3
- Question: Re-scope one design-sync task covering 3 feature folders, 19 docs and 152 cited ids against a 10-file pilot assessment?
- Options: Triage: split per feature, or narrow the criteria to the pilot's 10-file footprint (Option 1: reconcile operations-as-data alone).
- Chosen: Archived by Daniel's delegate orchestrator ORB-19008.
- Human note: Minted scope (3 folders, 19 docs, 152 cited ids including 31 legacy T-ids that `orbit.task.show` rejects) far exceeds the pilot's 10-file assessment; the design-sync auto-task mints a fresh run next cycle.
- Source: comment by human:daniel 2026-10-27T07:52Z; status_changed to archived

### ORB-20598: Implementer stopped for scope reassessment: the per-line metrics need a per-task

- Date: 2026-10-31 (answer relayed 2026-11-01T00:30Z)
- Task: ORB-20598
- Run: jrun-20261031-2330-c5
- Question: Implementer stopped for scope reassessment: the per-line metrics need a per-task denominator the store does not keep. How to proceed?
- Options: The option letters are not recorded in the task history read here.
- Chosen: Choice C: split `cost_per_line` and `lead_time_per_line` into ORB-20646; ORB-20598 keeps the whole-task/step-local classification, the refusal of a step-local primary, the exploratory label and the withheld verdict.
- Human note: `ok C` (Daniel, 2026-10-31), recorded by claude-opus-5-5 on the task. ORB-20598 moved back to backlog at 00:30Z.
- Source: comment 2026-11-01T00:30Z; ORB-20646 created 00:29Z

### ORB-20625: Candidate PR #7375 carried the right fix but non-compliant regression evidence, 

- Date: 2026-11-01T00:33Z and 00:36Z
- Task: ORB-20625
- Run: jrun-20261031-2344-c9
- Question: Candidate PR #7375 carried the right fix but non-compliant regression evidence, and resume was refused. Resume, hint, or restart?
- Options: Requeue with a hint; or discard the candidate and requeue.
- Chosen: First `Requeue with hint`, then `Discard and requeue`.
- Human note: The hint: keep the `blocked_run_stopped` fix, delete the private-helper test, replace the mocked delivery test with one public-boundary regression through the real runtime. Daniel: `Discard and requeue`; resume was refused because the failure handoff disagrees with the task's run record. The fresh run landed PR #7390.
- Source: comments 00:33Z and 00:36Z; operator_requeued event

### ORB-18135: PR #6045 was reviewed clean but is red on a guard test the landing reviewer miss

- Date: 2026-10-25T08:33Z
- Task: ORB-18135
- Run: jrun-20261025-0815-c3
- Question: PR #6045 was reviewed clean but is red on a guard test the landing reviewer missed. Resume the candidate or implement fresh?
- Options: Resume the published candidate, or discard it and implement fresh.
- Chosen: Discard the candidate and implement fresh (actor `operator`).
- Human note: `requeued to implement fresh: PR #6045 red on a guard test the landing reviewer missed (no-reconcile run reads); re-implement with the operator comment`.
- Source: candidate_discarded and operator_requeued events 08:33Z

### ORB-18164: PR #6042 failed one required check. Resume the candidate?

- Date: 2026-10-25T08:46Z
- Task: ORB-18164
- Run: jrun-20261025-0811-c3
- Question: PR #6042 failed one required check. Resume the candidate?
- Options: Rerun the failed job then resume, or discard.
- Chosen: Requeue onto the published candidate (actor `operator`).
- Human note: `Only red check was a flaky ci-guardrails compiler-cache stage (F2026-10-587); rerun of CI 40176214663 is green. Resume PR #6042.`
- Source: operator_requeued event 08:46Z

### ORB-18547: Before-landing review was incomplete only on three Linux sandbox tests a Mac rev

- Date: 2026-10-26T15:10Z and 15:40Z
- Task: ORB-18547
- Run: jrun-20261026-1425-c1
- Question: Before-landing review was incomplete only on three Linux sandbox tests a Mac reviewer could not run. Resume the kept candidate?
- Options: Run the tests on a Linux host and attach results, then resume.
- Chosen: Requeue onto the kept candidate, twice (heads bedde87d and 0ae6e4f1; actor `operator`).
- Human note: `operator ran them on the box at bedde87 (all passed) and attached the results`; then `Linux evidence re-run at post-review candidate 0ae6e4f, all passed and attached`.
- Source: operator_requeued events 15:10Z and 15:40Z

### ORB-19818: Landing review attributed inherited F1 repairs to this attempt and returned `acc

- Date: 2026-10-29T16:57Z
- Task: ORB-19818
- Run: jrun-20261029-1617-c1
- Question: Landing review attributed inherited F1 repairs to this attempt and returned `accept_with_fixes` with no change. Resume the candidate?
- Options: Resume the preserved candidate; or discard.
- Chosen: Requeue onto the candidate b96c6cec (PR #6943), by human:daniel.
- Human note: Independent review confirms the single F1 fix predated review; the reviewer erred. Resume through fresh validation and required review, preserving all claims, heads and failed review evidence. No spec change or discard; backlog waits for the next authorized post-upgrade window (ORB-19771).
- Source: operator_requeued event 16:57Z

### ORB-17664: The owner rejected `review.timing=after_landing`. Is it repaired, so the blocked

- Date: 2026-10-23T05:00Z
- Task: ORB-17664
- Run: jrun-20261023-0452-t1
- Question: The owner rejected `review.timing=after_landing`. Is it repaired, so the blocked leaf can resume?
- Options: Requeue to resume the retained candidate, or keep blocked.
- Chosen: Requeue (by human:daniel).
- Human note: Owner review.timing incompatibility repaired by on-call ORB-17606 (timing restored to off at 04:51Z; task reads verified); the leaf failed only at the steering-gate owner read; candidate retained on mac-mini-1.
- Source: operator_requeued event 05:00Z

### ORB-20359: The box's `workflow.purpose_crews` referenced undefined crews. Retry the frozen 

- Date: 2026-10-31T02:51Z
- Task: ORB-20359
- Run: jrun-20261031-0235-c3
- Question: The box's `workflow.purpose_crews` referenced undefined crews. Retry the frozen review?
- Options: Retry after config repair.
- Chosen: Requeue (by human:daniel).
- Human note: `.orbit/config.toml` now uses defined crew names and `orbit.task.show` succeeds on dk-server-2.
- Source: operator_requeued event 02:51Z

### ORB-20736: Final recovery escalated because dk-server-2 rejected `[plugin_enablement]` in i

- Date: 2026-11-01T04:50Z
- Task: ORB-20736
- Run: jrun-20261101-0435-c1
- Question: Final recovery escalated because dk-server-2 rejected `[plugin_enablement]` in its host config. Requeue?
- Options: Requeue after both box configs are rewritten.
- Chosen: Requeue (by human:daniel, at Daniel's request).
- Human note: `Infra block, not task-owned.` The run failed at dispatch loading tag instructions from the owner; both box configs were rewritten at 04:42Z and 04:44Z and no longer contain the section; no candidate was produced.
- Source: operator_requeued comment 04:50Z (read after the pass scan start)

### ORB-20125: Sweep unreachable under a stale minted contract. Requeue?

- Date: 2026-10-30T09:38Z
- Task: ORB-20125
- Run: jrun-20261030-0838-c26
- Question: Sweep unreachable under a stale minted contract. Requeue?
- Options: Fix the definition first, then requeue.
- Chosen: Requeue (actor `operator`; ORB-20132).
- Human note: `invalid native QA definition corrected and verified; recurring code-health definition minimally synced to shipped recursive <=20000-line partition; current task already has that corrected contract. Prior failed run terminal/clean/no-diff. Reuse partial artifacts.`
- Source: operator_requeued event 09:38Z

### ORB-20129 and ORB-20137: Both stopped with premature incomplete handoffs. Requeue?

- Date: 2026-10-30T09:50Z
- Task: ORB-20129 and ORB-20137
- Run: jrun-20261030-0838-c25 and jrun-20261030-0842-c4
- Question: Both stopped with premature incomplete handoffs. Requeue?
- Options: Continue under a revised plan.
- Chosen: Requeue both (actor `operator`; ORB-20132).
- Human note: `diagnosed premature incomplete handoff, updated substantial audit plan/complexity without weakening AC; no blind environmental retry`.
- Source: operator_requeued events 09:50Z

### ORB-17053 and ORB-17055: The task drew grok while its balance was exhausted (HTTP 402). Requeue?

- Date: 2026-10-19T15:22Z and 15:36Z
- Task: ORB-17053 and ORB-17055
- Run: jrun-20261019-1521-t1 and jrun-20261019-1536-t1
- Question: The task drew grok while its balance was exhausted (HTTP 402). Requeue?
- Options: Move to a funded crew.
- Chosen: Move to opus and requeue (actor `operator`).
- Human note: `Drew grok while the Grok Build balance is exhausted (HTTP 402); cancelled before work and moved to opus by the on-call operator`.
- Source: operator_requeued events

### ORB-18269: Muse subprocess exited 1 with no failure text; the retained candidate had no cha

- Date: 2026-10-25T20:33Z
- Task: ORB-18269
- Run: jrun-20261025-1946-t1
- Question: Muse subprocess exited 1 with no failure text; the retained candidate had no changes. Requeue?
- Options: Discard and implement fresh.
- Chosen: Discard and requeue (actor `dashboard`).
- Human note: `requeued to implement fresh: no work completed`.
- Source: candidate_discarded and operator_requeued events 20:33Z

### ORB-19150: Implementer declared `incomplete_scenario_counts` for a no-diff sweep. Requeue o

- Date: 2026-10-27T20:06Z
- Task: ORB-19150
- Run: jrun-20261027-1930-c17
- Question: Implementer declared `incomplete_scenario_counts` for a no-diff sweep. Requeue or close?
- Options: Triage proposed: do not requeue; close as a no-diff success with an execution summary stating the counts are disclosed estimates.
- Chosen: Closed: Daniel forced blocked to done.
- Human note: No note recorded; the action matches the triage proposal.
- Source: blocked_task_triaged marker 19:44Z; forced event 20:06Z

### ORB-19588: Implementer declared `incomplete_code_health_sweep`. Requeue so a fresh implemen

- Date: 2026-10-28T21:41Z
- Task: ORB-19588
- Run: jrun-20261028-2111-c16
- Question: Implementer declared `incomplete_code_health_sweep`. Requeue so a fresh implementer finishes?
- Options: Triage proposed requeue so a fresh implementer re-pins and reads the remaining lines.
- Chosen: Daniel forced blocked to done instead of the proposed requeue.
- Human note: No note recorded.
- Source: blocked_task_triaged marker 21:37Z; forced event 21:41Z

### ORB-20310 and ORB-20473: ORB-20310: filing cap used, rerun to finish passes (Option 1) or accept the part

- Date: 2026-10-30T23:25Z and 2026-10-31T16:09Z
- Task: ORB-20310 and ORB-20473
- Run: jrun-20261030-2320-t2 and jrun-20261031-0920-c3
- Question: ORB-20310: filing cap used, rerun to finish passes (Option 1) or accept the partial audit (Option 2)? ORB-20473: criterion 1 unstable; evaluate the 0.67 target by the median of three paired ratios (Option 1)?
- Options: Triage options as stated.
- Chosen: Daniel forced both blocked to rejected (ORB-20473 together with ORB-20470).
- Human note: No note recorded for either. ORB-20627 was filed to make perf criteria stable.
- Source: forced events

### Forced transitions with no recorded reason

Each row is a human lifecycle override of a blocked task (`forced` history event, actor human:daniel). The task history holds no comment explaining the choice within 90 minutes before it; the question is the final-recovery diagnosis nearest before it.

| Date (UTC) | Task | Run | Question (final-recovery diagnosis) | Chosen | Human note |
| --- | --- | --- | --- | --- | --- |
| 2026-10-04 20:15 | ORB-13964 | not recorded | blocked; no diagnosis text in history | forced blocked -> done | none recorded |
| 2026-10-04 21:09 | ORB-13984 | not recorded | blocked; no diagnosis text in history | forced blocked -> proposed | none recorded |
| 2026-10-25 06:05 | ORB-18096 | jrun-20261025-0448-c8 | final recovery activity `final_recovery` did not return a decision: completion recovery required checks have not all concluded; no complete failure diagnosis is available | forced blocked -> done | none recorded |
| 2026-10-25 19:02 | ORB-18270 | jrun-20261025-1832-c3 | final recovery activity `final_recovery` did not return a decision: worktree integrity violation `worktree_mismatch`: {"code":"worktree_mismatch","reason":"workspace_path '/home/d | forced blocked -> proposed | none recorded |
| 2026-10-28 00:34 | ORB-19242 | jrun-20261027-2353-t1 | blocked; no diagnosis text in history | forced blocked -> rejected | none recorded |
| 2026-10-28 02:16 | ORB-19265 | jrun-20261028-0149-c1 | blocked; no diagnosis text in history | forced blocked -> review | none recorded |
| 2026-10-29 21:37 | ORB-19689 | jrun-20261029-0753-c30 | Run jrun-20261029-0753-c30 (the fresh ordinary repair shipment authorized by ORB-19771, comment 2026-10-29T07:51:15Z) failed at resume_candidate/candidate_resume before any implem | forced blocked -> rejected | none recorded |
| 2026-10-29 22:54 | ORB-19960 | jrun-20261029-2251-t1 | blocked; no diagnosis text in history | forced blocked -> done | none recorded |
| 2026-10-29 23:45 | ORB-19962 | jrun-20261029-2309-c3 | ORB-19962 repeatedly stops with incomplete_examination. This executor returned after 49 seconds without timeout or stall. Its run-bound automation-coverage.json remains explicitly | forced blocked -> done | none recorded |
| 2026-10-30 07:33 | ORB-20007 | jrun-20261030-0533-c3 | Exploratory grading blocked at host runtime boundary, unchanged since failure: `orbit config get assistance --json` still shows policies=[], budget epoch=unfunded, token_ceiling=0 | forced blocked -> someday | none recorded |
| 2026-10-31 01:26 | ORB-20250 | jrun-20261031-0023-c1 | blocked; no diagnosis text in history | forced blocked -> done | none recorded |
| 2026-10-31 02:20 | ORB-20339 | jrun-20261031-0153-c19 | final recovery activity `final_recovery` did not return a decision: cli invocation failed (permanent): invalid input: denyModify match `/home/daniel/workspace/constellation/codeba | forced blocked -> done | none recorded |
| 2026-10-31 08:52 | ORB-20455 | jrun-20261031-0836-c16 | ORB-20455 (no-diff tool-failure-patterns auto-task) failed at implement_bundle/implement_one: codex self-declared status=failed scan_incomplete (exit 0, timed_out=false, 57s of 3h | forced blocked -> rejected | none recorded |
| 2026-10-31 16:09 | ORB-20470 | jrun-20261031-0903-c8 | ORB-20470 has no delivery candidate. Worktree /home/daniel/workspace/constellation/codebases/orbit/.orbit/state/worktrees/orbit-jrun-20261031-0903-c8 is clean at b75de3d09b27d0774 | forced blocked -> rejected | none recorded |
