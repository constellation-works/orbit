//! Out-of-run auto-fix: an `--auto-fix` owner drain repairs an Orbit pull
//! request that no run is watching.
//!
//! A delivery leaf's completion step repairs a failed required check or a
//! merge conflict only while it runs. Once the leaf ends without completing
//! (a forge timeout, a final-recovery escalation or requeue, a dead worker),
//! its pull request can turn red or conflict with its base while nothing
//! watches it. Each pass of a drain started with `--auto-fix` therefore projects
//! task bindings from stopped delivery history without an age window, reads
//! only those tasks' current metadata, and checks each task's newest own leaf.
//! When that pull request is blocked, the pass hands the task back to the
//! leaf's completion step: a
//! checkpoint resume rewound to `pr_complete` (`plan_completion_repair_resume`)
//! with `auto_fix` granted. The watcher there admits the repair against the
//! pull request's attempt ledger, dispatches the `workflow.purpose_crews.auto_fix`
//! crew, which first takes the task's footprint locks, pushes to the same
//! branch and completes the existing pull request. The resume keeps the leaf's
//! own `completion` grant, so the repair itself never merges, and a leaf
//! that never held `--complete` still leaves the task in review.
//!
//! A task's pull request is repaired only when all of these hold:
//!
//! - its newest `task_pr_pipeline` run carries only that task, ended
//!   `failed`, `timeout` or `interrupted` (a dead worker), and is not a
//!   claimed execution. For an operator's single-task Fix PR only, a leaf
//!   that succeeded without `--complete`, and so left its pull request in
//!   review with nothing to repair it [ORB-23003], qualifies too. A leaf that
//!   merged or was cancelled is not resumed;
//! - the task is in `review`, or in `blocked` or `backlog` because that run's
//!   lineage blocked or requeued it (a delivery failure handoff, an
//!   interruption or a final-recovery escalation or requeue). An operator's
//!   block or move, and a `no-auto-approve` tag, are never overridden;
//! - nothing of the task is live: no run, and no unsettled claim;
//! - the drain holds the `--complete` grant the run was started with;
//! - the run's `pr_open` opened the pull request the task records, on an
//!   `orbit/<task>-` branch, and the forge reports it open on that branch,
//!   naming the task and authored by a delivery identity;
//! - it conflicts with its base or a required check failed on its head, and
//!   the pull request's attempt ledger would admit a repair of that head and
//!   blocker (at most one per head and blocker, three per pull request);
//! - no earlier out-of-run auto-fix resumed delivery for that head and
//!   blocker, and fewer than the ledger's budget did for the pull request, so
//!   a resume that dies before it repairs cannot loop;
//! - checkpoint resume accepts the rewound run.
//!
//! A pass starts at most as many repairs as the drain has free slots after
//! adoption. A pull request found unblocked is not read again for
//! [`pass_types::CLEAN_RECHECK`]. A forge read failure defers the pass's remaining reads;
//! GitHub's `UNKNOWN` mergeability answer skips only that pull request.
//! Nothing here ends the drain: each refusal is reported in the step output
//! and a later pass reconsiders it.
//!
//! A leaf that ended `held` on a delivery refusal a fresh forge read can
//! clear (`delivery_check_evidence_inconclusive`,
//! `delivery_completion_budget_expired`, `delivery_evidence_stale`,
//! `delivery_requirements_unsatisfied`, `delivery_base_checks_pending`,
//! `delivery_forge_observation_unavailable`) is handed back too [ORB-23809],
//! under every other rule above except the blocker: completion reads the
//! forge again, and repairs only what its own watcher then finds.
//! `delivery_check_runs_absent` is handed back the same way once the
//! unchanged head reports a pending or passed required check [ORB-24457].
//! Until that check exists the pass lists the hold under its code and spends
//! no hand-back. A held
//! leaf whose task a `completion.hold` rule keeps in review is not handed
//! back. Its pull request is repaired through the ledger when the forge
//! reports a blocker; otherwise the hand-back spends none of the ledger and
//! is bounded per held lineage by a backoff and a ceiling (`held`), after
//! which the pass reports the task under its hold code. Other holds keep
//! their own documented next step.
//!
//! The dashboard's Fix PR action starts the same repair for one task through
//! [`crate::OrbitRuntime::start_task_pr_auto_fix`] (`task_auto_fix`): the same
//! verdict, plan, start and history record, with no drain behind it.

mod pass;
mod pass_types;
mod start;
mod verdict;

pub(super) use pass_types::{PassReads, RepairAuthority, RepairObservation, Verdict};
