//! End-to-end scoped automation through the real pipeline boundaries: the
//! grant-bound drain, atomic child admission with inheritance, promotion on
//! fresh evidence, aggregate recovery budgets, completion rechecks, and
//! replacement windows [ORB-11332].

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::OperationLayer;
use orbit_engine::{RuntimeHost, StepRecoveryAdmission, TaskAutomationUpdate};
use orbit_tools::ToolContext;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::automation::members::{MemberAssessment, MemberState};
use orbit_types::workflow::automation::{AutomationState, SourceRevision};
use orbit_types::workflow::{
    GrantRights, JobRun, JobRunState, OPERATION_ADMISSION_KEY, OperationAdmission, OperationGrant,
};
use serde_json::{Value, json};

use super::{Fixture, MACHINE, WorkerOverride, fixture, git, seed_task};
use crate::OrbitRuntime;
use crate::application::automation::preparation;
use crate::application::job::DrainWorkerLimitRequest;
use crate::application::job::pipeline::{ChildPipelineAdmission, ChildSubmission};
use crate::application::operation::{
    EnableOperationGrantRequest, OperationDrainRequest, OperationGrantControlRequest,
    triage_recovery_reservation,
};

const AUTONOMOUS_DONE: &str = "[operation]\npreset = \"autonomous\"\ndelivery_cap = \"done\"\n";

fn enable(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    rights: GrantRights,
    run_layer: OperationLayer,
) -> OperationGrant {
    runtime
        .enable_operation_grant(EnableOperationGrantRequest {
            task_ids,
            window_seconds: 3600,
            rights,
            run_layer,
            actor: "tester",
            source: "unit",
            claim_token: None,
        })
        .expect("enable grant")
}

fn all_rights() -> GrantRights {
    GrantRights {
        prepare: true,
        promote: true,
        complete: true,
    }
}

fn stop(runtime: &OrbitRuntime, grant_id: &str) {
    runtime
        .stop_operation_grant(OperationGrantControlRequest {
            grant_id: Some(grant_id),
            reason: Some("unit"),
            expected_revision: None,
            actor: "tester",
            source: "unit",
            claim_token: None,
        })
        .expect("stop grant");
}

fn revoke(runtime: &OrbitRuntime, grant_id: &str) {
    runtime
        .revoke_operation_grant(OperationGrantControlRequest {
            grant_id: Some(grant_id),
            reason: Some("unit"),
            expected_revision: None,
            actor: "tester",
            source: "unit",
            claim_token: None,
        })
        .expect("revoke grant");
}

fn start_drain(runtime: &OrbitRuntime, grant_id: &str) -> (JobRun, OperationAdmission) {
    let _worker = WorkerOverride::install();
    let result = runtime
        .submit_operation_drain(OperationDrainRequest {
            complexity_crews: &Default::default(),
            grant_id: Some(grant_id),
            for_seconds: Some(600),
            max_active_leaf_runs: None,
            allowed_crews: &[],
            actor: Some("tester"),
            claim_token: None,
        })
        .expect("submit grant-bound drain");
    let run = runtime
        .show_job_run(&result.invoke.run_id)
        .expect("drain run");
    (run, result.admission)
}

fn admit_leaf(runtime: &OrbitRuntime, parent: &str, task_id: &str) -> ChildSubmission {
    let _worker = WorkerOverride::install();
    runtime
        .submit_child_pipeline_run(
            "task_auto_pipeline",
            json!({ "task_ids": [task_id], "mode": "local", "base_branch": "main" }),
            None,
            Some("tester"),
            &ChildPipelineAdmission {
                parent_run_id: parent.to_string(),
                parent_step_id: Some("ship_leaves".to_string()),
                action: "invoke_detached".to_string(),
                blocking: false,
            },
        )
        .expect("child submission")
}

fn skipped_reason(submission: ChildSubmission) -> String {
    match submission {
        ChildSubmission::Skipped(reason) => reason,
        ChildSubmission::Submitted(result) => panic!("expected a skip, got run {}", result.run_id),
    }
}

