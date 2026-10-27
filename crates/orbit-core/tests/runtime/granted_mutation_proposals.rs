//! A mutating granted tool travels as a reviewed proposal [granted-tools §4]:
//! no live change before review, one application after the gate approves,
//! and a visible failure when application cannot happen. Every case runs on
//! a disposable workspace in a temporary directory.

use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, TaskAddParams,
    TaskUpdateParams,
};
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, MergedPullRequest, TaskPrForge};
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};
use orbit_types::task::{ExternalRef, MutationProposal, MutationReceipt, MutationReceiptState};
use orbit_types::workflow::{REVIEW_MANIFEST_ARTIFACT, ReviewManifest, ReviewTiming};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

const GRANT: &str = "orbit.auto_task.update";
const DEFINITION: &str = "proposal-target";
const PROPOSAL: &str = "mutation-proposals/describe.json";

fn granted_fixture() -> Fixture {
    granted_fixture_with(ReviewTiming::BeforePr)
}

fn granted_fixture_with(timing: ReviewTiming) -> Fixture {
    let fixture = Fixture::granted(timing, &[GRANT]);
    fixture
        .runtime
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": DEFINITION,
                "description": "before",
                "schedule": {"every_minutes": 60},
                "template": {"title": "Proposal target"},
            }),
        )
        .unwrap();
    fixture
}

