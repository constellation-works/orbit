//! The task-pilot routine honours a host provider failure hold. A
//! `provider_unavailable` or `provider_capacity` failure on this host withholds
//! the dispatch while it covers every crew the pilot would draw, and the draw
//! skips a held crew while another remains. Crew and pool names are fixture
//! policy; the guarded behaviour is that no `task_pilot_pipeline` run is
//! created for a held provider, and that the routine admits work again when
//! the backoff ends or a later success or auth probe clears the hold.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_core::application::automation::evaluate_routine;
use orbit_core::application::contracts::PULL_DRAIN_JOB;
use orbit_core::application::job::JobRunListParams;
use orbit_core::{JobRun, JobRunState, JobRunStep, JobTargetType};
use orbit_engine::RuntimeHost;
use orbit_types::task::ExecutionLocation;
use orbit_types::workflow::automation::AutomationDiagnostic;
use orbit_types::workflow::{
    ActivityCrewDraw, PipelineState, ProviderFailureClass, PullAuthExclusion, PullAuthRecovery,
    provider_failure_text,
};
use serde_json::json;

use super::{Workspace, pilot_routine, runtime_at};

const LOW: &str = "workflow.low_complexity_crews";

fn workspace_with(config: &str) -> Workspace {
    let mut workspace = Workspace::new();
    std::fs::write(workspace.repo.join(".orbit/config.toml"), config).unwrap();
    let global = workspace.runtime.global_root();
    workspace.runtime = runtime_at(&global, &workspace.repo.join(".orbit"));
    workspace.jobs = orbit_store::compose::workspace_job_run_store(
        workspace.runtime.sqlite_store().unwrap(),
        workspace.runtime.workspace_id().unwrap(),
    );
    workspace
}

fn this_host() -> ExecutionLocation {
    ExecutionLocation {
        machine_id: "fixture-machine".into(),
        machine_name: None,
    }
}

fn other_host() -> ExecutionLocation {
    ExecutionLocation {
        machine_id: "other-id".into(),
        machine_name: Some("other-host".into()),
    }
}

fn save_run(workspace: &Workspace, run: &JobRun, state: Option<&PipelineState>) {
    workspace
        .runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&workspace.runtime.workspace_id().unwrap(), run, state)
        .unwrap();
}

fn save_step(
    workspace: &Workspace,
    run_id: &str,
    at: DateTime<Utc>,
    class: ProviderFailureClass,
    provider: &str,
) {
    let step = JobRunStep {
        lock_wait_ms: None,
        step_index: 0,
        target_type: JobTargetType::Activity,
        target_id: "fixture".into(),
        started_at: Some(at),
        finished_at: Some(at),
        duration_ms: None,
        exit_code: Some(1),
        agent_response_json: None,
        state: JobRunState::Failed,
        error_code: Some(class.as_str().to_string()),
        error_message: Some(provider_failure_text(class, provider, "fixture failure")),
    };
    workspace
        .runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_step_for_workspace(
            &workspace.runtime.workspace_id().unwrap(),
            run_id,
            &step,
        )
        .unwrap();
}

/// A finished run with independent creation and completion timestamps.
fn finished_run(
    run_id: &str,
    created_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    state: JobRunState,
    crew: Option<&str>,
    host: ExecutionLocation,
) -> JobRun {
    JobRun {
        run_id: run_id.into(),
        job_id: "fixture-provider".into(),
        attempt: 1,
        state,
        scheduled_at: finished_at,
        created_at,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        duration_ms: None,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: crew.map(str::to_string),
        crew_model: None,
        steps: Vec::new(),
        executed_on: Some(host),
    }
}

fn save_failure(
    workspace: &Workspace,
    run_id: &str,
    now: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    class: ProviderFailureClass,
    provider: &str,
    crew: &str,
) {
    save_run(
        workspace,
        &finished_run(
            run_id,
            now,
            finished_at,
            JobRunState::Failed,
            Some(crew),
            this_host(),
        ),
        None,
    );
    save_step(workspace, run_id, finished_at, class, provider);
}