fn submitted_run(runtime: &OrbitRuntime, submission: ChildSubmission) -> JobRun {
    match submission {
        ChildSubmission::Submitted(result) => runtime.show_job_run(&result.run_id).expect("run"),
        ChildSubmission::Skipped(reason) => panic!("expected a child, got skip {reason}"),
    }
}

fn classify(runtime: &OrbitRuntime, parent: &str) -> Value {
    runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({ "run_id": parent }),
            ToolContext::default(),
        )
        .expect("classify")
}

fn window(runtime: &OrbitRuntime, parent: &str) -> Value {
    runtime
        .run_deterministic(
            "drain_window",
            &json!({}),
            &json!({ "run_id": parent, "for_seconds": 600 }),
            ToolContext::default(),
        )
        .expect("window")
}

/// Seed the accepted preparation assessment the shared evaluator would have
/// committed for `task_id`, bound to `fingerprint`.
fn seed_assessment(fixture: &Fixture, task_id: &str, fingerprint: &str, ready: bool) {
    let runtime = &fixture.runtime;
    let consumer = format!(
        "{MACHINE}/{}/routine/state-pilot",
        runtime.workspace_id().expect("workspace id")
    );
    let revision = SourceRevision {
        commit: git(&fixture.repo, &["rev-parse", "HEAD"]),
        tree: git(&fixture.repo, &["rev-parse", "HEAD^{tree}"]),
    };
    let store = runtime.automation_store().expect("automation store");
    let mut state = match store.automation_state(&consumer).expect("state") {
        Some(state) => state,
        None => {
            // The store admits only a clean baseline; the accepted record is
            // then written the way a committed checkpoint would leave it.
            let baseline = AutomationState {
                members: Some(MemberState::default()),
                consumer: consumer.clone(),
                epoch: "epoch".to_string(),
                repository: "repo".to_string(),
                branch: "main".to_string(),
                generation: 0,
                baseline: revision.clone(),
                observed: revision.clone(),
                covered: revision,
                pending_commits: Vec::new(),
                pending: Vec::new(),
                waived: Vec::new(),
                excluded: Vec::new(),
                unresolved: BTreeMap::new(),
                associations: BTreeMap::new(),
                active: None,
            };
            assert!(store.automation_initialize(&baseline).expect("initialize"));
            baseline
        }
    };
    state
        .members
        .get_or_insert_with(MemberState::default)
        .assessed
        .insert(
            task_id.to_string(),
            MemberAssessment {
                input_fingerprint: fingerprint.to_string(),
                resulting_fingerprint: fingerprint.to_string(),
                ready,
                receipt_id: format!("receipt-{task_id}"),
            },
        );
    let db = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).expect("db");
    db.execute(
        "UPDATE automation_consumers SET state_json = ?2 WHERE consumer = ?1",
        rusqlite::params![consumer, serde_json::to_string(&state).expect("encode")],
    )
    .expect("write seeded assessment");
}

fn current_fingerprint(fixture: &Fixture, task_id: &str) -> String {
    let runtime = &fixture.runtime;
    let task = runtime.get_task(task_id).expect("task");
    let head = preparation::head_revision(runtime, "main").expect("head");
    preparation::fingerprint(runtime, &task, &head).expect("fingerprint")
}