/// Put `content` at `path` the way the implementing agent does: through the
/// public `orbit.task.artifact.put` tool from a scratch file.
fn put(fixture: &Fixture, path: &str, content: &Value) -> Result<Value, orbit_common::OrbitError> {
    let source = fixture.repo.join(".orbit/tmp/put.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, content.to_string()).unwrap();
    fixture.runtime.run_tool(
        "orbit.task.artifact.put",
        json!({"id": fixture.task_id, "model": "codex", "path": path, "source_path": source}),
    )
}

fn describe(description: &str) -> Value {
    json!({
        "tool": GRANT,
        "input": {"name": DEFINITION, "description": description},
        "rationale": "The definition's description is stale.",
    })
}

fn definition_description(runtime: &OrbitRuntime) -> String {
    let listed = runtime
        .run_tool("orbit.auto_task.list", json!({}))
        .unwrap()
        .to_string();
    for description in ["before", "revised", "rewritten"] {
        if listed.contains(&format!("\"description\":\"{description}\"")) {
            return description.to_string();
        }
    }
    panic!("no known description in {listed}");
}

fn artifact<T: serde::de::DeserializeOwned>(fixture: &Fixture, path: &str) -> Option<T> {
    fixture
        .runtime
        .get_task_artifact(&fixture.task_id, path)
        .unwrap()
        .map(|artifact| serde_json::from_slice(&artifact.content).unwrap())
}

fn receipt(fixture: &Fixture, proposal: &MutationProposal) -> Option<MutationReceipt> {
    artifact(
        fixture,
        &format!("mutation-receipts/{}.json", proposal.idempotency_key),
    )
}

fn admitted_proposal_changes_nothing_live_case() {
    let fixture = granted_fixture();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();

    let proposal: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    assert_eq!(proposal.task_id, fixture.task_id);
    assert_eq!(proposal.tool, GRANT);
    assert_eq!(proposal.input["description"], "revised");
    assert_eq!(proposal.idempotency_key.len(), 64, "{proposal:?}");
    assert_eq!(definition_description(&fixture.runtime), "before");

    // An inner field-level gate still applies: the grant is not operator
    // authority.
    let error = put(
        &fixture,
        "mutation-proposals/toggle.json",
        &json!({
            "tool": GRANT,
            "input": {"name": DEFINITION, "enabled": false, "expected_enabled": true, "workspace": "repo"},
            "rationale": "Disable it.",
        }),
    )
    .expect_err("expected_enabled needs orbit.routine.control");
    assert!(
        error.to_string().contains("orbit.routine.control"),
        "{error}"
    );
    let error = put(
        &fixture,
        "mutation-proposals/mint.json",
        &json!({"tool": "orbit.auto_task.add", "input": {"name": "x"}, "rationale": "r"}),
    )
    .expect_err("an ungranted tool cannot be proposed");
    assert!(error.to_string().contains("does not grant"), "{error}");
    let error = put(
        &fixture,
        &format!("mutation-receipts/{}.json", proposal.idempotency_key),
        &json!({"state": "applied"}),
    )
    .expect_err("receipts are the pipeline's");
    assert!(error.to_string().contains("reserved"), "{error}");
    assert!(receipt(&fixture, &proposal).is_none());
}

fn approved_proposal_applies_once_case() {
    let mut fixture = granted_fixture();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    let proposal: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    fixture.admit();
    let manifest: ReviewManifest = artifact(&fixture, REVIEW_MANIFEST_ARTIFACT).unwrap();
    assert_eq!(manifest.mutation_proposals.len(), 1, "{manifest:?}");
    assert_eq!(manifest.mutation_proposals[0].artifact, PROPOSAL);
    assert_eq!(manifest.mutation_proposals[0].proposal, proposal);
    assert_eq!(definition_description(&fixture.runtime), "before");

    fixture.report("accept");
    fixture.settle().unwrap();
    let applied = receipt(&fixture, &proposal).expect("an applied receipt");
    assert_eq!(applied.state, MutationReceiptState::Applied, "{applied:?}");
    assert_eq!(applied.applied_input, proposal.input);
    assert_eq!(applied.proposal_artifact, PROPOSAL);
    assert_eq!(definition_description(&fixture.runtime), "revised");

    // A replayed settlement, and a retried run re-emitting the same proposal,
    // apply nothing again.
    fixture.settle().unwrap();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    fixture.settle().unwrap();
    assert_eq!(receipt(&fixture, &proposal), Some(applied));

    // A changed proposal is a new key the settled review never saw.
    put(&fixture, PROPOSAL, &describe("rewritten")).unwrap();
    let error = fixture
        .settle()
        .expect_err("an unreviewed proposal never applies");
    assert!(
        error.to_string().contains("mutation_application_failed"),
        "{error}"
    );
    assert!(
        error.to_string().contains("after review attempt"),
        "{error}"
    );
    assert_eq!(definition_description(&fixture.runtime), "revised");
    let changed: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    assert_ne!(changed.idempotency_key, proposal.idempotency_key);
    assert!(receipt(&fixture, &changed).is_none());
}

fn same_key_paths_apply_once_case() {
    let mut fixture = Fixture::granted(ReviewTiming::BeforePr, &["orbit.auto_task.mint"]);
    fixture
        .runtime
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": DEFINITION,
                "description": "disposable definition",
                "schedule": {"every_minutes": 60},
                "template": {"title": "Counted proposal effect"},
            }),
        )
        .unwrap();
    let draft = json!({"tool": "orbit.auto_task.mint", "input": {"name": DEFINITION}, "rationale": "First reason."});
    let mut second = draft.clone();
    second["rationale"] = json!("Another reason.");
    put(&fixture, PROPOSAL, &draft).unwrap();
    put(&fixture, "mutation-proposals/second.json", &second).unwrap();
    let proposal: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    let other: MutationProposal = artifact(&fixture, "mutation-proposals/second.json").unwrap();
    assert_eq!(proposal.idempotency_key, other.idempotency_key);
    fixture.admit();
    let manifest: ReviewManifest = artifact(&fixture, REVIEW_MANIFEST_ARTIFACT).unwrap();
    assert_eq!(manifest.mutation_proposals.len(), 2);
    fixture.report("accept");
    fixture.settle().unwrap();
    let count = || {
        fixture
            .runtime
            .list_tasks()
            .unwrap()
            .iter()
            .filter(|task| task.title == "[auto-task] Counted proposal effect")
            .count()
    };
    assert_eq!(count(), 1, "one live invocation per key");
    let applied = receipt(&fixture, &proposal).unwrap();
    assert_eq!(applied.state, MutationReceiptState::Applied);
    assert_eq!(applied.proposal_artifact, PROPOSAL);
    fixture.settle().unwrap();
    put(&fixture, "mutation-proposals/third.json", &second).unwrap();
    fixture.settle().unwrap();
    assert_eq!(count(), 1);
    assert_eq!(receipt(&fixture, &proposal), Some(applied));
}

fn failed_application_is_visible_case() {
    let mut fixture = granted_fixture();
    put(
        &fixture,
        PROPOSAL,
        &json!({
            "tool": GRANT,
            "input": {"name": "no-such-definition", "description": "revised"},
            "rationale": "Targets a definition that does not exist.",
        }),
    )
    .unwrap();
    let proposal: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    fixture.admit();
    fixture.report("accept");

    let error = fixture
        .settle()
        .expect_err("a failed application fails the gate");
    assert!(
        error.to_string().contains("mutation_application_failed"),
        "{error}"
    );
    let failed = receipt(&fixture, &proposal).expect("a failed receipt");
    assert_eq!(failed.state, MutationReceiptState::Failed, "{failed:?}");
    assert!(failed.error.is_some(), "{failed:?}");
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains("mutation_application_failed")),
        "{comments:?}"
    );
}