fn save_success(
    workspace: &Workspace,
    run_id: &str,
    now: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    crew: &str,
) {
    save_run(
        workspace,
        &finished_run(
            run_id,
            now,
            finished_at,
            JobRunState::Success,
            Some(crew),
            this_host(),
        ),
        None,
    );
}

/// A success that ran from `started_at` to `finished_at`.
fn save_success_spanning(
    workspace: &Workspace,
    run_id: &str,
    now: DateTime<Utc>,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    crew: &str,
) {
    let mut run = finished_run(
        run_id,
        now,
        finished_at,
        JobRunState::Success,
        Some(crew),
        this_host(),
    );
    run.scheduled_at = started_at;
    run.started_at = Some(started_at);
    save_run(workspace, &run, None);
}

/// A finished `task_pilot_pipeline` run whose pilots ran as `drawn`, the
/// crew it froze under the low pool. Its `resolved_crew` is `task_crew`, the
/// batch's task crew, as a real pilot run records it.
fn save_pilot_run(
    workspace: &Workspace,
    run_id: &str,
    now: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    state: JobRunState,
    task_crew: &str,
    drawn: &str,
) {
    let mut run = finished_run(
        run_id,
        now,
        finished_at,
        state,
        Some(task_crew),
        this_host(),
    );
    run.job_id = "task_pilot_pipeline".into();
    let mut pipeline = PipelineState::new(run_id.into(), run.job_id.clone(), json!({}));
    pipeline.activity_crew_draws.insert(
        LOW.into(),
        ActivityCrewDraw {
            crew: drawn.into(),
            source: LOW.into(),
            eligible_pool: Vec::new(),
        },
    );
    save_run(workspace, &run, Some(&pipeline));
}

/// Point this workspace's `codex` executor at a script that answers every
/// call with Codex's own 401 turn failure, as a revoked login does.
#[cfg(unix)]
fn install_logged_out_codex(workspace: &Workspace) {
    use orbit_types::resource::ExecutorResource;
    use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind};
    use std::os::unix::fs::PermissionsExt;

    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let provider = workspace.root.path().join("codex");
    std::fs::write(
        &provider,
        "#!/bin/sh\n/bin/cat > /dev/null\n/bin/cat <<'PAYLOAD'\n\
         {\"type\":\"turn.failed\",\"error\":{\"message\":\"unexpected status 401 Unauthorized\"}}\n\
         PAYLOAD\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    // The fixture observes the run's failure record, not OS confinement.
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    workspace
        .runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
}

fn save_probe(
    workspace: &Workspace,
    now: DateTime<Utc>,
    recovered_at: DateTime<Utc>,
    provider: &str,
) {
    let run_id = format!("jrun-probe-{provider}");
    let mut state = PipelineState::new(run_id.clone(), PULL_DRAIN_JOB.into(), json!({}));
    state.pull_drain.pull_auth_recovery.insert(
        provider.into(),
        PullAuthRecovery {
            exclusion: PullAuthExclusion {
                provider: provider.into(),
                account: None,
                host: "fixture-machine".into(),
                excluded_at: recovered_at - Duration::minutes(5),
                error_class: "provider_unavailable".into(),
                relogin_hint: "fixture login".into(),
                credential_source: "oauth".into(),
                next_probe_at: None,
                attempts: 1,
            },
            recovered_at: Some(recovered_at),
            recovery_source: Some("auth_probe".into()),
        },
    );
    let mut run = finished_run(
        &run_id,
        now,
        recovered_at,
        JobRunState::Success,
        None,
        this_host(),
    );
    run.job_id = PULL_DRAIN_JOB.into();
    save_run(workspace, &run, Some(&state));
}

fn evaluate(workspace: &Workspace, at: DateTime<Utc>, dry_run: bool) -> AutomationDiagnostic {
    evaluate_routine(&workspace.runtime, &pilot_routine(), dry_run, at).unwrap()
}

fn pilot_count(workspace: &Workspace) -> usize {
    workspace
        .runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("task_pilot_pipeline".into()),
            ..Default::default()
        })
        .unwrap()
        .len()
}