#[test]
fn grant_bound_drain_inherits_the_snapshot_and_rechecks_the_grant_per_child() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let a = seed_task(runtime, "in scope A", TaskStatus::Backlog);
    let b = seed_task(runtime, "in scope B", TaskStatus::Backlog);
    let outside = seed_task(runtime, "outside", TaskStatus::Backlog);
    let grant = enable(
        runtime,
        &[a.id.clone(), b.id.clone()],
        all_rights(),
        OperationLayer {
            leaf_ceiling: Some(2),
            ..OperationLayer::default()
        },
    );

    let (drain, admission) = start_drain(runtime, &grant.id);
    let drain_input = drain.input.clone().expect("drain input");
    assert_eq!(
        drain_input["completion"], "done",
        "cap done + complete right"
    );
    assert_eq!(drain_input["max_active_leaf_runs"], 2);
    assert_eq!(drain_input["for_seconds"], 600);
    assert_eq!(admission.grant_id, grant.id);
    assert_eq!(admission.completion, "done");
    assert_eq!(
        OperationAdmission::from_run_input(&drain_input).expect("snapshot"),
        Some(admission.clone())
    );

    // The child inherits exactly the parent's snapshot.
    let child = submitted_run(runtime, admit_leaf(runtime, &drain.run_id, &a.id));
    let child_input = child.input.clone().expect("child input");
    assert_eq!(
        OperationAdmission::from_run_input(&child_input).expect("snapshot"),
        Some(admission.clone())
    );
    assert_eq!(child_input["task_ids"], json!([a.id]));

    // Scope, claim, and ordinary-input refusals.
    assert_eq!(
        skipped_reason(admit_leaf(runtime, &drain.run_id, &outside.id)),
        "outside_grant_scope"
    );
    assert_eq!(
        skipped_reason(admit_leaf(runtime, &drain.run_id, &a.id)),
        "task_claimed"
    );
    let forged = runtime
        .submit_pipeline_run(
            "task_auto_pipeline",
            json!({ "task_ids": [b.id], OPERATION_ADMISSION_KEY: admission }),
            None,
            Some("tester"),
        )
        .expect_err("ordinary input cannot name the reserved key");
    assert!(
        forged.to_string().contains("reserved `operation` field"),
        "{forged}"
    );

    // The classifier offers only in-scope work, bounded by the captured ceiling.
    let before = classify(runtime, &drain.run_id);
    assert_eq!(before["operation"]["admission"], "open");
    assert_eq!(before["max_active_leaf_runs"], 2);
    assert_eq!(before["loose_task_ids"], json!([b.id]));

    // Stop lands after the coordinator observed eligibility for B. The grant
    // stop propagates to the bound coordinator's own admissions control, so
    // the stale admission is refused at the store boundary and nothing is
    // created. (The grant-level refusal for a coordinator whose control was
    // not written is covered by the Store's admission tests.)
    stop(runtime, &grant.id);
    assert_eq!(
        skipped_reason(admit_leaf(runtime, &drain.run_id, &b.id)),
        "admissions_stopped"
    );
    assert!(runtime.drain_admissions_stopped(&drain.run_id));
    let after = classify(runtime, &drain.run_id);
    assert_eq!(after["operation"]["admission"], "grant_stopped");
    assert_eq!(after["free_slots"], 0);
    assert_eq!(after["loose_task_ids"], json!([]));
    // The coordinator's own stop control wins the window's reason; the grant
    // state stays visible through the classifier report above.
    let closed = window(runtime, &drain.run_id);
    assert_eq!(closed["expired"], true);
    assert_eq!(closed["expired_reason"], "admissions_stopped");
    // Stop is not cancellation: the admitted child is untouched.
    assert!(
        !runtime
            .show_job_run(&child.run_id)
            .expect("child")
            .state
            .is_terminal()
    );
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .list_job_runs("task_auto_pipeline")
            .expect("children")
            .len(),
        1
    );
}

