//! Manual public-entry fixture for task quality (no automated feature tests).
//! Synthetic answers verify plumbing, never classifier accuracy or human labels.
#![allow(
    clippy::unwrap_used,
    reason = "isolated manual fixture assertions fail immediately on missing synthetic state or poisoned fixture locks"
)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use orbit_config::assistance::{AssistancePolicy, AssistanceSettings};
use orbit_core::{
    OrbitRuntime,
    application::{
        assistance::{AssistanceAdapter, AssistanceService},
        task::{TaskAddParams, TaskUpdateParams},
        task_quality::{self, TaskQualityDismissalReason, TaskQualitySnapshot},
    },
};
use orbit_store::contracts::assistance::{AssistanceClaim, AssistanceStoreBackend};
use orbit_types::{
    assistance::{
        self, AssistanceAnswer, AssistanceDisposition, AssistanceMode, AssistanceReason,
        AssistanceRequest, AssistanceResult, AssistanceValue,
    },
    desktop::{DesktopTaskFields, DesktopTaskOperation, DesktopTaskRequest},
    task::{Task, TaskPriority, TaskStatus},
    tool::ToolSessionContext,
};
use serde_json::{Value, json};

type FixtureResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type SendAction = Box<dyn FnOnce() + Send>;

#[derive(Clone, Copy)]
enum Reply {
    Clean,
    Flag,
    Missing,
    Malformed,
    Refused,
    Unavailable,
}

struct SyntheticAdapter {
    calls: AtomicUsize,
    prepare_calls: AtomicUsize,
    reply: Mutex<Reply>,
    on_send: Mutex<Option<SendAction>>,
    requests: Mutex<Vec<AssistanceRequest>>,
}

impl AssistanceAdapter for SyntheticAdapter {
    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        _: &[String],
        _: &AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        Ok(request)
    }
    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        _: &AssistancePolicy,
    ) -> AssistanceResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap()
            .push(claim.work.request.clone());
        if let Some(action) = self.on_send.lock().unwrap().take() {
            action();
        }
        let reply = *self.reply.lock().unwrap();
        match reply {
            Reply::Refused => return AssistanceResult::assistance_gap(AssistanceReason::Refused),
            Reply::Unavailable => {
                return AssistanceResult::assistance_gap(AssistanceReason::Unavailable);
            }
            _ => (),
        }
        let mut result = AssistanceResult::assistance_gap(AssistanceReason::Complete);
        result.disposition = AssistanceDisposition::Uncertain;
        result.answers = claim
            .work
            .request
            .contract
            .questions
            .iter()
            .map(|(name, question)| {
                let flag = matches!(reply, Reply::Flag) && name == "criterion_not_observable/1";
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
        if matches!(reply, Reply::Missing) {
            result.answers.pop_first();
        }
        if matches!(reply, Reply::Malformed) {
            result.answers.values_mut().next().unwrap().probability = Some(2.0);
        }
        result
    }
}

struct TaskQualityFixture {
    runtime: OrbitRuntime,
    global: PathBuf,
    workspace: PathBuf,
    settings: AssistanceSettings,
    service: AssistanceService,
    adapter: Arc<SyntheticAdapter>,
}

