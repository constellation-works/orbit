//! Gate decisions remain queryable without counting them as tool failures.

use std::path::PathBuf;

use chrono::Utc;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{ReviewLandingRequest, ReviewerInvocationRequest, RuntimeHost};
use orbit_store::contracts::{FailureClass, classify};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAdmission,
    ReviewBudget, ReviewCertificate, ReviewTiming, ReviewerInvocationEvent,
};
use serde_json::{Value, json};
use tempfile::TempDir;

/// A before-PR gated task over a one-commit candidate, shared with the
/// report-revision regressions.
pub(super) struct Fixture {
    pub(super) _root: TempDir,
    pub(super) runtime: OrbitRuntime,
    pub(super) repo: PathBuf,
    pub(super) task_id: String,
    pub(super) input: Value,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("config.toml"),
            "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"reviewers\"\n[operation]\nreview_crew = \"reviewers\"\n[review]\nbefore_pr = true\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("git");
            orbit_common::test_env::clear_inherited_authority(|key| {
                command.env_remove(key);
            });
            let output = command.args(args).current_dir(&repo).output().unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Orbit Test"]);
        git(&["config", "user.email", "orbit-test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("candidate.txt"), "before\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "seed"]);
        git(&["checkout", "-b", "candidate"]);
        std::fs::write(repo.join("candidate.txt"), "after\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "candidate"]);

        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Gate audit fixture".into(),
                description: "Exercise review decisions and execution errors.".into(),
                acceptance_criteria: vec![
                    "Audit preserves decisions and distinguishes errors.".into(),
                ],
                plan: "Review candidate.txt.".into(),
                context_files: vec!["file:candidate.txt".into()],
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            })
            .unwrap();
        let policy = runtime.operation_policy();
        let admission = ReviewAdmission {
            contract_version: REVIEW_CONTRACT_VERSION,
            policy_version: policy.version,
            timing: ReviewTiming::BeforePr,
            timing_source: policy.review_before_pr.source.label().into(),
            crew: policy.review_crew.value.clone(),
            crew_source: policy.review_crew.source.label().into(),
            // A bounded fixture budget exercises exhaustion without depending
            // on the operational default.
            budget: ReviewBudget { minutes: 10 },
            captured_at: Utc::now(),
        };
        let run = runtime
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({"review": admission})),
                None,
            )
            .unwrap();
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    job_run_id: Some(Some(run.run_id.clone())),
                    ..Default::default()
                },
                Some("codex".into()),
                None,
            )
            .unwrap();
        let input = json!({
            "job_run_id": run.run_id,
            "completed_task_ids": [task.id],
            "workspace_path": repo.canonicalize().unwrap(),
            "base": "main", "base_sync": "local", "mode": "pr",
        });
        Self {
            _root: root,
            runtime,
            repo,
            task_id: task.id.to_string(),
            input,
        }
    }

    pub(super) fn admit(&mut self) {
        self.input["admission"] = self
            .runtime
            .run_deterministic(
                "review_gate_admit",
                &json!({}),
                &self.input,
                Default::default(),
            )
            .unwrap();
    }

    fn report(&self, verdict: &str) {
        self.put_report(&json!({
            "schema_version": REVIEW_CONTRACT_VERSION,
            "attempt_id": self.input["admission"]["attempt_id"],
            "verdict": verdict, "summary": "Checked candidate.",
            "findings": [],
            "validation": [{"command": "fixture check", "outcome": "passed", "role": "required"}],
            "escalation": if verdict == "accept" { None } else { Some("Reviewer cannot accept candidate.") },
        }));
    }

    /// Attach `report` the way the reviewer does: through the public
    /// `orbit.task.artifact.put` tool from a scratch file.
    pub(super) fn put_report(&self, report: &Value) {
        let path = self.repo.join(".orbit/tmp").join(REVIEW_REPORT_ARTIFACT);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, report.to_string()).unwrap();
        self.runtime.run_tool("orbit.task.artifact.put", json!({
            "id": self.task_id, "model": "codex", "path": REVIEW_REPORT_ARTIFACT, "source_path": path,
        })).unwrap();
    }

    /// Run the deterministic settlement for the admitted attempt.
    pub(super) fn settle(&self) -> Result<Value, orbit_engine::DispatchError> {
        self.runtime.run_deterministic(
            "review_gate_settle",
            &json!({}),
            &self.input,
            Default::default(),
        )
    }

    fn rows(&self) -> Vec<AuditEvent> {
        self.runtime
            .list_audit_events(None, Some("review.gate".into()), None, None, 100)
            .unwrap()
    }

    fn latest(&self, phase: &str) -> (AuditEvent, Value) {
        self.rows()
            .into_iter()
            .find_map(|row| {
                let detail: Value =
                    serde_json::from_str(row.arguments_json.as_deref().unwrap()).unwrap();
                (detail["phase"] == phase).then_some((row, detail))
            })
            .unwrap()
    }
}