#[test]
fn promotion_needs_fresh_positive_evidence_and_the_promote_right() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let fresh = seed_task(runtime, "fresh", TaskStatus::Proposed);
    let stale = seed_task(runtime, "stale", TaskStatus::Proposed);
    let unready = seed_task(runtime, "unready", TaskStatus::Proposed);
    let missing = seed_task(runtime, "missing", TaskStatus::Proposed);
    seed_assessment(
        &fixture,
        &fresh.id,
        &current_fingerprint(&fixture, &fresh.id),
        true,
    );
    seed_assessment(
        &fixture,
        &stale.id,
        "fingerprint-from-an-older-task-meaning",
        true,
    );
    seed_assessment(
        &fixture,
        &unready.id,
        &current_fingerprint(&fixture, &unready.id),
        false,
    );
    let scope = vec![
        fresh.id.clone(),
        stale.id.clone(),
        unready.id.clone(),
        missing.id.clone(),
    ];

    let grant = enable(runtime, &scope, all_rights(), OperationLayer::default());
    let (drain, _) = start_drain(runtime, &grant.id);
    let report = classify(runtime, &drain.run_id);
    let decisions = report["operation"]["promotions"]
        .as_array()
        .expect("promotions")
        .iter()
        .map(|decision| {
            (
                decision["task_id"].as_str().expect("id").to_string(),
                decision["reason"].as_str().expect("reason").to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(decisions[&fresh.id], "fresh_positive_assessment");
    assert_eq!(decisions[&stale.id], "assessment_stale");
    assert_eq!(decisions[&unready.id], "assessment_unready");
    assert_eq!(decisions[&missing.id], "assessment_missing");

    let promoted = runtime.get_task(&fresh.id).expect("task");
    assert_eq!(promoted.status, TaskStatus::Backlog);
    assert!(
        runtime
            .get_task_history(&fresh.id)
            .expect("history")
            .iter()
            .any(|entry| entry.event == "operation_promoted")
    );
    assert_eq!(
        runtime.get_task(&stale.id).expect("task").status,
        TaskStatus::Proposed
    );
    // The promoted task is offered by the same iteration; the rest are not.
    assert_eq!(report["loose_task_ids"], json!([fresh.id]));
    // Promotion is idempotent: a second pass has nothing left to decide for it.
    let again = classify(runtime, &drain.run_id);
    assert!(
        !again["operation"]["promotions"]
            .as_array()
            .expect("promotions")
            .iter()
            .any(|decision| decision["task_id"] == fresh.id)
    );

    // Without the promote right, fresh evidence alone promotes nothing.
    stop(runtime, &grant.id);
    let second = seed_task(runtime, "second fresh", TaskStatus::Proposed);
    seed_assessment(
        &fixture,
        &second.id,
        &current_fingerprint(&fixture, &second.id),
        true,
    );
    let narrow = enable(
        runtime,
        std::slice::from_ref(&second.id),
        GrantRights {
            prepare: true,
            promote: false,
            complete: false,
        },
        OperationLayer::default(),
    );
    let (narrow_drain, _) = start_drain(runtime, &narrow.id);
    let report = classify(runtime, &narrow_drain.run_id);
    assert_eq!(
        report["operation"]["promotions"][0]["reason"],
        "promote_right_missing"
    );
    assert_eq!(
        runtime.get_task(&second.id).expect("task").status,
        TaskStatus::Proposed
    );
}

#[test]
fn recovery_budget_spans_step_hooks_and_triage_and_escalates_when_spent() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "recoverable", TaskStatus::Backlog);
    let grant = enable(
        runtime,
        std::slice::from_ref(&task.id),
        all_rights(),
        OperationLayer {
            recovery_episodes_per_task: Some(2),
            ..OperationLayer::default()
        },
    );
    let (drain, _) = start_drain(runtime, &grant.id);
    let child = submitted_run(runtime, admit_leaf(runtime, &drain.run_id, &task.id));

    // A run without a snapshot keeps the unbounded pre-existing behavior.
    let unbound = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": ["ORB-0"] })),
            None,
        )
        .expect("unbound run");
    assert_eq!(
        runtime
            .authorize_step_recovery(&unbound.run_id, "sync_base")
            .expect("unbound"),
        StepRecoveryAdmission::Allowed
    );

    // First episode: a step hook. A retry of the same step reuses it.
    assert_eq!(
        runtime
            .authorize_step_recovery(&child.run_id, "sync_base")
            .expect("first"),
        StepRecoveryAdmission::Reserved { episode: 1 }
    );
    assert_eq!(
        runtime
            .authorize_step_recovery(&child.run_id, "sync_base")
            .expect("same step"),
        StepRecoveryAdmission::Reserved { episode: 1 }
    );
    runtime
        .settle_step_recovery(&child.run_id, "sync_base", 90)
        .expect("settle");

    // Second episode: terminal-run triage shares the same lineage budget.
    let reservation = triage_recovery_reservation(runtime, &task.id, &child)
        .expect("triage reservation")
        .expect("bound run");
    assert_eq!(reservation.episode, Some(2));
    assert_eq!(reservation.exhausted, None);

    // Third: spent. The task carries a durable escalation and is not retried.
    assert_eq!(
        runtime
            .authorize_step_recovery(&child.run_id, "push")
            .expect("third"),
        StepRecoveryAdmission::Denied {
            reason: "recovery_episodes_exhausted".to_string()
        }
    );
    let history = runtime.get_task_history(&task.id).expect("history");
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.event == "recovery_budget_exhausted")
            .count(),
        1
    );
    // The open triage episode is reused for the same run until it settles;
    // once settled, the lineage has nothing left for a later failure.
    let reused = triage_recovery_reservation(runtime, &task.id, &child)
        .expect("triage reservation")
        .expect("bound run");
    assert_eq!(reused.episode, Some(2));
    crate::application::operation::settle_triage_episode(runtime, &task.id, &child.run_id)
        .expect("settle triage");
    let later_failure = JobRun {
        run_id: "jrun-later".to_string(),
        ..child.clone()
    };
    let exhausted_triage = triage_recovery_reservation(runtime, &task.id, &later_failure)
        .expect("triage reservation")
        .expect("bound run");
    assert_eq!(
        exhausted_triage.exhausted,
        Some("recovery_episodes_exhausted")
    );
    // Repeated denial records the escalation once.
    let _ = runtime.authorize_step_recovery(&child.run_id, "commit");
    assert_eq!(
        runtime
            .get_task_history(&task.id)
            .expect("history")
            .iter()
            .filter(|entry| entry.event == "recovery_budget_exhausted")
            .count(),
        1
    );
    let ledger = runtime
        .operation_store()
        .expect("store")
        .operation_recovery_ledger(&runtime.workspace_id().expect("ws"), &task.id)
        .expect("ledger")
        .expect("ledger exists");
    assert_eq!(ledger.episodes.len(), 2);
    assert_eq!(ledger.consumed_seconds, 90);
}