fn run_without_review_refuses_proposals_case() {
    let fixture = granted_fixture_with(ReviewTiming::None);
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    let error = fixture
        .runtime
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &fixture.input,
            Default::default(),
        )
        .expect_err("a run that reviews nothing cannot deliver a proposal");
    let message = error.to_string();
    assert!(
        message.contains("mutation_proposals_unreviewed"),
        "{message}"
    );
    assert!(message.contains(PROPOSAL), "{message}");
    assert_eq!(definition_description(&fixture.runtime), "before");
}

fn unapplied_proposal_blocks_completion_case() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Granted completion".into(),
            plan: "Propose the definition change.".into(),
            granted_tools: vec![GRANT.into()],
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
                    PROPOSAL,
                    describe("revised").to_string(),
                )],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }
    let error = runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .expect_err("a task is never done while its proposed change did not happen");
    assert!(error.to_string().contains(PROPOSAL), "{error}");
}

/// A plain task in `review` carrying one unapplied proposal.
fn unapplied_review_task() -> (tempfile::TempDir, OrbitRuntime, String) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Granted completion".into(),
            plan: "Propose the definition change.".into(),
            granted_tools: vec![GRANT.into()],
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
                    PROPOSAL,
                    describe("revised").to_string(),
                )],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }
    (root, runtime, task.id)
}

fn move_to_review(runtime: &OrbitRuntime, task_id: &str) {
    if runtime.get_task(task_id).unwrap().status == TaskStatus::Review {
        return;
    }
    runtime
        .update_task_with_identity(
            task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
}

fn stop_fixture_run(fixture: &Fixture) {
    let run_id = fixture.input["job_run_id"].as_str().unwrap().to_string();
    let _ = RuntimeHost::mark_job_run_running(
        &fixture.runtime,
        &run_id,
        chrono::Utc::now(),
        std::process::id(),
    );
    let _ = RuntimeHost::finalize_job_run(
        &fixture.runtime,
        &run_id,
        orbit_types::workflow::JobRunState::Success,
        chrono::Utc::now(),
        Some(1),
    );
    let _ = fixture.runtime.update_task_with_identity(
        &fixture.task_id,
        TaskUpdateParams {
            job_run_id: Some(None),
            ..Default::default()
        },
        None,
        None,
    );
}

/// A granted fixture whose proposal the review gate has applied.
fn applied_fixture() -> Fixture {
    let mut fixture = granted_fixture();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    fixture.admit();
    fixture.report("accept");
    fixture.settle().unwrap();
    assert_eq!(definition_description(&fixture.runtime), "revised");
    stop_fixture_run(&fixture);
    fixture
}

fn approve_unapplied_proposal_blocks_and_force_completes_case() {
    let (_root, runtime, task_id) = unapplied_review_task();
    let error = runtime
        .approve_task(&task_id, None, None)
        .expect_err("approve must refuse an unapplied proposal");
    assert!(error.to_string().contains(PROPOSAL), "{error}");
    assert_eq!(
        runtime.get_task(&task_id).unwrap().status,
        TaskStatus::Review
    );
    let human = runtime
        .clone()
        .with_actor(orbit_core::ActorIdentity::human("human:operator"));
    human
        .force_update_task_with_identity(
            &task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(human.get_task(&task_id).unwrap().status, TaskStatus::Done);
}

fn automation_done_unapplied_proposal_blocks_case() {
    let (_root, runtime, task_id) = unapplied_review_task();
    let error = RuntimeHost::apply_task_automation_update(
        &runtime,
        &task_id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Done),
            ..Default::default()
        },
    )
    .expect_err("automation done must refuse an unapplied proposal");
    assert!(error.to_string().contains(PROPOSAL), "{error}");
    assert_eq!(
        runtime.get_task(&task_id).unwrap().status,
        TaskStatus::Review
    );
}

