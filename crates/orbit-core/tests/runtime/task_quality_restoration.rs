//! Restored semantic content must regain current task-quality coverage [ORB-19825].
//!
//! A -> B -> A through the public task update and task-quality application
//! APIs: after B supersedes A, restoring A must yield a current assessment or
//! a fresh bounded pending attempt, and repeated ticks must not leave the
//! subject permanently without current coverage. Superseded history and
//! dismissal identities stay attached to their original snapshots.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use orbit_config::assistance::{AssistancePolicy, AssistanceSettings};
use orbit_core::OrbitRuntime;
use orbit_core::application::assistance::{AssistanceAdapter, AssistanceService};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::application::task_quality::{self, TaskQualityDismissalReason};
use orbit_store::contracts::assistance::AssistanceClaim;
use orbit_types::assistance::{
    self, AssistanceAnswer, AssistanceDisposition, AssistanceMode, AssistanceReason,
    AssistanceRequest, AssistanceResult, AssistanceValue,
};

struct SyntheticAdapter {
    calls: AtomicUsize,
}

impl AssistanceAdapter for SyntheticAdapter {
    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        _: &[String],
        _: &AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason> {
        Ok(request)
    }

    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        _: &AssistancePolicy,
    ) -> AssistanceResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut result = AssistanceResult::assistance_gap(AssistanceReason::Complete);
        result.disposition = AssistanceDisposition::Uncertain;
        result.answers = claim
            .work
            .request
            .contract
            .questions
            .iter()
            .map(|(name, question)| {
                // The validator requires the probability to agree with the
                // predicate value: true needs p >= 0.5, false needs p < 0.5.
                let flag = name == "criterion_not_observable/1";
                (
                    name.clone(),
                    AssistanceAnswer {
                        value: AssistanceValue::Predicate(flag),
                        probability: Some(if flag { 0.99 } else { 0.01 }),
                        probabilities: Vec::new(),
                        criterion_refs: question.criterion_refs().to_vec(),
                    },
                )
            })
            .collect();
        result
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    service: AssistanceService,
    adapter: Arc<SyntheticAdapter>,
}

impl Fixture {
    fn open() -> Self {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let template = task_quality::task_quality_question_template();
        let mut settings = AssistanceSettings::default();
        settings.budget.epoch = "synthetic-only/1".into();
        settings.budget.token_ceiling = 10_000_000;
        settings.budget.spend_ceiling_microusd = 10_000_000;
        settings.budget.input_microusd_per_million_tokens = Some(1);
        settings.budget.pricing_version = Some("synthetic-price/1".into());
        settings.limits.max_attempts = 1;
        settings.policies.push(AssistancePolicy {
            workspace_id: runtime.workspace_id().unwrap(),
            use_case: task_quality::TASK_QUALITY_USE_CASE.into(),
            mode: AssistanceMode::Shadow,
            model: "gpt-6-luna".into(),
            model_revision: "fixture/1".into(),
            policy_version: "synthetic-only/1".into(),
            contract_digest: assistance::assistance_digest(&serde_json::to_vec(&template).unwrap()),
            evidence_fields: vec![
                "title".into(),
                "description".into(),
                "criteria".into(),
                "criterion_refs".into(),
            ],
            evidence_ref_prefixes: vec!["task:".into()],
            credential_env: "ORBIT_ASSISTANCE_SYNTHETIC".into(),
        });
        let adapter = Arc::new(SyntheticAdapter {
            calls: AtomicUsize::new(0),
        });
        let service = AssistanceService::new(
            orbit_store::compose::assistance_store_from_store(runtime.sqlite_store().unwrap()),
            adapter.clone(),
            Arc::new(move || Ok(settings.clone())),
        );
        Self {
            _root: root,
            runtime,
            service,
            adapter,
        }
    }

    fn calls(&self) -> usize {
        self.adapter.calls.load(Ordering::SeqCst)
    }

    fn tick(&self) {
        self.runtime
            .run_task_quality_with_service(&self.service)
            .unwrap();
    }

    fn history(&self, id: &str) -> task_quality::TaskQualityHistory {
        self.runtime
            .task_quality_history(id, &self.service)
            .unwrap()
    }
}

const DESCRIPTION: &str = "Deliver evidence of the shared observable outcome.";
fn criteria() -> Vec<String> {
    vec![
        "Code should be clean.".into(),
        "Record observed command output.".into(),
    ]
}