#[test]
fn recovery_git_mutation_requires_live_task_run_and_worktree_lineage() {
    let cancelled_fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &cancelled_fixture.runtime;
    let task = seed_task(runtime, "recoverable owner", TaskStatus::Backlog);
    let grant = enable(
        runtime,
        std::slice::from_ref(&task.id),
        all_rights(),
        OperationLayer::default(),
    );
    let (drain, _) = start_drain(runtime, &grant.id);
    let child = submitted_run(runtime, admit_leaf(runtime, &drain.run_id, &task.id));
    let mut state = runtime
        .read_run_state(&child.run_id)
        .expect("read child state")
        .expect("child pipeline state");
    state.sync_pipeline(json!({
        "worktree": {
            "workspace_path": cancelled_fixture.repo,
            "job_run_id": child.run_id,
        }
    }));
    runtime
        .write_run_state(&child.run_id, &state)
        .expect("checkpoint assigned worktree");

    let cancelled = runtime
        .cancel_job_run(&child.run_id)
        .expect("cancel pending child");
    assert_eq!(cancelled.final_state, "cancelled");
    let error = runtime
        .validate_step_recovery_mutation(
            &child.run_id,
            "sync_base",
            std::slice::from_ref(&task.id),
            &cancelled_fixture.repo,
        )
        .expect_err("cancelled run must not mutate Git");
    assert!(error.to_string().contains("state 'cancelled'"), "{error}");

    let other = cancelled_fixture.repo.join("other");
    std::fs::create_dir(&other).expect("other directory");
    let mismatch = runtime
        .validate_step_recovery_mutation(
            &child.run_id,
            "sync_base",
            std::slice::from_ref(&task.id),
            &other,
        )
        .expect_err("cancelled state remains authoritative before path mismatch");
    assert!(
        mismatch.to_string().contains("state 'cancelled'"),
        "{mismatch}"
    );

    let live_fixture = fixture(AUTONOMOUS_DONE);
    let live_runtime = &live_fixture.runtime;
    let live_task = seed_task(live_runtime, "live recovery owner", TaskStatus::Backlog);
    let live_grant = enable(
        live_runtime,
        std::slice::from_ref(&live_task.id),
        all_rights(),
        OperationLayer::default(),
    );
    let (live_drain, _) = start_drain(live_runtime, &live_grant.id);
    let live_child = submitted_run(
        live_runtime,
        admit_leaf(live_runtime, &live_drain.run_id, &live_task.id),
    );
    live_runtime
        .apply_task_automation_update(
            &live_task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(live_child.run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("couple task to recovery owner");
    let live_retry = live_runtime
        .insert_job_run(
            &live_child.job_id,
            live_child.attempt + 1,
            Utc::now(),
            live_child.input.clone(),
            Some(live_child.run_id.clone()),
        )
        .expect("insert recovery retry");
    live_runtime
        .mark_job_run_running(&live_retry.run_id, Utc::now(), std::process::id())
        .expect("mark recovery retry running");
    let mut live_state = orbit_types::workflow::PipelineState::new(
        live_retry.run_id.clone(),
        live_retry.job_id.clone(),
        live_retry.input.clone().expect("retry input"),
    );
    live_state.sync_pipeline(json!({
        "worktree": {
            "workspace_path": live_fixture.repo,
            "job_run_id": live_child.run_id,
        }
    }));
    live_runtime
        .write_run_state(&live_retry.run_id, &live_state)
        .expect("checkpoint live worktree");

    live_runtime
        .validate_step_recovery_mutation(
            &live_retry.run_id,
            "sync_base",
            std::slice::from_ref(&live_task.id),
            &live_fixture.repo,
        )
        .expect("matching live ownership permits host mutation");
    let wrong_tasks = live_runtime
        .validate_step_recovery_mutation(
            &live_retry.run_id,
            "sync_base",
            &["ORB-unrelated".to_string()],
            &live_fixture.repo,
        )
        .expect_err("run task identity must match");
    assert!(
        wrong_tasks.to_string().contains("task lineage"),
        "{wrong_tasks}"
    );
    let wrong_path = live_runtime
        .validate_step_recovery_mutation(
            &live_retry.run_id,
            "sync_base",
            std::slice::from_ref(&live_task.id),
            &live_fixture.repo.join(".orbit"),
        )
        .expect_err("assigned path must match the worktree checkpoint");
    assert!(
        wrong_path.to_string().contains("does not match"),
        "{wrong_path}"
    );
    live_runtime
        .finalize_job_run(&live_retry.run_id, JobRunState::Success, Utc::now(), None)
        .expect("finalize live fixture run");
    live_runtime
        .cancel_job_run(&live_child.run_id)
        .expect("cancel source fixture run");
}

#[test]
fn completion_survives_stop_but_not_revocation_and_needs_the_right() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "deliverable", TaskStatus::Backlog);
    let grant = enable(
        runtime,
        std::slice::from_ref(&task.id),
        all_rights(),
        OperationLayer::default(),
    );
    let (drain, _) = start_drain(runtime, &grant.id);
    let child = submitted_run(runtime, admit_leaf(runtime, &drain.run_id, &task.id));
    let task_ids = vec![task.id.clone()];

    runtime
        .authorize_task_completion(&child.run_id, &task_ids)
        .expect("admitted work may complete");
    let outside = runtime
        .authorize_task_completion(&child.run_id, &["ORB-0".to_string()])
        .expect_err("outside the scope");
    assert!(matches!(outside, OrbitError::CapabilityDenied(_)));

    // Ordinary stop keeps the captured completion authority.
    stop(runtime, &grant.id);
    runtime
        .authorize_task_completion(&child.run_id, &task_ids)
        .expect("stop keeps captured bounds");

    // Hard revocation withdraws it at the transition itself.
    revoke(runtime, &grant.id);
    let refused = runtime
        .authorize_task_completion(&child.run_id, &task_ids)
        .expect_err("revoked");
    assert!(
        matches!(refused, OrbitError::CapabilityDenied(_)),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("was revoked"));

    // A drain under a grant without the complete right captures `review`
    // and cannot be talked into completing.
    let review_only = enable(
        runtime,
        std::slice::from_ref(&task.id),
        GrantRights {
            prepare: false,
            promote: true,
            complete: false,
        },
        OperationLayer::default(),
    );
    let (review_drain, admission) = start_drain(runtime, &review_only.id);
    assert_eq!(admission.completion, "review");
    assert!(
        review_drain
            .input
            .expect("input")
            .get("completion")
            .is_none()
    );
    // The unbound child from the revoked grant still claims the task, so the
    // new coordinator cannot double-claim it.
    assert_eq!(
        skipped_reason(admit_leaf(runtime, &review_drain.run_id, &task.id)),
        "task_claimed"
    );

    // Unbound runs are untouched by any of this.
    let unbound = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": ["ORB-0"] })),
            None,
        )
        .expect("unbound run");
    runtime
        .authorize_task_completion(&unbound.run_id, &["ORB-0".to_string()])
        .expect("unbound runs keep --complete semantics");
}