#[test]
fn settled_negative_verdicts_are_successful_calls_with_decision_detail() {
    if !super::dispatch_admission::isolated(
        "review_gate_audit::settled_negative_verdicts_are_successful_calls_with_decision_detail",
    ) {
        return;
    }
    // The persisted legacy spelling is still accepted and projected using
    // the current verdict contract.
    for (report, verdict) in [("changes_required", "reject"), ("incomplete", "incomplete")] {
        let mut fixture = Fixture::new();
        fixture.admit();
        fixture.report(report);
        let settled = fixture.runtime.run_deterministic(
            "review_gate_settle",
            &json!({}),
            &fixture.input,
            Default::default(),
        );
        assert!(
            settled.is_err(),
            "a negative verdict must still stop delivery"
        );
        let (row, detail) = fixture.latest("settle");
        assert_eq!(row.status, AuditEventStatus::Success);
        assert!(row.error_message.is_none());
        assert_eq!(detail["outcome"]["verdict"], verdict);
        assert!(
            detail["outcome"]["escalation"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
        let stats = fixture
            .runtime
            .audit_event_stats(None, Some("review.gate".into()))
            .unwrap();
        assert_eq!(
            (stats.total, stats.success_count, stats.failure_count),
            (2, 2, 0)
        );
    }
}

#[test]
fn uncovered_landings_succeed_and_exhausted_admissions_are_denied() {
    if !super::dispatch_admission::isolated(
        "review_gate_audit::uncovered_landings_succeed_and_exhausted_admissions_are_denied",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    // The reviewer runs past the candidate's ten minutes without a verdict:
    // the preflight and a re-admission are both denied.
    for event in [
        ReviewerInvocationEvent::Started {
            timeout_seconds: 3600,
        },
        ReviewerInvocationEvent::Finished {
            runtime_seconds: 601,
        },
    ] {
        RuntimeHost::record_reviewer_invocation(
            &fixture.runtime,
            &ReviewerInvocationRequest {
                run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
                lineage_key: fixture.input["admission"]["lineage_key"]
                    .as_str()
                    .unwrap()
                    .into(),
                attempt_id: fixture.input["admission"]["attempt_id"]
                    .as_str()
                    .unwrap()
                    .into(),
                event,
            },
        )
        .unwrap();
    }
    for preflight in [false, true] {
        let mut input = fixture.input.clone();
        input["preflight"] = json!(preflight);
        assert!(
            fixture
                .runtime
                .run_deterministic("review_gate_admit", &json!({}), &input, Default::default())
                .is_err()
        );
        let (row, detail) = fixture.latest(if preflight { "preflight" } else { "admit" });
        assert_eq!(row.status, AuditEventStatus::Denied);
        assert!(
            row.error_message
                .as_deref()
                .is_some_and(|message| !message.trim().is_empty())
        );
        assert!(
            row.error_message
                .as_deref()
                .unwrap()
                .contains("review_budget_exhausted")
        );
        assert_eq!(classify(&row), FailureClass::Denied);
        assert_eq!(detail["outcome"], "refused");
    }
    fixture.report("accept");
    fixture
        .runtime
        .run_deterministic(
            "review_gate_settle",
            &json!({}),
            &fixture.input,
            Default::default(),
        )
        .unwrap();
    let certificate: ReviewCertificate = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    for (landed_commit, reason) in [
        (None, "mapping_unknown"),
        (
            Some(certificate.final_candidate.commit.clone()),
            "external_landing_race",
        ),
    ] {
        fixture
            .runtime
            .record_review_landing(&ReviewLandingRequest {
                run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
                task_ids: vec![fixture.task_id.clone()],
                workspace_path: fixture.repo.clone(),
                pr_number: "1".into(),
                base: "main".into(),
                reviewed_head_sha: certificate.final_candidate.commit.clone(),
                managed_merge: false,
                landed_commit,
            })
            .unwrap();
        let (row, detail) = fixture.latest("landing");
        assert_eq!(row.status, AuditEventStatus::Success);
        assert!(row.error_message.is_none());
        assert_eq!(detail["covered"], false);
        assert_eq!(detail["reason"], reason);
    }
    assert_eq!(
        fixture
            .runtime
            .review_store()
            .unwrap()
            .review_landings(&certificate.attempt_id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn execution_failures_have_messages_and_remain_unexpected() {
    if !super::dispatch_admission::isolated(
        "review_gate_audit::execution_failures_have_messages_and_remain_unexpected",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    let mut input = fixture.input.clone();
    input["base_sha"] = json!("missing-git-object");
    assert!(
        fixture
            .runtime
            .run_deterministic("review_gate_admit", &json!({}), &input, Default::default())
            .is_err()
    );
    fixture.admit();
    input = fixture.input.clone();
    input["admission"]["attempt_id"] = json!("missing-attempt");
    assert!(
        fixture
            .runtime
            .run_deterministic("review_gate_settle", &json!({}), &input, Default::default())
            .is_err()
    );
    let failures = fixture
        .rows()
        .into_iter()
        .filter(|row| row.status == AuditEventStatus::Failure)
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2);
    for row in failures {
        assert!(
            row.error_message
                .as_deref()
                .is_some_and(|message| !message.trim().is_empty()),
            "gate failures must carry their execution error: {row:?}"
        );
        assert_eq!(classify(&row), FailureClass::Unexpected);
    }
}