fn final_recovery_done_unapplied_proposal_blocks_case() {
    let (root, runtime, task_id) = unapplied_review_task();
    let repo = root.path().join("repo");
    let git = |args: &[&str]| {
        let mut command = crate::git_test_support::command();
        let output = command.args(args).current_dir(&repo).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "Orbit Test"]);
    git(&["config", "user.email", "orbit-test@example.com"]);
    git(&["commit", "--allow-empty", "-q", "-m", "landed"]);
    let commit = git(&["rev-parse", "HEAD"]);
    let task = runtime.get_task(&task_id).unwrap();
    let observed = runtime.final_recovery_revision(&task).unwrap();
    let error = runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: task_id.clone(),
                run_id: "jrun-fr-unapplied".to_string(),
                observed,
                repo_root: repo.clone(),
                base_ref: "main".to_string(),
                completion: FinalRecoveryCompletion::Done,
                requeue_bound: Default::default(),
            },
            Some(&json!({
                "decision": "complete_no_diff",
                "evidence_commit": commit,
                "rationale": "already landed"
            })),
        )
        .expect_err("final recovery to done must refuse an unapplied proposal");
    assert!(error.to_string().contains(PROPOSAL), "{error}");
    assert_eq!(
        runtime.get_task(&task_id).unwrap().status,
        TaskStatus::Review
    );
}

/// An already-done task is not a completion. Final recovery records the
/// terminal refusal even while a mutation proposal is still unapplied.
fn final_recovery_refuses_already_done_unapplied_proposal_case() {
    let (root, runtime, task_id) = unapplied_review_task();
    let human = runtime
        .clone()
        .with_actor(orbit_core::ActorIdentity::human("human:operator"));
    human
        .force_update_task_with_identity(
            &task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("a human force still completes a task that has an unapplied proposal");
    assert_eq!(runtime.get_task(&task_id).unwrap().status, TaskStatus::Done);
    let repo = root.path().join("repo");
    let git = |args: &[&str]| {
        let mut command = crate::git_test_support::command();
        let output = command.args(args).current_dir(&repo).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "Orbit Test"]);
    git(&["config", "user.email", "orbit-test@example.com"]);
    git(&["commit", "--allow-empty", "-q", "-m", "landed"]);
    let commit = git(&["rev-parse", "HEAD"]);
    let task = runtime.get_task(&task_id).unwrap();
    assert!(
        task.relations.is_empty(),
        "the fixture's resolves are workspace-local because it names none"
    );
    assert!(task.job_run_id.is_none(), "the completion run is stopped");
    let observed = runtime.final_recovery_revision(&task).unwrap();
    let outcome = runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: task_id.clone(),
                run_id: "jrun-fr-already-done".to_string(),
                observed,
                repo_root: repo,
                base_ref: "main".to_string(),
                completion: FinalRecoveryCompletion::Done,
                requeue_bound: Default::default(),
            },
            Some(&json!({
                "decision": "complete_no_diff",
                "evidence_commit": commit,
                "rationale": "already landed"
            })),
        )
        .unwrap_or_else(|error| {
            panic!("an already-done task must be refused, not denied: {error}")
        });
    let FinalRecoveryOutcome::Refused { reason } = outcome else {
        panic!("an already-done task must be refused, got {outcome:?}");
    };
    assert!(
        reason.contains("already done")
            && reason.contains("final recovery never reopens settled work"),
        "{reason}"
    );
    assert_eq!(runtime.get_task(&task_id).unwrap().status, TaskStatus::Done);
    let comments = runtime.get_task_comments(&task_id).unwrap();
    assert!(
        comments.iter().any(|comment| {
            comment.message.contains("outcome=refused") && comment.message.contains(&reason)
        }),
        "the refusal comment must be recorded: {comments:?}"
    );
}

#[derive(Clone)]
struct MergedForge {
    merged: MergedPullRequest,
}