#[test]
fn live_concurrency_changes_narrow_but_never_widen_the_captured_ceiling() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let a = seed_task(runtime, "one", TaskStatus::Backlog);
    let b = seed_task(runtime, "two", TaskStatus::Backlog);
    let c = seed_task(runtime, "three", TaskStatus::Backlog);
    let grant = enable(
        runtime,
        &[a.id.clone(), b.id.clone(), c.id.clone()],
        all_rights(),
        OperationLayer {
            leaf_ceiling: Some(2),
            ..OperationLayer::default()
        },
    );
    let (drain, _) = start_drain(runtime, &grant.id);

    // Raising the live ceiling past the grant changes nothing: the grant caps it.
    runtime
        .set_drain_worker_limit(DrainWorkerLimitRequest {
            run_id: &drain.run_id,
            max_active_leaf_runs: 5,
            expected_revision: None,
            reason: None,
            actor: "tester",
            source: "unit",
            claim_token: None,
        })
        .expect("raise live limit");
    submitted_run(runtime, admit_leaf(runtime, &drain.run_id, &a.id));
    submitted_run(runtime, admit_leaf(runtime, &drain.run_id, &b.id));
    assert_eq!(
        skipped_reason(admit_leaf(runtime, &drain.run_id, &c.id)),
        "capacity_saturated"
    );

    // Lowering it narrows immediately, without cancelling anything.
    runtime
        .set_drain_worker_limit(DrainWorkerLimitRequest {
            run_id: &drain.run_id,
            max_active_leaf_runs: 1,
            expected_revision: None,
            reason: None,
            actor: "tester",
            source: "unit",
            claim_token: None,
        })
        .expect("lower live limit");
    assert_eq!(
        skipped_reason(admit_leaf(runtime, &drain.run_id, &c.id)),
        "capacity_saturated"
    );
    assert_eq!(classify(runtime, &drain.run_id)["max_active_leaf_runs"], 1);
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .list_pending_or_running_job_runs("task_auto_pipeline")
            .expect("children")
            .len(),
        2,
        "narrowing cancels no child"
    );
}