fn withhold_reason(diagnostic: &AutomationDiagnostic, task_id: &str) -> String {
    diagnostic
        .state
        .as_ref()
        .and_then(|state| state.members.as_ref())
        .and_then(|members| members.withheld.get(task_id))
        .cloned()
        .unwrap_or_else(|| panic!("no withhold for {task_id}: {diagnostic:?}"))
}

fn assert_withheld(workspace: &Workspace, now: DateTime<Utc>, task_id: &str) -> String {
    workspace.install_pilot_job();
    let pilots_before = pilot_count(workspace);
    let suppressed = evaluate(workspace, now, false);
    assert_eq!(suppressed.reason, "work_withheld", "{suppressed:?}");
    assert!(suppressed.batch.is_empty(), "{suppressed:?}");
    assert_eq!(
        pilot_count(workspace),
        pilots_before,
        "a held provider must not start task_pilot_pipeline"
    );
    let reason = withhold_reason(&suppressed, task_id);
    assert!(
        reason.starts_with("provider_hold:"),
        "withhold names the provider hold: {reason}"
    );
    reason
}

/// The hold has lifted, so the first tick only starts the debounce and the
/// tick after it admits the batch. Dry-run admission creates no run.
fn assert_resumes(workspace: &Workspace, now: DateTime<Utc>, task_id: &str) {
    let pending = evaluate(workspace, now, false);
    assert_eq!(pending.reason, "debouncing", "{pending:?}");
    let members = pending.state.unwrap().members.unwrap();
    assert!(
        members.withheld.is_empty(),
        "a lifted hold leaves nothing withheld: {:?}",
        members.withheld
    );
    assert!(members.pending.contains_key(task_id));
    let due = evaluate(workspace, now + Duration::minutes(3), true);
    assert_eq!(due.reason, "would_fire", "{due:?}");
    assert_eq!(due.batch.len(), 1, "{due:?}");
    assert_eq!(due.batch[0].task_ids, [task_id]);
}