#[test]
fn restored_semantic_content_regains_current_coverage() {
    let f = Fixture::open();

    // Content A, assessed.
    let task = f
        .runtime
        .add_task(TaskAddParams {
            title: "Content A".into(),
            description: DESCRIPTION.into(),
            acceptance_criteria: criteria(),
            ..Default::default()
        })
        .unwrap();
    f.tick();
    assert_eq!(f.calls(), 1, "content A is assessed on its tick");
    let history = f.history(&task.id);
    let current_a = history
        .current_id
        .clone()
        .expect("content A has a current snapshot after its tick");
    let assessed_a = history
        .snapshots
        .into_iter()
        .find(|s| s.id == current_a)
        .expect("content A has a current snapshot after its tick");
    assert!(
        assessed_a.result.is_some(),
        "content A has a completed assessment"
    );
    let snapshot_a = assessed_a.id.clone();
    let work_a = assessed_a.work_id.clone().expect("content A was admitted");
    assert_eq!(assessed_a.annotations.len(), 1);
    let annotation_a = assessed_a.annotations[0].id.clone();
    f.runtime
        .dismiss_task_quality_annotation(
            &task.id,
            &snapshot_a,
            &annotation_a,
            TaskQualityDismissalReason::ValidInvestigation,
            &f.service,
        )
        .unwrap();

    // Content B, assessed; A becomes superseded history.
    f.runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                title: Some("Content B".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    f.tick();
    assert_eq!(f.calls(), 2, "content B is assessed on its tick");
    let history = f.history(&task.id);
    let snapshot_b = history.current_id.clone().expect("content B is current");
    assert_ne!(snapshot_b, snapshot_a);
    let assessed_b = history
        .snapshots
        .iter()
        .find(|s| s.id == snapshot_b)
        .unwrap();
    assert!(assessed_b.result.is_some());
    assert!(
        history.snapshots.iter().any(|s| s.id == snapshot_a),
        "superseded content A history is retained"
    );

    // Restore content A, then tick repeatedly.
    f.runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                title: Some("Content A".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    f.tick();
    let history = f.history(&task.id);
    let current = history.current_id.clone().expect(
        "restored content A has current coverage after its tick; \
         the pinned defect left current_id permanently null",
    );
    assert_eq!(
        current, snapshot_a,
        "restoration reuses its snapshot identity"
    );
    let restored = history
        .snapshots
        .iter()
        .find(|s| s.id == snapshot_a)
        .unwrap();
    assert_ne!(
        restored.work_id.as_ref(),
        Some(&work_a),
        "restoration mints a fresh attempt instead of reusing the stale work"
    );
    assert!(
        restored.result.is_some() || restored.work_id.is_some(),
        "restored content A has a current assessment or a fresh pending attempt"
    );
    assert!(
        restored.coverage_gap != Some(AssistanceReason::InvalidRequest),
        "restoration is not reported as a bounded-contract gap"
    );
    assert!(
        f.calls() >= 3,
        "restoration produced a fresh bounded attempt"
    );

    f.tick();
    let history = f.history(&task.id);
    assert!(
        history.current_id.is_some(),
        "repeated ticks keep current coverage for restored content A"
    );
    let settled = history
        .snapshots
        .iter()
        .find(|s| Some(&s.id) == history.current_id.as_ref())
        .unwrap();
    assert!(
        settled.result.is_some(),
        "repeated ticks settle the restored assessment"
    );
    assert_eq!(
        settled
            .annotations
            .iter()
            .find(|a| a.id == annotation_a)
            .and_then(|a| a.dismissal.as_ref())
            .map(|d| d.reason),
        Some(TaskQualityDismissalReason::ValidInvestigation),
        "restoration preserves the dismissal identity"
    );
    let superseded_b = history
        .snapshots
        .iter()
        .find(|s| s.id == snapshot_b)
        .expect("content B history is retained after restoration");
    assert!(
        superseded_b.result.is_some(),
        "superseded content B keeps its assessment history"
    );

    // Restoring content confers no task or workflow authority.
    let task = f.runtime.get_task(&task.id).unwrap();
    assert_eq!(task.title, "Content A");
    assert_eq!(task.description, DESCRIPTION);
    assert_eq!(task.acceptance_criteria, criteria());
}