#[test]
fn ordinary_expiry_closes_admission_and_promotion_but_keeps_captured_completion() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "expiring", TaskStatus::Backlog);
    let proposed = seed_task(runtime, "late proposal", TaskStatus::Proposed);
    let grant = runtime
        .enable_operation_grant(EnableOperationGrantRequest {
            task_ids: &[task.id.clone(), proposed.id.clone()],
            window_seconds: 1,
            rights: all_rights(),
            run_layer: OperationLayer::default(),
            actor: "tester",
            source: "unit",
            claim_token: None,
        })
        .expect("enable short grant");
    let (drain, admission) = start_drain(runtime, &grant.id);
    let child = submitted_run(runtime, admit_leaf(runtime, &drain.run_id, &task.id));
    seed_assessment(
        &fixture,
        &proposed.id,
        &current_fingerprint(&fixture, &proposed.id),
        true,
    );

    std::thread::sleep(std::time::Duration::from_millis(1_200));

    // Expiry is derived from the absolute deadline: no status write happened.
    assert_eq!(
        runtime.operation_grant(&grant.id).expect("grant").revision,
        1
    );
    assert_eq!(
        skipped_reason(admit_leaf(runtime, &drain.run_id, &proposed.id)),
        "grant_expired"
    );
    let report = classify(runtime, &drain.run_id);
    assert_eq!(report["operation"]["admission"], "grant_expired");
    assert_eq!(report["free_slots"], 0);
    assert!(
        report["operation"]["promotions"]
            .as_array()
            .expect("promotions")
            .is_empty(),
        "expired evidence cannot authorize a new promotion"
    );
    assert_eq!(
        runtime.get_task(&proposed.id).expect("task").status,
        TaskStatus::Proposed
    );
    assert_eq!(
        window(runtime, &drain.run_id)["expired_reason"],
        "grant_expired"
    );

    // Admitted work retains its captured policy after the window.
    assert_eq!(admission.completion, "done");
    runtime
        .authorize_task_completion(&child.run_id, std::slice::from_ref(&task.id))
        .expect("expiry keeps captured completion");
    // And a new window is not implied: the workspace has no active grant.
    assert!(runtime.active_operation_grant().expect("active").is_none());
}