fn low_draw(workspace: &Workspace) -> (String, ActivityCrewDraw) {
    let run = workspace
        .jobs
        .insert_job_run("task_pilot_pipeline", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    workspace
        .jobs
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    let state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
    workspace.jobs.write_run_state(&run.run_id, &state).unwrap();
    let model = workspace
        .runtime
        .agent_crew_config_for_input(&json!({
            "job_run_id": run.run_id,
            "crew_config_key": LOW,
            "inspection_revision": "0".repeat(40),
            "partition_index": 0,
        }))
        .unwrap()
        .expect("the draw resolves a crew")
        .model
        .expect("the crew has a model");
    let draw = workspace
        .jobs
        .read_run_state(&run.run_id)
        .unwrap()
        .unwrap()
        .activity_crew_draws
        .remove(LOW)
        .expect("the low pool draw is frozen on the run");
    (model, draw)
}

fn until_text(finished_at: DateTime<Utc>, minutes: i64) -> String {
    (finished_at + Duration::minutes(minutes)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn unavailable_on_the_only_crew_withholds_and_creates_no_pilot_run() {
    let workspace = Workspace::new();
    let task = workspace.task("held codex login");
    let now = Utc::now();
    let finished = now - Duration::minutes(5);
    save_failure(
        &workspace,
        "jrun-codex-auth",
        now,
        finished,
        ProviderFailureClass::Unavailable,
        "codex",
        "fixture",
    );
    let reason = assert_withheld(&workspace, now, &task.id);
    assert!(
        reason.contains("provider_unavailable codex")
            && reason.contains(&until_text(finished, 30))
            && reason.contains("jrun-codex-auth")
            && reason.contains("fixture"),
        "{reason}"
    );
}

/// Repair can fail long after the pipeline row was created. The hold must
/// survive the creation-time scan window and expire from the failure's finish.
fn an_old_pipeline_repair_failure_holds_until_its_finish_backoff() {
    let now = Utc::now();
    let created_at = now - Duration::minutes(75);
    let finished_at = now - Duration::minutes(5);
    for (class, backoff_minutes) in [
        (ProviderFailureClass::Unavailable, 30),
        (ProviderFailureClass::Capacity, 15),
    ] {
        let workspace = Workspace::new();
        let task = workspace.task("wait for the repair pilot provider to recover");
        let run_id = "jrun-old-repair";
        save_pilot_run(
            &workspace,
            run_id,
            created_at,
            finished_at,
            JobRunState::Failed,
            "fixture",
            "fixture",
        );
        let step = JobRunStep {
            step_index: 4,
            target_type: JobTargetType::Activity,
            target_id: "repair_pilots".into(),
            state: JobRunState::Failed,
            started_at: Some(created_at + Duration::minutes(40)),
            finished_at: Some(finished_at),
            duration_ms: None,
            exit_code: Some(1),
            error_code: Some(class.as_str().into()),
            error_message: Some(provider_failure_text(class, "codex", "repair failure")),
            agent_response_json: None,
            lock_wait_ms: None,
        };
        workspace
            .runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_step_for_workspace(
                &workspace.runtime.workspace_id().unwrap(),
                run_id,
                &step,
            )
            .unwrap();

        let expires_at = finished_at + Duration::minutes(backoff_minutes);
        for check_at in [now, expires_at - Duration::seconds(1)] {
            let reason = assert_withheld(&workspace, check_at, &task.id);
            assert!(
                reason.contains(class.as_str())
                    && reason.contains(run_id)
                    && reason.contains(&until_text(finished_at, backoff_minutes)),
                "the repair failure holds until its finish-based backoff: {reason}"
            );
        }
        assert_resumes(&workspace, expires_at, &task.id);
    }
}

fn a_later_success_clears_unavailable_and_capacity() {
    let now = Utc::now();
    let failure_at = now - Duration::minutes(10);
    let success_at = now - Duration::minutes(1);

    let unavailable = Workspace::new();
    let task = unavailable.task("cleared by a later codex run");
    save_failure(
        &unavailable,
        "jrun-codex-auth",
        now,
        failure_at,
        ProviderFailureClass::Unavailable,
        "codex",
        "fixture",
    );
    save_success(&unavailable, "jrun-codex-ok", now, success_at, "fixture");
    assert_resumes(&unavailable, now, &task.id);

    let capacity = Workspace::new();
    let task = capacity.task("cleared by a later run of the held crew");
    save_failure(
        &capacity,
        "jrun-codex-capacity",
        now,
        failure_at,
        ProviderFailureClass::Capacity,
        "codex",
        "fixture",
    );
    save_success(&capacity, "jrun-codex-ok", now, success_at, "fixture");
    assert_resumes(&capacity, now, &task.id);
}

/// A success that started before the failure ran on the credentials or
/// capacity that then failed, so finishing after it does not clear the hold.
fn a_success_started_before_the_failure_does_not_clear_it() {
    let now = Utc::now();
    let started_at = now - Duration::minutes(12);
    let failure_at = now - Duration::minutes(10);
    let finished_at = now - Duration::minutes(1);

    let unavailable = Workspace::new();
    let task = unavailable.task("codex run spans the login failure");
    save_failure(
        &unavailable,
        "jrun-codex-auth",
        now,
        failure_at,
        ProviderFailureClass::Unavailable,
        "codex",
        "fixture",
    );
    save_success_spanning(
        &unavailable,
        "jrun-codex-long",
        now,
        started_at,
        finished_at,
        "fixture",
    );
    let reason = assert_withheld(&unavailable, now, &task.id);
    assert!(
        reason.contains("provider_unavailable codex") && reason.contains("jrun-codex-auth"),
        "{reason}"
    );

    let capacity = Workspace::new();
    let task = capacity.task("held crew run spans the capacity failure");
    save_failure(
        &capacity,
        "jrun-codex-capacity",
        now,
        failure_at,
        ProviderFailureClass::Capacity,
        "codex",
        "fixture",
    );
    save_success_spanning(
        &capacity,
        "jrun-codex-long",
        now,
        started_at,
        finished_at,
        "fixture",
    );
    let reason = assert_withheld(&capacity, now, &task.id);
    assert!(
        reason.contains("provider_capacity fixture") && reason.contains("jrun-codex-capacity"),
        "{reason}"
    );
}

fn an_auth_probe_recovery_clears_unavailable() {
    let workspace = Workspace::new();
    let task = workspace.task("cleared by an auth probe");
    let now = Utc::now();
    save_failure(
        &workspace,
        "jrun-codex-auth",
        now,
        now - Duration::minutes(10),
        ProviderFailureClass::Unavailable,
        "codex",
        "fixture",
    );
    save_probe(&workspace, now, now - Duration::minutes(1), "codex");
    assert_resumes(&workspace, now, &task.id);
}

fn unavailable_holds_every_crew_of_the_provider() {
    let workspace = workspace_with(
        r#"
[workflow]
default_crew = "claude_a"
low_complexity_crews = ["claude_a", "claude_b"]
[crews.claude_a]
provider = "claude"
model = "fixture-a"
[crews.claude_b]
provider = "claude"
model = "fixture-b"
"#,
    );
    let task = workspace.task("both claude crews share the login");
    let now = Utc::now();
    save_failure(
        &workspace,
        "jrun-claude-auth",
        now,
        now - Duration::minutes(5),
        ProviderFailureClass::Unavailable,
        "claude",
        "claude_a",
    );
    let reason = assert_withheld(&workspace, now, &task.id);
    assert!(
        reason.contains("provider_unavailable claude") && reason.contains("jrun-claude-auth"),
        "{reason}"
    );
    let (_model, draw) = low_draw(&workspace);
    assert!(
        draw.source.contains("provider hold, none skipped")
            && draw.source.contains("provider_unavailable claude"),
        "{}",
        draw.source
    );
    let mut names = draw
        .eligible_pool
        .iter()
        .map(|member| member.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["claude_a", "claude_b"]);
}

fn another_providers_crew_is_drawn_and_the_routine_still_admits() {
    let workspace = workspace_with(
        r#"
[workflow]
default_crew = "free_claude"
low_complexity_crews = ["held_codex", "free_claude"]
[crews.held_codex]
provider = "codex"
model = "fixture-held"
[crews.free_claude]
provider = "claude"
model = "fixture-free"
"#,
    );
    let task = workspace.task("claude is still available");
    let now = Utc::now();
    save_failure(
        &workspace,
        "jrun-codex-auth",
        now,
        now - Duration::minutes(5),
        ProviderFailureClass::Unavailable,
        "codex",
        "held_codex",
    );
    assert_resumes(&workspace, now, &task.id);
    let (model, draw) = low_draw(&workspace);
    assert_eq!(draw.crew, "free_claude");
    assert_eq!(model, "fixture-free");
    assert_eq!(
        draw.eligible_pool
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>(),
        ["free_claude"]
    );
    assert!(
        draw.source.contains("provider hold")
            && draw.source.contains("provider_unavailable codex")
            && draw.source.contains("held_codex"),
        "{}",
        draw.source
    );
}

fn a_probe_does_not_clear_provider_wide_capacity_but_a_later_success_does() {
    let now = Utc::now();
    let finished = now - Duration::minutes(5);
    let wide = Workspace::new();
    let task = wide.task("capacity with no resolved crew");
    save_run(
        &wide,
        &finished_run(
            "jrun-codex-capacity-wide",
            now,
            finished,
            JobRunState::Failed,
            None,
            this_host(),
        ),
        None,
    );
    save_step(
        &wide,
        "jrun-codex-capacity-wide",
        finished,
        ProviderFailureClass::Capacity,
        "codex",
    );
    save_probe(&wide, now, now - Duration::minutes(1), "codex");
    let reason = assert_withheld(&wide, now, &task.id);
    assert!(
        reason.contains("provider_capacity codex")
            && reason.contains(&until_text(finished, 15))
            && reason.contains("jrun-codex-capacity-wide"),
        "{reason}"
    );

    let cleared = Workspace::new();
    let task = cleared.task("a later codex success clears provider-wide capacity");
    save_run(
        &cleared,
        &finished_run(
            "jrun-codex-capacity-wide",
            now,
            finished,
            JobRunState::Failed,
            None,
            this_host(),
        ),
        None,
    );
    save_step(
        &cleared,
        "jrun-codex-capacity-wide",
        finished,
        ProviderFailureClass::Capacity,
        "codex",
    );
    save_success(
        &cleared,
        "jrun-codex-ok",
        now,
        now - Duration::minutes(1),
        "fixture",
    );
    assert_resumes(&cleared, now, &task.id);
}

fn capacity_on_the_only_crew_withholds_and_a_probe_does_not_clear_it() {
    let workspace = Workspace::new();
    let task = workspace.task("codex is at capacity");
    let now = Utc::now();
    let finished = now - Duration::minutes(5);
    save_failure(
        &workspace,
        "jrun-codex-capacity",
        now,
        finished,
        ProviderFailureClass::Capacity,
        "codex",
        "fixture",
    );
    save_probe(&workspace, now, now - Duration::minutes(1), "codex");
    let reason = assert_withheld(&workspace, now, &task.id);
    assert!(
        reason.contains("provider_capacity fixture")
            && reason.contains(&until_text(finished, 15))
            && reason.contains("jrun-codex-capacity"),
        "{reason}"
    );
}

fn capacity_on_one_crew_leaves_another_crew_of_that_provider() {
    let workspace = workspace_with(
        r#"
[workflow]
default_crew = "claude_a"
low_complexity_crews = ["claude_a", "claude_b"]
[crews.claude_a]
provider = "claude"
model = "fixture-a"
[crews.claude_b]
provider = "claude"
model = "fixture-b"
"#,
    );
    let task = workspace.task("the other claude model still has room");
    let now = Utc::now();
    save_failure(
        &workspace,
        "jrun-claude-a-capacity",
        now,
        now - Duration::minutes(5),
        ProviderFailureClass::Capacity,
        "claude",
        "claude_a",
    );
    assert_resumes(&workspace, now, &task.id);
    let (model, draw) = low_draw(&workspace);
    assert_eq!(draw.crew, "claude_b");
    assert_eq!(model, "fixture-b");
    assert_eq!(
        draw.eligible_pool
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>(),
        ["claude_b"]
    );
    assert!(
        draw.source.contains("provider hold") && draw.source.contains("provider_capacity claude_a"),
        "{}",
        draw.source
    );
}

fn another_hosts_failure_does_not_withhold() {
    let workspace = Workspace::new();
    let task = workspace.task("the other host lost its login");
    let now = Utc::now();
    let finished = now - Duration::minutes(5);
    save_run(
        &workspace,
        &finished_run(
            "jrun-other-host",
            now,
            finished,
            JobRunState::Failed,
            Some("fixture"),
            other_host(),
        ),
        None,
    );
    save_step(
        &workspace,
        "jrun-other-host",
        finished,
        ProviderFailureClass::Unavailable,
        "codex",
    );
    assert_resumes(&workspace, now, &task.id);
}

fn limit_and_refusal_failures_do_not_withhold_the_routine() {
    let now = Utc::now();
    for class in [ProviderFailureClass::Limit, ProviderFailureClass::Refusal] {
        let workspace = Workspace::new();
        let task = workspace.task(class.as_str());
        save_failure(
            &workspace,
            &format!("jrun-{}", class.as_str()),
            now,
            now - Duration::minutes(5),
            class,
            "codex",
            "fixture",
        );
        assert_resumes(&workspace, now, &task.id);
    }
}

/// A pilot that fails on the provider's login is the failure that holds the
/// next tick. The shipped job runs for real, with only the provider scripted,
/// so the projection reads the record a real pilot failure leaves.
fn a_pilot_that_fails_on_the_provider_login_withholds_the_next_tick() {
    // The scripted provider is a Unix shell script.
    #[cfg(unix)]
    pilot_login_failure_withholds_the_next_tick();
}

#[cfg(unix)]
fn pilot_login_failure_withholds_the_next_tick() {
    let workspace = Workspace::new();
    install_logged_out_codex(&workspace);
    let task = workspace.task("pilot meets a revoked codex login");
    let job = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/jobs/task_pilot_pipeline.yaml");
    let outcome = workspace
        .runtime
        .run_job_v2_from_yaml(
            &job,
            json!({"task_ids": [task.id], "workspace_path": workspace.repo, "base_branch": "main"}),
        )
        .unwrap();
    assert!(
        !outcome.success,
        "the pilot's provider refused it: {outcome:?}"
    );
    let reason = assert_withheld(&workspace, Utc::now(), &task.id);
    assert!(
        reason.contains("provider_unavailable codex") && reason.contains(&outcome.run_id),
        "the withhold names the pilot run's own failure: {reason}"
    );
}

/// A pilot run's `resolved_crew` is its batch's task crew, but its pilots ran
/// as the crew it drew. A pilot success clears only that crew's provider, and
/// a pilot capacity failure holds that crew.
fn a_pilot_run_counts_as_the_crew_it_drew() {
    let config = r#"
[workflow]
default_crew = "codex_a"
low_complexity_crews = ["codex_a", "codex_b"]
[crews.codex_a]
provider = "codex"
model = "fixture-a"
[crews.codex_b]
provider = "codex"
model = "fixture-b"
[crews.claude_c]
provider = "claude"
model = "fixture-c"
"#;
    let now = Utc::now();

    let unavailable = workspace_with(config);
    let task = unavailable.task("a claude pilot does not clear codex");
    save_failure(
        &unavailable,
        "jrun-codex-auth",
        now,
        now - Duration::minutes(10),
        ProviderFailureClass::Unavailable,
        "codex",
        "codex_a",
    );
    save_pilot_run(
        &unavailable,
        "jrun-pilot-claude",
        now,
        now - Duration::minutes(2),
        JobRunState::Success,
        "codex_a",
        "claude_c",
    );
    let reason = assert_withheld(&unavailable, now, &task.id);
    assert!(
        reason.contains("provider_unavailable codex") && reason.contains("jrun-codex-auth"),
        "{reason}"
    );

    let capacity = workspace_with(config);
    let task = capacity.task("the pilot's drawn crew is the one out of room");
    save_pilot_run(
        &capacity,
        "jrun-pilot-capacity",
        now,
        now - Duration::minutes(5),
        JobRunState::Failed,
        "codex_a",
        "codex_b",
    );
    save_step(
        &capacity,
        "jrun-pilot-capacity",
        now - Duration::minutes(5),
        ProviderFailureClass::Capacity,
        "codex",
    );
    assert_resumes(&capacity, now, &task.id);
    let (model, draw) = low_draw(&capacity);
    assert_eq!(draw.crew, "codex_a", "{}", draw.source);
    assert_eq!(model, "fixture-a");
    assert!(
        draw.source.contains("provider_capacity codex_b"),
        "the hold lands on the crew the pilot drew: {}",
        draw.source
    );
}

/// Recovery and verification run agents on recorded draws, not the task crew.
fn recovery_and_verification_count_their_recorded_draws() {
    let config = r#"
[workflow]
default_crew = "codex_a"
low_complexity_crews = ["codex_a", "codex_b"]
[crews.codex_a]
provider = "codex"
model = "fixture-a"
[crews.codex_b]
provider = "codex"
model = "fixture-b"
[crews.claude_c]
provider = "claude"
model = "fixture-c"
"#;
    let now = Utc::now();
    for job in [
        "blocked_task_recovery_pipeline",
        "finding_verification_pipeline",
    ] {
        let workspace = workspace_with(config);
        let task = workspace.task("another provider's recovery does not clear this login");
        save_failure(
            &workspace,
            "jrun-auth",
            now,
            now - Duration::minutes(10),
            ProviderFailureClass::Unavailable,
            "codex",
            "codex_a",
        );
        let mut run = finished_run(
            "jrun-drawn-success",
            now,
            now - Duration::minutes(2),
            JobRunState::Success,
            Some("codex_a"),
            this_host(),
        );
        run.job_id = job.into();
        let mut state = PipelineState::new(run.run_id.clone(), job.into(), json!({}));
        if job == "blocked_task_recovery_pipeline" {
            state.activity_crew_draws.insert(
                "workflow.final_recovery_crews".into(),
                ActivityCrewDraw {
                    crew: "claude_c".into(),
                    source: "fixture".into(),
                    eligible_pool: Vec::new(),
                },
            );
        } else {
            state.pipeline = json!({
                "prepare": {"partitions": [{"crew": "claude_c"}]},
                "verdict_results": [{"tasks": []}],
            });
        }
        save_run(&workspace, &run, Some(&state));
        let reason = assert_withheld(&workspace, now, &task.id);
        assert!(
            reason.contains("provider_unavailable codex"),
            "{job}: {reason}"
        );
    }

    // Two codex verifier partitions and an unrelated claude filer. Capacity
    // holds only the failed partition's crew, including in the escalation round.
    for escalation in [false, true] {
        let workspace = workspace_with(config);
        let task = workspace.task("capacity belongs to the failed verifier partition");
        let mut run = finished_run(
            "jrun-verifier-capacity",
            now,
            now - Duration::minutes(5),
            JobRunState::Failed,
            Some("claude_c"),
            this_host(),
        );
        run.job_id = "finding_verification_pipeline".into();
        let message =
            provider_failure_text(ProviderFailureClass::Capacity, "codex", "fixture failure");
        let partitions = json!([{ "crew": "codex_a" }, { "crew": "codex_b" }]);
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
        state.pipeline = if escalation {
            json!({"prepare": {"partitions": [{"crew": "claude_c"}]},
                "verdict_results": [{"tasks": []}],
                "apply": {"escalation_prepared": {"partitions": partitions}},
                "escalation_errors": [null, message]})
        } else {
            json!({"prepare": {"partitions": partitions}, "verdict_errors": [null, message]})
        };
        save_run(&workspace, &run, Some(&state));
        save_step(
            &workspace,
            &run.run_id,
            now - Duration::minutes(5),
            ProviderFailureClass::Capacity,
            "codex",
        );
        assert_resumes(&workspace, now, &task.id);
        let (_, draw) = low_draw(&workspace);
        assert_eq!(
            draw.crew, "codex_a",
            "only the failing verifier loses capacity: {}",
            draw.source
        );
        assert!(
            draw.source.contains("provider_capacity codex_b"),
            "{}",
            draw.source
        );
    }
}

/// A pilot admitted into debounce before the failure keeps its pending member.
/// The hold that starts during the debounce must still stop its dispatch.
fn a_hold_that_starts_during_debounce_withholds_the_pending_pilot() {
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("login fails while the pilot debounces");
    let started = Utc::now();
    let pending = evaluate(&workspace, started, false);
    assert_eq!(pending.reason, "debouncing", "{pending:?}");

    let now = started + Duration::minutes(3);
    save_failure(
        &workspace,
        "jrun-codex-auth",
        now,
        now - Duration::minutes(1),
        ProviderFailureClass::Unavailable,
        "codex",
        "fixture",
    );
    let reason = assert_withheld(&workspace, now, &task.id);
    assert!(
        reason.contains("provider_unavailable codex") && reason.contains("jrun-codex-auth"),
        "{reason}"
    );
}

crate::scenario_table!(
    task_pilot_routine_honours_host_provider_holds_scenarios,
    unavailable_on_the_only_crew_withholds_and_creates_no_pilot_run,
    an_old_pipeline_repair_failure_holds_until_its_finish_backoff,
    a_hold_that_starts_during_debounce_withholds_the_pending_pilot,
    a_later_success_clears_unavailable_and_capacity,
    a_success_started_before_the_failure_does_not_clear_it,
    a_pilot_that_fails_on_the_provider_login_withholds_the_next_tick,
    a_pilot_run_counts_as_the_crew_it_drew,
    recovery_and_verification_count_their_recorded_draws,
    an_auth_probe_recovery_clears_unavailable,
    unavailable_holds_every_crew_of_the_provider,
    another_providers_crew_is_drawn_and_the_routine_still_admits,
    a_probe_does_not_clear_provider_wide_capacity_but_a_later_success_does,
    capacity_on_the_only_crew_withholds_and_a_probe_does_not_clear_it,
    capacity_on_one_crew_leaves_another_crew_of_that_provider,
    another_hosts_failure_does_not_withhold,
    limit_and_refusal_failures_do_not_withhold_the_routine,
);
