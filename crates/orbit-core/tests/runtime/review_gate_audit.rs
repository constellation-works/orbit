//! Gate decisions remain queryable without counting them as tool failures.

use std::path::PathBuf;

use chrono::Utc;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{ReviewLandingRequest, ReviewerInvocationRequest, RuntimeHost};
use orbit_store::contracts::{FailureClass, classify};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAdmission,
    ReviewBudget, ReviewCertificate, ReviewCrewPoolMember, ReviewTiming, ReviewerInvocationEvent,
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

#[derive(Default)]
struct FixtureSettings<'a> {
    workflow: &'a str,
    review_setting: Option<&'a str>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        Self::new_with_required_commands(&[])
    }

    pub(super) fn new_with_required_commands(required: &[&str]) -> Self {
        Self::new_with_config(required, "")
    }

    /// The fixture with `review` appended to its `[review]` table.
    pub(super) fn new_with_config(required: &[&str], review: &str) -> Self {
        Self::build(
            required,
            review,
            ReviewTiming::BeforePr,
            10,
            None,
            &[],
            Default::default(),
        )
    }

    /// The fixture whose captured `review.minutes` is `minutes` [ORB-15094].
    pub(super) fn new_with_review_minutes(minutes: u32) -> Self {
        Self::build(
            &[],
            "",
            ReviewTiming::BeforePr,
            minutes,
            None,
            &[],
            Default::default(),
        )
    }

    /// The fixture for a run that captured `review.before_landing` instead
    /// [ORB-14849].
    pub(super) fn before_landing() -> Self {
        Self::before_landing_with_config("")
    }

    /// [`Self::before_landing`] with `review` appended to its `[review]`
    /// table.
    pub(super) fn before_landing_with_config(review: &str) -> Self {
        Self::build(
            &[],
            review,
            ReviewTiming::BeforeLanding,
            10,
            None,
            &[],
            Default::default(),
        )
    }

    /// The fixture whose task the `implementer` crew implemented, for a run
    /// that captured `operation.review_crew = ["implementer", "reviewers"]`
    /// whole [ORB-15195].
    pub(super) fn review_pool() -> Self {
        Self::build(
            &[],
            "",
            ReviewTiming::BeforePr,
            10,
            Some(""),
            &[],
            Default::default(),
        )
    }

    /// The fixture whose task an operator granted `granted` at filing, for a
    /// run that captured `timing` [granted-tools §4].
    pub(super) fn granted(timing: ReviewTiming, granted: &[&str]) -> Self {
        Self::build(&[], "", timing, 10, None, granted, Default::default())
    }

    fn build(
        required: &[&str],
        review: &str,
        timing: ReviewTiming,
        review_minutes: u32,
        selection: Option<&str>,
        granted: &[&str],
        settings: FixtureSettings<'_>,
    ) -> Self {
        crate::scenarios::require_isolated_mutable_fixture("review_gate_audit::Fixture::build");
        let FixtureSettings {
            workflow,
            review_setting,
        } = settings;
        let pool = selection.is_some();
        let selection = selection.unwrap_or_default();
        let switch = match timing {
            ReviewTiming::BeforeLanding => "before_landing",
            _ => "before_pr",
        };
        let workspace_review = review_setting
            .map(str::to_string)
            .unwrap_or_else(|| format!("{switch} = true"));
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let required_commands = if required.is_empty() {
            String::new()
        } else {
            format!("required_validation_commands = {required:?}\n")
        };
        let (implementer, review_crew) = if pool {
            (
                "[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.alternative]\nprovider = \"claude\"\nmodel = \"fixture-claude\"\n[crews.parked]\nprovider = \"claude\"\nmodel = \"fixture-parked\"\n",
                if selection.is_empty() {
                    "[\"implementer\", \"reviewers\"]"
                } else {
                    "[\"implementer:1\", \"reviewers:3\", \"alternative:1\", \"parked:0\"]"
                },
            )
        } else {
            ("", "\"reviewers\"")
        };
        std::fs::write(
            workspace.join("config.toml"),
            format!(
                "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n{implementer}[workflow]\ndefault_crew = \"reviewers\"\n{workflow}\n{selection}\n{required_commands}[operation]\nreview_crew = {review_crew}\n[review]\n{workspace_review}\n{review}"
            ),
        )
        .unwrap();
        let git = |args: &[&str]| {
            let mut command = crate::git_test_support::command();
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
                crew: pool.then(|| "implementer".into()),
                granted_tools: granted.iter().map(|tool| (*tool).to_string()).collect(),
                ..Default::default()
            })
            .unwrap();
        let policy = runtime.operation_policy();
        let admission = ReviewAdmission {
            contract_version: REVIEW_CONTRACT_VERSION,
            policy_version: policy.version,
            timing,
            timing_source: if review_setting.is_some() {
                orbit_types::workflow::REVIEW_RUN_OPTION_SOURCE.into()
            } else {
                match timing {
                    ReviewTiming::BeforeLanding => {
                        policy.review_before_landing.source.label().into()
                    }
                    _ => policy.review_before_pr.source.label().into(),
                }
            },
            crew: (!pool).then(|| policy.review_crew.value[0].clone()),
            crew_pool: if pool {
                policy
                    .review_crew
                    .value
                    .iter()
                    .map(|name| ReviewCrewPoolMember {
                        name: name
                            .split_once(':')
                            .map_or(name.as_str(), |(name, _)| name)
                            .into(),
                        weight: name
                            .split_once(':')
                            .map_or(1, |(_, weight)| weight.parse().unwrap()),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            crew_source: policy.review_crew.source.label().into(),
            // A bounded fixture budget exercises exhaustion without depending
            // on the operational default.
            budget: ReviewBudget {
                minutes: review_minutes,
            },
            required_validation_commands: Some(
                runtime.workflow_required_validation_commands().to_vec(),
            ),
            baseline_commands: runtime.review_baseline_commands().to_vec(),
            selective_before_landing: None,
            captured_at: Utc::now(),
            host_evidence: Vec::new(),
        };
        let run = runtime
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({"review": admission, "task_ids": [task.id], "task_id": task.id})),
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

    pub(super) fn report(&self, verdict: &str) {
        self.put_report(&json!({
            "schema_version": REVIEW_CONTRACT_VERSION,
            "attempt_id": self.input["admission"]["attempt_id"],
            "verdict": verdict, "summary": "Checked candidate.",
            "findings": [],
            "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
            "escalation": if verdict == "accept" { None } else { Some("Reviewer cannot accept candidate.") },
        }));
    }

    /// Attach `report` the way the reviewer does: through the public
    /// `orbit.task.artifact.put` tool from a scratch file.
    pub(super) fn put_report(&self, report: &Value) {
        self.try_put_report(report).unwrap();
    }

    /// [`Self::put_report`], returning the tool's refusal instead of
    /// panicking on it.
    pub(super) fn try_put_report(&self, report: &Value) -> Result<Value, orbit_common::OrbitError> {
        let path = self.repo.join(".orbit/tmp").join(REVIEW_REPORT_ARTIFACT);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, report.to_string()).unwrap();
        self.runtime.run_tool("orbit.task.artifact.put", json!({
            "id": self.task_id, "model": "codex", "path": REVIEW_REPORT_ARTIFACT, "source_path": path,
        }))
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

/// [ORB-15195] A run that captured a review pool admits one member as its
/// reviewer, not the crew that implemented its task, and the reviewer record
/// names the pool as its source.
#[test]
fn a_captured_pool_reviews_with_a_crew_that_did_not_implement_the_task() {
    if !super::dispatch_admission::isolated(
        "review_gate_audit::a_captured_pool_reviews_with_a_crew_that_did_not_implement_the_task",
    ) {
        return;
    }
    let mut fixture = Fixture::review_pool();
    fixture.admit();
    let admission = &fixture.input["admission"];
    assert_eq!(admission["applies"], true, "{admission}");
    assert_eq!(admission["reviewer"]["crew"], "reviewers", "{admission}");
    assert_eq!(
        admission["reviewer"]["crew_source"], "workspace pool",
        "{admission}"
    );
}

/// A reviewer pool is narrowed for independence before weighting; pressure
/// and configured/effective weights survive the public gate admission.
fn review_pool_admission_records_pressure_after_independence_filtering() {
    let mut fixture = Fixture::build(
        &[],
        "",
        ReviewTiming::BeforePr,
        10,
        Some("crew_selection = \"usage_pressure\""),
        &[],
        Default::default(),
    );
    crate::usage_pressure_draws::record_week(
        &fixture.runtime,
        "claude",
        10.0,
        chrono::Duration::hours(6),
    );
    crate::usage_pressure_draws::record_week(
        &fixture.runtime,
        "codex",
        0.0,
        chrono::Duration::days(7),
    );
    fixture.admit();
    crate::usage_pressure_draws::assert_pool(
        &fixture.input["admission"]["reviewer"]["eligible_pool"],
        &[("alternative", 1, 8.0), ("reviewers", 3, 3.0)],
    );
}

fn settled_negative_verdicts_are_successful_calls_with_decision_detail() {
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
        let history = fixture.runtime.get_task_history(&fixture.task_id).unwrap();
        let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
        let typed = orbit_types::task::recorded_review_settlements(&history, &comments);
        let legacy = orbit_types::task::recorded_review_settlements(&[], &comments);
        assert_eq!(typed.len(), 1);
        assert_eq!(typed, legacy, "old comment-only records retain their facts");
        let old_shape = json!({"at": comments[0].at, "by": "system", "event": "updated"});
        let old_history: orbit_types::task::TaskHistoryEntry =
            serde_json::from_value(old_shape).unwrap();
        assert!(
            old_history.decision.is_none(),
            "old history has no metadata"
        );
        assert_eq!(
            orbit_types::task::recorded_review_settlements(&[old_history], &comments),
            legacy
        );
        let refused = fixture
            .runtime
            .update_task_with_identity(
                &fixture.task_id,
                TaskUpdateParams {
                    comment: Some("Forged review".into()),
                    review_settlement: Some(typed[0].1.clone()),
                    ..Default::default()
                },
                None,
                None,
            )
            .expect_err("ordinary callers cannot publish gate metadata");
        assert!(refused.to_string().contains("system verdict comment"));
        assert_eq!(
            fixture.runtime.get_task_history(&fixture.task_id).unwrap(),
            history
        );
        assert_eq!(
            fixture.runtime.get_task_comments(&fixture.task_id).unwrap(),
            comments
        );
        let mut display = comments.clone();
        display[0].message = "Display text may change without changing recorded facts.".into();
        assert_eq!(
            orbit_types::task::recorded_review_settlements(&history, &display),
            typed
        );
        let mut malformed = comments.clone();
        malformed[0].message = format!(
            "before-PR review settled attempt `legacy`: verdict **{verdict}**\n- Reviewer runtime: unknown\n- Implementation: ``"
        );
        let unknown = orbit_types::task::recorded_review_settlements(&[], &malformed);
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].1.runtime_seconds, None);
        assert_eq!(unknown[0].1.reviewed_commit, None);
        assert_eq!(unknown[0].1.reviewer_model, None);
        malformed[0].message =
            "before-PR review settled attempt `legacy`: verdict **unknown**".into();
        assert!(orbit_types::task::recorded_review_settlements(&[], &malformed).is_empty());
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

fn uncovered_landings_succeed_and_exhausted_admissions_are_denied() {
    let mut fixture = Fixture::new();
    fixture.admit();
    // The reviewer runs past the candidate's ten minutes without a verdict:
    // the preflight and a re-admission are both denied.
    for event in [
        ReviewerInvocationEvent::Started,
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
    // Replay each persisted partial-publication shape through the real gate.
    let registry =
        TaskRegistryStore::open(&task_registry_path(&fixture.runtime.global_root())).unwrap();
    let bundle = registry
        .canonical_task_bundle_path(&fixture.runtime.workspace_id().unwrap(), &fixture.task_id)
        .unwrap();
    let events_path = bundle.join(orbit_types::task::TASK_EVENTS_FILE_NAME);
    let comments_path = bundle.join(orbit_types::task::TASK_COMMENTS_FILE_NAME);
    let without_settlement = || {
        std::fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .filter(|line| {
                serde_json::from_str::<orbit_types::task::TaskEventRowV2>(line)
                    .unwrap()
                    .event_type
                    != orbit_types::task::REVIEW_SETTLED_EVENT
            })
            .map(|line| format!("{line}\n"))
            .collect::<String>()
    };
    std::fs::write(&events_path, without_settlement()).unwrap();
    std::fs::write(&comments_path, "").unwrap();
    fixture
        .settle()
        .expect("artifact-only publication acquires comment and decision");
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    let history = fixture.runtime.get_task_history(&fixture.task_id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(
        orbit_types::task::recorded_review_settlements(&history, &comments).len(),
        1
    );

    // Conversely, an interrupted artifact write is repaired without reposting.
    let manifest_path = bundle
        .join("artifacts")
        .join(orbit_types::task::TASK_ARTIFACT_MANIFEST_FILE_NAME);
    let mut manifest: orbit_types::task::ArtifactManifestV2 =
        serde_yaml::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest
        .files
        .retain(|file| file.path != REVIEW_GATE_ARTIFACT);
    std::fs::write(&manifest_path, serde_yaml::to_string(&manifest).unwrap()).unwrap();
    fixture
        .settle()
        .expect("comment-only publication reacquires the certificate artifact");
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fixture.runtime.get_task_comments(&fixture.task_id).unwrap(),
        comments
    );
    assert_eq!(
        fixture.runtime.get_task_history(&fixture.task_id).unwrap(),
        history
    );

    let query = orbit_core::application::agents::AgentQuery::parse(Some("all"), Some(1)).unwrap();
    let typed_scorecard = fixture
        .runtime
        .agent_role_scorecard(
            orbit_core::application::agents::AgentRole::Reviewers,
            &query,
        )
        .unwrap();
    let typed_hands = fixture
        .runtime
        .task_handled_by(&fixture.runtime.get_task_row(&fixture.task_id).unwrap());
    assert_eq!(
        typed_hands
            .iter()
            .filter(|hand| hand["verb"] == "reviewed")
            .count(),
        1
    );

    // A pre-metadata publication remains comment-only, with no eager rewrite.
    std::fs::write(&events_path, without_settlement()).unwrap();
    let legacy_history = fixture.runtime.get_task_history(&fixture.task_id).unwrap();
    fixture
        .settle()
        .expect("an older peer's comment-only settlement replays");
    assert_eq!(
        fixture.runtime.get_task_history(&fixture.task_id).unwrap(),
        legacy_history
    );
    assert_eq!(
        fixture.runtime.get_task_comments(&fixture.task_id).unwrap(),
        comments
    );
    let legacy_scorecard = fixture
        .runtime
        .agent_role_scorecard(
            orbit_core::application::agents::AgentRole::Reviewers,
            &query,
        )
        .unwrap();
    assert_eq!(
        typed_scorecard["rows"], legacy_scorecard["rows"],
        "typed and legacy facts preserve reviewer scoring and runtime"
    );
    assert_eq!(
        fixture
            .runtime
            .task_handled_by(&fixture.runtime.get_task_row(&fixture.task_id).unwrap()),
        typed_hands
    );
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

fn execution_failures_have_messages_and_remain_unexpected() {
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

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    settled_negative_verdicts_are_successful_calls_with_decision_detail_scenarios,
    settled_negative_verdicts_are_successful_calls_with_decision_detail,
    review_pool_admission_records_pressure_after_independence_filtering,
    uncovered_landings_succeed_and_exhausted_admissions_are_denied,
    execution_failures_have_messages_and_remain_unexpected
);