#[test]
fn grant_bound_drain_uses_complexity_override_without_widening_authority() {
    let fixture = fixture("[workflow]\nmedium_complexity_crews = [\"grok\"]\n");
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "Grant crew pool fixture", TaskStatus::Backlog);
    runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                complexity: Some(orbit_types::task::TaskComplexity::Medium),
                ..Default::default()
            },
        )
        .expect("assess complexity");
    let grant = enable(
        runtime,
        std::slice::from_ref(&task.id),
        GrantRights {
            prepare: true,
            promote: true,
            complete: false,
        },
        OperationLayer::default(),
    );
    let _worker = WorkerOverride::install();
    let drain = runtime
        .submit_operation_drain(OperationDrainRequest {
            grant_id: Some(&grant.id),
            for_seconds: Some(600),
            max_active_leaf_runs: Some(1),
            allowed_crews: &["terra".into()],
            complexity_crews: &orbit_config::ComplexityCrewPools {
                medium: Some(vec!["terra".into()]),
                ..Default::default()
            },
            actor: Some("tester"),
            claim_token: None,
        })
        .expect("grant-bound pool override");
    let ChildSubmission::Submitted(child) = admit_leaf(runtime, &drain.invoke.run_id, &task.id)
    else {
        panic!("admitted");
    };
    let input = runtime
        .get_job_run_backend(&child.run_id)
        .expect("read child")
        .expect("child")
        .input
        .expect("input");
    assert_eq!(input["crew"], "terra");
    assert_eq!(
        input["crew_selection"]["source"],
        "run_input.medium_complexity_crews"
    );
    assert_eq!(input["allowed_crews"], json!(["terra"]));
    assert_eq!(input[OPERATION_ADMISSION_KEY]["grant_id"], grant.id);
    assert_eq!(input[OPERATION_ADMISSION_KEY]["limits"]["leaf_ceiling"], 1);
    stop(runtime, &grant.id);
    assert_eq!(
        classify(runtime, &drain.invoke.run_id)["loose_task_ids"],
        json!([])
    );
}