impl TaskQualityFixture {
    fn open(root: &Path) -> FixtureResult<Self> {
        let global = root.join("global");
        let workspace = root.join("repo/.orbit");
        std::fs::create_dir_all(&global)?;
        std::fs::create_dir_all(&workspace)?;
        let runtime = OrbitRuntime::from_roots(&global, &workspace)?;
        let template = task_quality::task_quality_question_template();
        let mut settings = AssistanceSettings::default();
        settings.budget.epoch = "synthetic-only/1".into();
        settings.budget.token_ceiling = 10_000_000;
        settings.budget.spend_ceiling_microusd = 10_000_000;
        settings.budget.input_microusd_per_million_tokens = Some(1);
        settings.budget.pricing_version = Some("synthetic-price/1".into());
        settings.limits.max_attempts = 1;
        settings.policies.push(AssistancePolicy {
            workspace_id: runtime.workspace_id()?,
            use_case: task_quality::TASK_QUALITY_USE_CASE.into(),
            mode: AssistanceMode::Shadow,
            model: "gpt-6-luna".into(),
            model_revision: "fixture/1".into(),
            policy_version: "synthetic-only/1".into(),
            contract_digest: assistance::assistance_digest(&serde_json::to_vec(&template)?),
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
            prepare_calls: AtomicUsize::new(0),
            reply: Mutex::new(Reply::Clean),
            on_send: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
        });
        let config_root = global.clone();
        let service = AssistanceService::new(
            orbit_store::compose::assistance_store_from_store(runtime.sqlite_store()?),
            adapter.clone(),
            Arc::new(move || {
                Ok(orbit_config::ConfigStore::open(
                    orbit_config::ConfigScope::Global,
                    config_root.join("config.toml"),
                )?
                .snapshot()?
                .assistance_settings)
            }),
        );
        let fixture = Self {
            runtime,
            global,
            workspace,
            settings,
            service,
            adapter,
        };
        fixture.save_settings()?;
        Ok(fixture)
    }
    fn save_settings(&self) -> FixtureResult {
        std::fs::write(
            self.global.join("config.toml"),
            toml::to_string(&json!({"assistance": self.settings}))?,
        )?;
        Ok(())
    }
    fn add(&self, title: &str) -> FixtureResult<Task> {
        Ok(self.runtime.add_task(TaskAddParams {
            title: title.into(),
            description: "Deliver evidence of the shared observable outcome.".into(),
            acceptance_criteria: vec![
                "Code should be clean.".into(),
                "Record observed command output.".into(),
            ],
            ..Default::default()
        })?)
    }
    fn tick(&self) -> FixtureResult {
        Ok(self.runtime.run_task_quality_with_service(&self.service)?)
    }
    fn current(&self, id: &str) -> FixtureResult<TaskQualitySnapshot> {
        let history = self.runtime.task_quality_history(id, &self.service)?;
        Ok(history
            .snapshots
            .into_iter()
            .find(|s| Some(&s.id) == history.current_id.as_ref())
            .ok_or("no current snapshot")?)
    }
    fn task_projection(&self, id: &str) -> FixtureResult<Value> {
        Ok(
            json!({"task": self.runtime.get_task(id)?, "comments": self.runtime.get_task_comments(id)?, "history": self.runtime.get_task_history(id)?}),
        )
    }
    fn calls(&self) -> usize {
        self.adapter.calls.load(Ordering::SeqCst)
    }
    fn private_history_path(&self, id: &str) -> FixtureResult<PathBuf> {
        let workspace = assistance::assistance_digest(self.runtime.workspace_id()?.as_bytes());
        let subject = assistance::assistance_digest(id.as_bytes());
        Ok(self
            .global
            .join("state/task-quality/v1")
            .join(&workspace[7..])
            .join(format!("{}.json", &subject[7..])))
    }
}