impl TaskPrForge for MergedForge {
    fn open_pull_requests(
        &self,
        _repo_root: &std::path::Path,
    ) -> Result<Vec<ForgePullRequest>, orbit_common::OrbitError> {
        Ok(Vec::new())
    }
    fn merged_pull_requests(
        &self,
        _repo_root: &std::path::Path,
    ) -> Result<Vec<MergedPullRequest>, orbit_common::OrbitError> {
        Ok(vec![self.merged.clone()])
    }
    fn merged_pull_request(
        &self,
        _repo_root: &std::path::Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, orbit_common::OrbitError> {
        Ok((self.merged.pull.number == number).then(|| self.merged.clone()))
    }
    fn authenticated_login(
        &self,
        _repo_root: &std::path::Path,
    ) -> Result<String, orbit_common::OrbitError> {
        Ok("orbit-bot".to_string())
    }
    fn close_pull_request(
        &self,
        _repo_root: &std::path::Path,
        _number: u64,
        _comment: &str,
    ) -> Result<(), orbit_common::OrbitError> {
        Ok(())
    }
}

fn external_merge_runtime(fixture: &Fixture) -> OrbitRuntime {
    stop_fixture_run(fixture);
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &fixture.task_id,
        TaskAutomationUpdate {
            external_refs: vec![ExternalRef::github_pr("7".to_string()).unwrap()],
            ..Default::default()
        },
    )
    .unwrap();
    let base = fixture.runtime.workspace_base_branch().to_string();
    let merged = MergedPullRequest {
        pull: ForgePullRequest {
            number: 7,
            head_branch: format!("orbit/{}-47642870", fixture.task_id),
            body: format!("Delivers {}.", fixture.task_id),
            author: "orbit-bot".to_string(),
        },
        base_branch: base,
        head_sha: "0dbd8e2000000000000000000000000000000000".to_string(),
        merge_commit: "03f7af7d6a1b2c3d4e5f60718293a4b5c6d7e8f9".to_string(),
    };
    fixture
        .runtime
        .clone()
        .with_task_pr_forge(std::sync::Arc::new(MergedForge { merged }))
}

fn external_merge_unapplied_proposal_blocks_case() {
    let fixture = granted_fixture();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    let runtime = external_merge_runtime(&fixture);
    let error = runtime
        .settle_externally_merged_task(&fixture.task_id)
        .expect_err("external-merge settlement must refuse an unapplied proposal");
    assert!(error.to_string().contains(PROPOSAL), "{error}");
    assert_ne!(
        runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Done
    );
}

fn applied_proposals_still_complete_case() {
    // approve
    let fixture = applied_fixture();
    move_to_review(&fixture.runtime, &fixture.task_id);
    fixture
        .runtime
        .approve_task(&fixture.task_id, None, None)
        .unwrap();
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Done
    );

    // automation done
    let fixture = applied_fixture();
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &fixture.task_id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Done),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Done
    );

    // task_complete (activity update from review)
    let fixture = applied_fixture();
    move_to_review(&fixture.runtime, &fixture.task_id);
    let run_id = fixture.input["job_run_id"].as_str().unwrap().to_string();
    RuntimeHost::update_task_from_activity(
        &fixture.runtime,
        &fixture.task_id,
        TaskActivityUpdate {
            status: TaskStatus::Done,
            expected_status: TaskStatus::Review,
            execution_summary: None,
            comment: None,
            note: Some("completion authorized".to_string()),
            agent: None,
            model: None,
            calling_run_id: Some(run_id),
        },
    )
    .unwrap();
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Done
    );

    // external-merge settlement
    let fixture = applied_fixture();
    let runtime = external_merge_runtime(&fixture);
    runtime
        .settle_externally_merged_task(&fixture.task_id)
        .unwrap();
    assert_eq!(
        runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Done
    );

    // final recovery to done
    let fixture = applied_fixture();
    let git = |args: &[&str]| {
        let mut command = crate::git_test_support::command();
        let output = command
            .args(args)
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let commit = git(&["rev-parse", "main"]);
    move_to_review(&fixture.runtime, &fixture.task_id);
    let task = fixture.runtime.get_task(&fixture.task_id).unwrap();
    let observed = fixture.runtime.final_recovery_revision(&task).unwrap();
    fixture
        .runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: fixture.task_id.clone(),
                run_id: "jrun-fr-applied".to_string(),
                observed,
                repo_root: fixture.repo.clone(),
                base_ref: "main".to_string(),
                completion: FinalRecoveryCompletion::Done,
                requeue_bound: Default::default(),
            },
            Some(&json!({
                "decision": "complete_no_diff",
                "evidence_commit": commit,
                "rationale": "already landed"
            })),
        )
        .unwrap();
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Done
    );
}

crate::scenario_table!(
    granted_mutation_proposal_scenarios,
    admitted_proposal_changes_nothing_live_case,
    approved_proposal_applies_once_case,
    same_key_paths_apply_once_case,
    failed_application_is_visible_case,
    run_without_review_refuses_proposals_case,
    unapplied_proposal_blocks_completion_case,
    approve_unapplied_proposal_blocks_and_force_completes_case,
    automation_done_unapplied_proposal_blocks_case,
    final_recovery_done_unapplied_proposal_blocks_case,
    final_recovery_refuses_already_done_unapplied_proposal_case,
    external_merge_unapplied_proposal_blocks_case,
    applied_proposals_still_complete_case
);