fn fixture_entries_and_feedback(root: &Path, schema: &Value) -> FixtureResult {
    let f = TaskQualityFixture::open(root)?;
    let template = task_quality::task_quality_question_template();
    for (name, question) in &template.questions {
        assert_eq!(
            schema["question_sets"]["task_quality"]["questions"][name]["rubric"],
            question.rubric()
        );
    }
    assert_eq!(
        template.question_set_version,
        schema["question_sets"]["task_quality"]["version"]
    );
    *f.adapter.reply.lock().unwrap() = Reply::Flag;
    let task = f.add("Record investigation evidence")?;
    assert_eq!(f.calls(), 0);
    assert_eq!(
        f.adapter.prepare_calls.load(Ordering::SeqCst),
        0,
        "create never enters an adapter, including credential preparation"
    );
    let pending = f.current(&task.id)?;
    assert!(pending.work_id.is_none() && pending.result.is_none());
    let before = f.task_projection(&task.id)?;
    f.tick()?;
    let current = f.current(&task.id)?;
    assert_eq!(current.id, pending.id);
    assert_eq!(f.calls(), 1);
    assert_eq!(current.annotations.len(), 1);
    assert_eq!(current.annotations[0].criterion_refs[0].index, 1);
    assert_eq!(
        current.annotations[0].criterion_refs[0].text_digest,
        assistance::assistance_digest(task.acceptance_criteria[0].as_bytes())
    );
    assert_eq!(
        current.result.as_ref().unwrap().disposition,
        AssistanceDisposition::Uncertain
    );
    assert!(
        f.service
            .assistance_advisory(current.work_id.as_ref().unwrap(), &current.source_digest)?
            .is_none()
    );
    let request = f.adapter.requests.lock().unwrap()[0].clone();
    for (name, question) in &request.contract.questions {
        if name.starts_with("criterion_not_observable") {
            assert_eq!(
                question.criterion_refs().len(),
                1,
                "criterion flags must always identify their criterion"
            );
        } else if name.starts_with("requirements_conflict") {
            assert_eq!(
                question.criterion_refs().len(),
                2,
                "conflict flags must always identify their criterion pair"
            );
        }
    }
    assert!(
        request
            .contract
            .questions
            .contains_key("requirements_conflict/1/2")
    );
    assert!(
        request
            .contract
            .questions
            .contains_key("criterion_not_observable/2")
    );
    assert_eq!(
        request.contract.questions["requirements_conflict/1/2"]
            .criterion_refs()
            .len(),
        2
    );
    // A producer cannot change a granted rubric via generic template expansion.
    let mut forged = request.clone();
    if let assistance::AssistanceQuestion::Predicate { rubric, .. } =
        forged.contract.questions.values_mut().next().unwrap()
    {
        *rubric = "different policy".into();
    }
    assert!(
        f.service
            .enqueue_expanded_assistance(forged, &[], &template)
            .is_err()
    );
    f.runtime.dismiss_task_quality_annotation(
        &task.id,
        &current.id,
        &current.annotations[0].id,
        TaskQualityDismissalReason::ValidInvestigation,
        &f.service,
    )?;
    f.tick()?;
    assert_eq!(f.calls(), 1);
    // Simulate death after the service committed work but before its receipt
    // was written to the consumer ledger. The packet timestamp remains fixed.
    let private = f.private_history_path(&task.id)?;
    let mut lost_receipt: Value = serde_json::from_slice(&std::fs::read(&private)?)?;
    lost_receipt["snapshots"][0]["work_id"] = Value::Null;
    lost_receipt["snapshots"][0]["result"] = Value::Null;
    orbit_common::fs::io::atomic_write_text(&private, &serde_json::to_string(&lost_receipt)?)?;
    f.tick()?;
    assert_eq!(
        f.calls(),
        1,
        "lost enqueue receipt reuses the committed work and result"
    );
    assert_eq!(f.current(&task.id)?.work_id, current.work_id);
    assert_eq!(
        f.task_projection(&task.id)?,
        before,
        "quality writes confer no task/readiness/pilot or workflow authority"
    );
    let restarted = OrbitRuntime::from_roots(&f.global, &f.workspace)?;
    let after = restarted.task_quality_history(&task.id, &f.service)?;
    assert_eq!(
        after.snapshots[0].annotations[0]
            .dismissal
            .as_ref()
            .unwrap()
            .reason,
        TaskQualityDismissalReason::ValidInvestigation
    );

    // Dashboard and guarded MCP creates use a durable retry identity.
    let session = ToolSessionContext::trusted_local(Some(f.runtime.workspace_id()?), None, None);
    let create = DesktopTaskRequest {
        request_id: "fixture-create-replay".into(),
        operation: DesktopTaskOperation::Create {
            title: "Coherent cross component outcome".into(),
            description: "Coordinate the API and UI for the same outcome.".into(),
            acceptance_criteria: vec!["Observe the API and UI agree.".into()],
            priority: TaskPriority::Medium,
            crew: None,
        },
    };
    let first = f
        .runtime
        .desktop_task_write(create.clone(), None, None, &session)?;
    let replay = f.runtime.desktop_task_write(create, None, None, &session)?;
    assert!(replay.replayed);
    assert_eq!(first.snapshot.task.id, replay.snapshot.task.id);
    let id = first.snapshot.task.id;
    let snapshot = f.current(&id)?;
    *f.adapter.reply.lock().unwrap() = Reply::Clean;
    f.tick()?;
    assert_eq!(f.calls(), 2);
    f.tick()?;
    assert_eq!(
        f.calls(),
        2,
        "create replay produces one logical assessment"
    );
    assert_eq!(f.current(&id)?.id, snapshot.id);
    assert!(
        f.current(&id)?.annotations.is_empty(),
        "coherent multi-component scope is allowed"
    );
    let revision = f.runtime.desktop_task_snapshot(&id, &session)?.revision;
    f.runtime.desktop_task_write(
        DesktopTaskRequest {
            request_id: "fixture-desktop-edit".into(),
            operation: DesktopTaskOperation::Edit {
                id: id.clone(),
                expected_revision: revision,
                fields: DesktopTaskFields {
                    title: Some("Revised coherent shared outcome".into()),
                    ..Default::default()
                },
            },
        },
        None,
        None,
        &session,
    )?;
    assert_ne!(
        f.current(&id)?.id,
        snapshot.id,
        "desktop edits schedule from their separate atomic write path"
    );
    Ok(())
}

fn fixture_freshness(root: &Path) -> FixtureResult {
    let f = TaskQualityFixture::open(root)?;
    let task = f.add("Valid investigation delivers evidence")?;
    f.tick()?;
    assert!(f.current(&task.id)?.annotations.is_empty());
    let initial = f.current(&task.id)?;
    f.runtime.update_task_with_identity(
        &task.id,
        TaskUpdateParams {
            comment: Some("More context".into()),
            priority: Some(TaskPriority::High),
            plan: Some("Inspect and report evidence".into()),
            ..Default::default()
        },
        None,
        None,
    )?;
    f.tick()?;
    assert_eq!(f.calls(), 1);
    assert_eq!(f.current(&task.id)?.id, initial.id);
    for title in ["First semantic edit", "Coalesced latest edit"] {
        f.runtime.update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                title: Some(title.into()),
                ..Default::default()
            },
            None,
            None,
        )?;
    }
    f.tick()?;
    assert_eq!(
        f.calls(),
        2,
        "semantic edits coalesce before the next attempt"
    );
    let latest = f.current(&task.id)?;
    assert_ne!(latest.source_digest, initial.source_digest);
    let runtime = f.runtime.clone();
    let id = task.id.clone();
    *f.adapter.on_send.lock().unwrap() = Some(Box::new(move || {
        runtime
            .update_task_with_identity(
                &id,
                TaskUpdateParams {
                    description: Some("Changed during inference".into()),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }));
    f.runtime.update_task_with_identity(
        &task.id,
        TaskUpdateParams {
            description: Some("Before inference".into()),
            ..Default::default()
        },
        None,
        None,
    )?;
    f.tick()?;
    let history = f.runtime.task_quality_history(&task.id, &f.service)?;
    assert!(history.snapshots.iter().any(|s| {
        s.result
            .as_ref()
            .is_some_and(|r| r.disposition == AssistanceDisposition::Stale)
    }));
    assert!(
        f.current(&task.id)?.result.is_none(),
        "late reply cannot annotate the newer content"
    );
    f.tick()?;
    let before_reorder = f.current(&task.id)?;
    let mut criteria = task.acceptance_criteria.clone();
    criteria.reverse();
    f.runtime.update_task_with_identity(
        &task.id,
        TaskUpdateParams {
            acceptance_criteria: Some(criteria),
            ..Default::default()
        },
        None,
        None,
    )?;
    assert_ne!(f.current(&task.id)?.id, before_reorder.id);
    assert!(
        f.runtime
            .dismiss_task_quality_annotation(
                &task.id,
                &initial.id,
                "invented",
                TaskQualityDismissalReason::FalsePositive,
                &f.service
            )
            .is_err()
    );
    f.tick()?;
    Ok(())
}

fn fixture_recovery_off_terminal_and_gaps(root: &Path) -> FixtureResult {
    let mut f = TaskQualityFixture::open(root)?;
    let delayed = f.add("Owner restarts long after creation")?;
    let private = f.private_history_path(&delayed.id)?;
    let mut marker: Value = serde_json::from_slice(&std::fs::read(&private)?)?;
    marker["snapshots"][0]["collected_at"] = json!("2000-01-01T00:00:00Z");
    orbit_common::fs::io::atomic_write_text(&private, &serde_json::to_string(&marker)?)?;
    f.tick()?;
    assert_eq!(
        f.calls(),
        1,
        "first collection happens at admission after restart"
    );
    f.settings.policies[0].mode = AssistanceMode::Off;
    f.save_settings()?;
    let task = f.add("Created while off")?;
    f.tick()?;
    assert_eq!(f.calls(), 1);
    assert!(
        f.runtime
            .task_quality_history(&task.id, &f.service)?
            .snapshots
            .is_empty()
    );
    f.settings.policies[0].mode = AssistanceMode::Shadow;
    f.save_settings()?;
    // No creation event or marker exists. Restart plus reconciliation recovers
    // exactly the same committed-task gap a crash before marker publication leaves.
    let restarted = OrbitRuntime::from_roots(&f.global, &f.workspace)?;
    restarted.run_task_quality_with_service(&f.service)?;
    assert_eq!(f.calls(), 2);
    assert_eq!(
        f.current(&task.id)?.result.as_ref().unwrap().reason,
        AssistanceReason::Complete
    );

    // A failed marker write must not undo committed task creation.
    let directory = f.global.join("state/task-quality");
    let saved = f.global.join("state/task-quality-preserved");
    std::fs::rename(&directory, &saved)?;
    std::fs::write(&directory, "synthetic write outage")?;
    let outage = f.add("Created during local ledger outage")?;
    std::fs::rename(&directory, f.global.join("state/synthetic-outage-evidence"))?;
    std::fs::rename(&saved, &directory)?;
    f.tick()?;
    assert_eq!(f.calls(), 3);
    assert!(f.current(&outage.id)?.result.is_some());

    for (i, reply, expected) in [
        (0, Reply::Missing, AssistanceReason::MissingAnswers),
        (1, Reply::Malformed, AssistanceReason::Malformed),
        (2, Reply::Refused, AssistanceReason::Refused),
        (3, Reply::Unavailable, AssistanceReason::Unavailable),
    ] {
        *f.adapter.reply.lock().unwrap() = reply;
        let task = f.add(&format!("Synthetic gap case {i}"))?;
        f.tick()?;
        let snapshot = f.current(&task.id)?;
        assert_eq!(snapshot.result.as_ref().unwrap().reason, expected);
        assert!(snapshot.annotations.is_empty());
    }
    *f.adapter.reply.lock().unwrap() = Reply::Clean;
    let terminal = f.add("Terminal before worker")?;
    let runtime = f.runtime.clone();
    let id = terminal.id.clone();
    *f.adapter.on_send.lock().unwrap() = Some(Box::new(move || {
        runtime
            .update_task_with_identity(
                &id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Archived),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }));
    f.tick()?;
    let calls = f.calls();
    f.tick()?;
    assert_eq!(f.calls(), calls);
    let history = f.runtime.task_quality_history(&terminal.id, &f.service)?;
    assert!(
        !history.snapshots.is_empty(),
        "terminal history is retained"
    );
    assert!(history.current_id.is_none(), "late terminal reply is stale");
    f.settings.policies[0].mode = AssistanceMode::Off;
    f.save_settings()?;
    let terminal = f.add("Already terminal")?;
    f.runtime.update_task_with_identity(
        &terminal.id,
        TaskUpdateParams {
            status: Some(TaskStatus::Archived),
            ..Default::default()
        },
        None,
        None,
    )?;
    f.settings.policies[0].mode = AssistanceMode::Shadow;
    f.save_settings()?;
    f.tick()?;
    assert!(
        f.runtime
            .task_quality_history(&terminal.id, &f.service)?
            .snapshots
            .is_empty()
    );
    assert_eq!(f.calls(), calls);

    // Evidence boundaries have a visible gap rather than a partial clean bill.
    let oversized = f.runtime.add_task(TaskAddParams {
        title: "Bounded evidence".into(),
        acceptance_criteria: (0..28).map(|i| format!("Observe case {i}")).collect(),
        ..Default::default()
    })?;
    f.tick()?;
    assert_eq!(
        f.current(&oversized.id)?.coverage_gap,
        Some(AssistanceReason::InvalidRequest)
    );
    assert_eq!(f.calls(), calls);
    let accounting = f
        .runtime
        .sqlite_store()?
        .assistance_accounting("synthetic-only/1")?;
    assert_eq!(accounting.attempts as usize, calls);
    Ok(())
}

fn main() -> FixtureResult {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "--fixture-child") {
        let schema: Value =
            serde_json::from_slice(&std::fs::read(args.get(2).ok_or("schema path required")?)?)?;
        let root = Path::new(args.get(3).ok_or("isolated root required")?);
        fixture_entries_and_feedback(&root.join("entries"), &schema)?;
        fixture_freshness(&root.join("freshness"))?;
        fixture_recovery_off_terminal_and_gaps(&root.join("recovery"))?;
        std::io::stdout().write_all(b"{\"fixture\":\"task-quality/1\",\"status\":\"passed\",\"network_calls\":0,\"labels\":\"synthetic\"}\n")?;
        return Ok(());
    }
    let schema = std::fs::canonicalize(args.get(1).ok_or("pass the evaluator schema JSON path")?)?;
    let root = tempfile::Builder::new()
        .prefix("task-quality-fixture-")
        .tempdir_in(schema.parent().ok_or("schema parent required")?)?;
    let output = Command::new(std::env::current_exe()?)
        .arg("--fixture-child")
        .arg(schema)
        .arg(root.path())
        .env_clear()
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path())
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        return Err("isolated task-quality fixture failed".into());
    }
    Ok(())
}
