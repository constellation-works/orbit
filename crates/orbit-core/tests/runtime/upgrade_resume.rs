#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

//! [ORB-14273] Clock sweep auto-resume for upgrade-interrupted runs:
//! after the executable generation settles, the clock resumes each
//! upgrade-interrupted run at most once. Runs interrupted for other
//! reasons, claimed follower leaves, and live workers are untouched.
//! [ORB-14320] Only the current upgrade's interruptions are resumed: an
//! earlier upgrade's, an elapsed or stopped drain, and a superseded routine
//! run are skipped, and every decision is audited once. A run whose claim
//! cannot be read is deferred without a decision, so a later tick decides it.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use fs2::FileExt;
use orbit_common::fs::generation::{
    CompatibilityIdentity, LedgerCompatibility, ParticipantRole, PendingSwitch, pending_switch,
};
use orbit_core::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use orbit_core::application::routines::{
    RoutineMachineIdentity, SweepOptions, run_sweep_at_with_providers,
};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_store::contracts::{
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract, AuditEventFilter,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, JobRunStepParams, JobRunStoreBackend, LocalPullAdmission,
    LocalPullPhase, PullDestination,
};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    ChildDispatch, JobRunState, JobRunTrigger, JobTargetType, PipelineState,
    REVIEW_CONTRACT_VERSION, ReviewAdmission, ReviewTiming,
};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::json;
use tempfile::TempDir;

struct SingleWorkspace(OrbitRuntime);

impl RoutineWorkspaceProvider for SingleWorkspace {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.0.workspace_id()?,
            name: "test-workspace".into(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            entries: vec![(workspace, self.0.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }
}

struct TestContext {
    _root: TempDir,
    global: PathBuf,
    workspace: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_UPGRADE_RESUME_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }

    let home = TempDir::new().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

fn record_upgrade_interruption(jobs: &dyn JobRunStoreBackend, run_id: &str) {
    record_upgrade_interruption_at(jobs, run_id, Utc::now());
}

fn record_upgrade_interruption_at(jobs: &dyn JobRunStoreBackend, run_id: &str, now: DateTime<Utc>) {
    jobs.complete_job_run_step(
        run_id,
        &JobRunStepParams {
            step_index: 1,
            target_type: JobTargetType::Activity,
            target_id: "upgrade-quiesce".to_string(),
            started_at: now,
            finished_at: now,
            duration_ms: Some(1),
            exit_code: None,
            agent_response_json: None,
            state: JobRunState::Interrupted,
            error_code: Some("upgrade_quiesce".to_string()),
            error_message: Some("interrupted at an upgrade generation boundary".to_string()),
        },
    )
    .unwrap();
    jobs.finalize_job_run(run_id, JobRunState::Interrupted, now, None)
        .unwrap();
}

fn setup_context() -> TestContext {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let jobs_dir = global.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();

    let job_yaml = "\
schemaVersion: 2
kind: Job
metadata:
  name: test_pipeline
spec:
  state: enabled
  steps: []
";
    std::fs::write(jobs_dir.join("test_pipeline.yaml"), job_yaml).unwrap();

    let workspace = repo.join(".orbit");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );

    TestContext {
        _root: root,
        global,
        workspace,
        runtime,
        jobs,
    }
}

#[test]
fn clock_sweep_resumes_upgrade_interrupted_run_once_after_generation_settles() {
    let test_name =
        "upgrade_resume::clock_sweep_resumes_upgrade_interrupted_run_once_after_generation_settles";
    if run_isolated_test(test_name) {
        return;
    }
    let ctx = setup_context();

    // 1. Create an upgrade-interrupted run.
    let run_upgrade = ctx
        .jobs
        .insert_job_run(
            "test_pipeline",
            1,
            Utc::now(),
            Some(json!({"param": "value"})),
            None,
        )
        .unwrap();
    record_upgrade_interruption(ctx.jobs.as_ref(), &run_upgrade.run_id);
    let run_upgrade_stored = ctx.jobs.get_job_run(&run_upgrade.run_id).unwrap().unwrap();
    assert_eq!(run_upgrade_stored.state, JobRunState::Interrupted);
    assert!(
        run_upgrade_stored
            .steps
            .iter()
            .any(|s| s.error_code.as_deref() == Some("upgrade_quiesce"))
    );

    // 2. Create a run interrupted for a non-upgrade reason (e.g., worker_terminated).
    let run_other = ctx
        .jobs
        .insert_job_run("test_pipeline", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    let now = Utc::now();
    ctx.jobs
        .complete_job_run_step(
            &run_other.run_id,
            &JobRunStepParams {
                step_index: 1,
                target_type: JobTargetType::Activity,
                target_id: "diagnostic".to_string(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Interrupted,
                error_code: Some("worker_terminated".to_string()),
                error_message: Some("worker process crashed".to_string()),
            },
        )
        .unwrap();
    ctx.jobs
        .finalize_job_run(&run_other.run_id, JobRunState::Interrupted, now, None)
        .unwrap();
    let run_other_stored = ctx.jobs.get_job_run(&run_other.run_id).unwrap().unwrap();
    assert_eq!(run_other_stored.state, JobRunState::Interrupted);

    // 3. Create a claimed follower leaf interrupted for upgrade.
    let run_claimed = ctx
        .jobs
        .insert_job_run("test_pipeline", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    record_upgrade_interruption(ctx.jobs.as_ref(), &run_claimed.run_id);
    // Mark as local pull admission in sqlite so `local_pull_for_run` returns Some.
    ctx.jobs.local_pull_admissions().unwrap();
    let admission = LocalPullAdmission {
        destination: PullDestination {
            owner_machine_id: "owner-m".into(),
            owner_workspace_id: "owner-ws".into(),
            selector: "owner-m/owner-ws".into(),
            execution_machine_id: "exec-m".into(),
        },
        request: AdmissionRequest {
            request_id: "req-1".into(),
            caller_version: "1".into(),
            caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            caller_fingerprint: None,
            caller_before_pr: false,
            review_gate: false,
            run_context: AdmissionRunContext {
                run_id: "parent-run".into(),
                job_name: "workspace_auto_pipeline".into(),
                machine_name: None,
            },
            ship: AdmissionShipContract {
                mode: "local".into(),
                base_branch: "main".into(),
                landing_branch: "main".into(),
                before_pr: false,
                completion: "review".into(),
                authorization_reference: None,
                review: None,
            },
            crews: None,
            os: None,
        },
        receipt: None,
        leaf_run_id: Some(run_claimed.run_id.clone()),
        phase: LocalPullPhase::Claimed,
        settlement: None,
        refusal: None,
        settlement_refusal: None,
    };
    ctx.runtime
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO local_pull_admissions (
                workspace_id, owner_machine, owner_workspace, execution_machine,
                request_id, claim_id, leaf_run_id, record_json
             ) VALUES (?1, 'owner-m', 'owner-ws', 'exec-m', 'req-1', NULL, ?2, ?3)",
            rusqlite::params![
                ctx.runtime.workspace_id().unwrap(),
                run_claimed.run_id,
                serde_json::to_string(&admission).unwrap()
            ],
        )
        .unwrap();
    assert!(
        ctx.jobs
            .local_pull_for_run(&run_claimed.run_id)
            .unwrap()
            .is_some()
    );

    let provider = SingleWorkspace(ctx.runtime.clone());
    let machine = RoutineMachineIdentity {
        machine_id: "test-mach".into(),
        machine_name: "test-host".into(),
    };

    // ---- Phase 1: Generation switch pending ----
    let pending_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(ctx.global.join(".generation-pending.json"))
        .unwrap();
    pending_file.lock_exclusive().unwrap();
    let switch = PendingSwitch {
        pid: 9999,
        role: ParticipantRole::Command,
        digest: "test-digest".into(),
        target: CompatibilityIdentity {
            store_schema: LedgerCompatibility {
                version: 1,
                writer_floor: 1,
                reader_floor: 1,
            },
            workspace_layout: LedgerCompatibility {
                version: 1,
                writer_floor: 1,
                reader_floor: 1,
            },
            features: BTreeMap::new(),
        },
        requested_at: Utc::now(),
        deadline: Utc::now() + chrono::Duration::seconds(60),
    };
    serde_json::to_writer(&pending_file, &switch).unwrap();
    pending_file.sync_all().unwrap();
    assert!(pending_switch(&ctx.global).is_some());

    // Clock tick during pending generation switch must NOT resume anything.
    let sweep = run_sweep_at_with_providers(
        &ctx.global,
        SweepOptions::default(),
        machine.clone(),
        &provider,
    )
    .expect("sweep runs");
    assert!(!sweep.lock_busy);
    assert!(
        ctx.jobs
            .job_run_retries(&run_upgrade.run_id, 1)
            .unwrap()
            .is_empty(),
        "must not resume while generation switch is pending"
    );

    // ---- Phase 2: Generation settles ----
    pending_file.unlock().unwrap();
    drop(pending_file);
    let _ = std::fs::remove_file(ctx.global.join(".generation-pending.json"));
    assert!(pending_switch(&ctx.global).is_none());

    // Clock tick after generation settles must resume the upgrade run.
    let sweep = run_sweep_at_with_providers(
        &ctx.global,
        SweepOptions::default(),
        machine.clone(),
        &provider,
    )
    .expect("sweep runs");
    assert!(!sweep.lock_busy);

    // Check results:
    // a) `run_upgrade` was resumed once!
    let retries = ctx.jobs.job_run_retries(&run_upgrade.run_id, 10).unwrap();
    assert_eq!(
        retries.len(),
        1,
        "upgrade-interrupted run must be resumed exactly once"
    );
    let resumed_run = &retries[0];
    assert_eq!(
        resumed_run.retry_source_run_id.as_deref(),
        Some(run_upgrade.run_id.as_str())
    );

    // b) `run_other` was NOT resumed!
    assert!(
        ctx.jobs
            .job_run_retries(&run_other.run_id, 1)
            .unwrap()
            .is_empty(),
        "run interrupted for worker_terminated must remain untouched"
    );

    // c) `run_claimed` was NOT resumed!
    assert!(
        ctx.jobs
            .job_run_retries(&run_claimed.run_id, 1)
            .unwrap()
            .is_empty(),
        "claimed follower leaf must remain untouched for owner claim recovery"
    );

    // ---- Phase 3: Repeat clock sweep (second tick) ----
    // Subsequent clock ticks must NOT re-resume the already-resumed run.
    let sweep2 = run_sweep_at_with_providers(
        &ctx.global,
        SweepOptions::default(),
        machine.clone(),
        &provider,
    )
    .expect("sweep runs");
    assert!(!sweep2.lock_busy);

    let retries_after_second_tick = ctx.jobs.job_run_retries(&run_upgrade.run_id, 10).unwrap();
    assert_eq!(
        retries_after_second_tick.len(),
        1,
        "repeated tick must not create another retry descendant ('at most once')"
    );
    assert!(
        ctx.jobs
            .job_run_retries(&run_other.run_id, 1)
            .unwrap()
            .is_empty(),
        "run_other remains untouched on repeat tick"
    );
    assert!(
        ctx.jobs
            .job_run_retries(&run_claimed.run_id, 1)
            .unwrap()
            .is_empty(),
        "run_claimed remains untouched on repeat tick"
    );

    // ---- Phase 4: Dry-run check ----
    let run_dry = ctx
        .jobs
        .insert_job_run("test_pipeline", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    record_upgrade_interruption(ctx.jobs.as_ref(), &run_dry.run_id);

    let dry_sweep = run_sweep_at_with_providers(
        &ctx.global,
        SweepOptions {
            dry_run: true,
            sweep_cadence_seconds: 60,
            ..SweepOptions::default()
        },
        machine,
        &provider,
    )
    .expect("dry run sweep");
    assert!(!dry_sweep.lock_busy);
    assert!(
        ctx.jobs
            .job_run_retries(&run_dry.run_id, 1)
            .unwrap()
            .is_empty(),
        "dry-run sweep must not resume runs"
    );
}

const UPGRADE_RESUME_AUDIT: &str = "pipeline.run.upgrade_resume";

/// Insert a run of `job_id`, seed its run state, and record it interrupted by
/// an upgrade at `interrupted_at`.
fn upgrade_interrupted_run(
    ctx: &TestContext,
    job_id: &str,
    interrupted_at: DateTime<Utc>,
    seed: impl FnOnce(&mut PipelineState),
) -> String {
    let input = if ["workspace_auto_pipeline", "workspace_pull_pipeline"].contains(&job_id) {
        json!({"review": review_admission(&ctx.runtime)})
    } else {
        json!({})
    };
    let run = ctx
        .jobs
        .insert_job_run(job_id, 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), job_id.to_string(), input);
    seed(&mut state);
    ctx.jobs.write_run_state(&run.run_id, &state).unwrap();
    record_upgrade_interruption_at(ctx.jobs.as_ref(), &run.run_id, interrupted_at);
    // Keep creation order strict for the supersession check.
    std::thread::sleep(std::time::Duration::from_millis(20));
    run.run_id
}

fn review_admission(runtime: &OrbitRuntime) -> ReviewAdmission {
    let policy = runtime.operation_policy();
    ReviewAdmission {
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: policy.version,
        timing: if policy.review_before_pr.value {
            ReviewTiming::BeforePr
        } else {
            ReviewTiming::None
        },
        timing_source: policy.review_before_pr.source.label().into(),
        crew: policy.review_crew.value.clone(),
        crew_source: policy.review_crew.source.label().into(),
        budget: policy.review_budget(),
        required_validation_commands: Some(
            runtime.workflow_required_validation_commands().to_vec(),
        ),
        baseline_commands: runtime.review_baseline_commands().to_vec(),
        captured_at: Utc::now(),
        host_evidence: Vec::new(),
    }
}

/// Every successful auto-resume decision recorded for `run_id`.
fn decisions(ctx: &TestContext, run_id: &str) -> Vec<serde_json::Value> {
    ctx.runtime
        .list_audit_events_filtered(&AuditEventFilter {
            tool_name: Some(UPGRADE_RESUME_AUDIT.to_string()),
            status: Some(AuditEventStatus::Success),
            job_run_id: Some(run_id.to_string()),
            limit: 10,
            ..AuditEventFilter::default()
        })
        .unwrap()
        .iter()
        .map(|event| serde_json::from_str(event.arguments_json.as_deref().unwrap()).unwrap())
        .collect()
}

fn assert_skipped(ctx: &TestContext, run_id: &str, reason: &str) {
    assert!(
        ctx.jobs.job_run_retries(run_id, 1).unwrap().is_empty(),
        "run skipped for {reason} must not be resumed"
    );
    let decisions = decisions(ctx, run_id);
    assert_eq!(decisions.len(), 1, "one audited decision: {decisions:?}");
    assert_eq!(decisions[0]["decision"], "skipped", "{decisions:?}");
    assert_eq!(decisions[0]["reason"], reason, "{decisions:?}");
}

#[test]
fn clock_sweep_resumes_only_the_current_upgrades_interruptions_and_audits_each_decision() {
    let test_name = "upgrade_resume::clock_sweep_resumes_only_the_current_upgrades_interruptions_and_audits_each_decision";
    if run_isolated_test(test_name) {
        return;
    }
    // This test binary cannot be re-executed as a worker; a resumed run's
    // substitute exits at once, so submission itself succeeds.
    orbit_core::test_support::install_substitute_pipeline_worker(["true".to_string()]);
    let mut ctx = setup_context();
    let now = Utc::now();
    let weeks_ago = now - Duration::days(17);
    let deadline = |at: DateTime<Utc>| at.to_rfc3339_opts(SecondsFormat::Secs, true);

    // A delivery run captured while before-PR review was off must not resume
    // after the workspace turns it on: successful checkpoints would otherwise
    // carry the old admission past today's review gate.
    let review_changed = upgrade_interrupted_run(&ctx, "workspace_auto_pipeline", now, |state| {
        state.record_pipeline_output(
            "open_window",
            json!({"deadline": deadline(now + Duration::hours(1))}),
        );
    });
    std::fs::write(
        ctx.workspace.join("config.toml"),
        "[review]\nbefore_pr = true\n",
    )
    .unwrap();
    ctx.runtime = OrbitRuntime::from_roots(&ctx.global, &ctx.workspace).unwrap();
    assert!(ctx.runtime.operation_policy().review_before_pr.value);

    // An earlier upgrade's interruption, and the current upgrade's.
    let historical = upgrade_interrupted_run(&ctx, "test_pipeline", weeks_ago, |_| {});
    let current = upgrade_interrupted_run(&ctx, "test_pipeline", now, |_| {});

    // Drains the current upgrade interrupted: one whose window has elapsed,
    // one an operator stopped while its window was still open, and a ship
    // wrapper that never stamped a window of its own.
    let expired_drain = upgrade_interrupted_run(&ctx, "workspace_auto_pipeline", now, |state| {
        state.record_pipeline_output(
            "open_window",
            json!({"deadline": deadline(now - Duration::minutes(5))}),
        );
    });
    let stopped_drain = upgrade_interrupted_run(&ctx, "workspace_pull_pipeline", now, |state| {
        state.record_pipeline_output(
            "open_window",
            json!({"deadline": deadline(now + Duration::hours(1))}),
        );
        state.set_drain_admissions_stop("operator".into(), Some("on-call".into()));
    });
    let windowless_ship = upgrade_interrupted_run(&ctx, "workspace_ship_pipeline", now, |_| {});

    // A ship wrapper delegates its window and stop control to its auto drain
    // child. The wrapper itself has no stop flag, so inspect the child state.
    let child_input = json!({"review": review_admission(&ctx.runtime)});
    let stopped_ship_child = ctx
        .jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            Utc::now(),
            Some(child_input.clone()),
            None,
        )
        .unwrap();
    let mut child_state = PipelineState::new(
        stopped_ship_child.run_id.clone(),
        "workspace_auto_pipeline".into(),
        child_input,
    );
    child_state.record_pipeline_output(
        "open_window",
        json!({"deadline": deadline(now + Duration::hours(1))}),
    );
    child_state.set_drain_admissions_stop("operator".into(), Some("on-call".into()));
    ctx.jobs
        .write_run_state(&stopped_ship_child.run_id, &child_state)
        .unwrap();
    let stopped_ship = upgrade_interrupted_run(&ctx, "workspace_ship_pipeline", now, |state| {
        state.record_child_dispatch(ChildDispatch::submitted(
            stopped_ship_child.run_id.clone(),
            "workspace_auto_pipeline".into(),
            "invoke_and_wait".into(),
            true,
            false,
            now,
        ));
    });

    // A routine run that a newer fire of the same routine superseded, and one
    // whose only newer run belongs to another routine.
    let routine = |name: &str| {
        let trigger = JobRunTrigger::routine(name, "slot");
        move |state: &mut PipelineState| state.trigger = Some(trigger)
    };
    let superseded = upgrade_interrupted_run(&ctx, "test_pipeline", now, routine("ci-sweep"));
    let unsuperseded = upgrade_interrupted_run(&ctx, "test_pipeline", now, routine("pilot"));
    let newer = ctx
        .jobs
        .insert_job_run("test_pipeline", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    let mut newer_state =
        PipelineState::new(newer.run_id.clone(), "test_pipeline".into(), json!({}));
    newer_state.trigger = Some(JobRunTrigger::routine("ci-sweep", "next-slot"));
    ctx.jobs
        .write_run_state(&newer.run_id, &newer_state)
        .unwrap();
    ctx.jobs
        .finalize_job_run(&newer.run_id, JobRunState::Cancelled, Utc::now(), None)
        .unwrap();

    let provider = SingleWorkspace(ctx.runtime.clone());
    let machine = RoutineMachineIdentity {
        machine_id: "test-mach".into(),
        machine_name: "test-host".into(),
    };
    let tick = || {
        let sweep = run_sweep_at_with_providers(
            &ctx.global,
            SweepOptions::default(),
            machine.clone(),
            &provider,
        )
        .expect("sweep runs");
        assert!(!sweep.lock_busy);
    };
    tick();

    for (run_id, label) in [(&current, "current"), (&unsuperseded, "unsuperseded")] {
        let retries = ctx.jobs.job_run_retries(run_id, 10).unwrap();
        assert_eq!(
            retries.len(),
            1,
            "the current upgrade's {label} run resumes once"
        );
        let decisions = decisions(&ctx, run_id);
        assert_eq!(decisions.len(), 1, "{decisions:?}");
        assert_eq!(decisions[0]["decision"], "resumed", "{decisions:?}");
        assert_eq!(decisions[0]["resumed_run_id"], retries[0].run_id.as_str());
    }
    assert_skipped(&ctx, &historical, "interrupted_before_current_upgrade");
    assert_skipped(&ctx, &review_changed, "review_admission_changed");
    assert_skipped(&ctx, &expired_drain, "drain_window_elapsed");
    assert_skipped(&ctx, &stopped_drain, "drain_admissions_stopped");
    assert_skipped(&ctx, &windowless_ship, "drain_window_elapsed");
    assert_skipped(&ctx, &stopped_ship, "drain_admissions_stopped");
    assert_skipped(&ctx, &superseded, "superseded");

    // A skipped drain admitted nothing: no run of a drain job exists beyond
    // the interrupted ones.
    for job in [
        "workspace_auto_pipeline",
        "workspace_pull_pipeline",
        "workspace_ship_pipeline",
    ] {
        let expected = match job {
            "workspace_auto_pipeline" => 3, // includes the stopped ship child
            "workspace_pull_pipeline" => 1,
            "workspace_ship_pipeline" => 2,
            _ => unreachable!(),
        };
        assert_eq!(
            ctx.jobs.list_job_runs(job).unwrap().len(),
            expected,
            "{job}"
        );
    }

    // A decided run is not reconsidered: the next tick adds no decision and
    // no resume.
    tick();
    for run_id in [
        &historical,
        &review_changed,
        &expired_drain,
        &stopped_drain,
        &windowless_ship,
        &stopped_ship,
        &superseded,
    ] {
        assert_eq!(decisions(&ctx, run_id).len(), 1, "{run_id} decided once");
        assert!(ctx.jobs.job_run_retries(run_id, 1).unwrap().is_empty());
    }
    for run_id in [&current, &unsuperseded] {
        assert_eq!(decisions(&ctx, run_id).len(), 1, "{run_id} decided once");
        assert_eq!(ctx.jobs.job_run_retries(run_id, 10).unwrap().len(), 1);
    }
}

#[test]
fn clock_sweep_caps_successful_upgrade_resumes_per_tick() {
    let test_name = "upgrade_resume::clock_sweep_caps_successful_upgrade_resumes_per_tick";
    if run_isolated_test(test_name) {
        return;
    }
    // This test binary cannot be re-executed as a worker; resumed substitutes
    // exit at once, so each submission completes without starting another run.
    orbit_core::test_support::install_substitute_pipeline_worker(["true".to_string()]);
    let ctx = setup_context();
    let now = Utc::now();
    let runs = (0..40)
        .map(|_| upgrade_interrupted_run(&ctx, "test_pipeline", now, |_| {}))
        .collect::<Vec<_>>();

    let provider = SingleWorkspace(ctx.runtime.clone());
    let machine = RoutineMachineIdentity {
        machine_id: "test-mach".into(),
        machine_name: "test-host".into(),
    };
    let tick = || {
        let sweep = run_sweep_at_with_providers(
            &ctx.global,
            SweepOptions::default(),
            machine.clone(),
            &provider,
        )
        .expect("sweep runs");
        assert!(!sweep.lock_busy);
    };

    tick();
    let resumed_after_first_tick = runs
        .iter()
        .filter(|run_id| !ctx.jobs.job_run_retries(run_id, 1).unwrap().is_empty())
        .count();
    assert!(
        resumed_after_first_tick > 0 && resumed_after_first_tick < runs.len(),
        "one sweep must resume only part of a backlog larger than its cap"
    );

    tick();
    for run_id in &runs {
        assert_eq!(
            ctx.jobs.job_run_retries(run_id, 1).unwrap().len(),
            1,
            "every run is resumed exactly once across ticks"
        );
        assert_eq!(decisions(&ctx, run_id).len(), 1, "{run_id} decided once");
    }
}

#[test]
fn clock_sweep_defers_an_upgrade_interrupted_run_while_its_claim_is_unreadable() {
    let test_name = "upgrade_resume::clock_sweep_defers_an_upgrade_interrupted_run_while_its_claim_is_unreadable";
    if run_isolated_test(test_name) {
        return;
    }
    // This test binary cannot be re-executed as a worker; a resumed run's
    // substitute exits at once, so submission itself succeeds.
    orbit_core::test_support::install_substitute_pipeline_worker(["true".to_string()]);
    let ctx = setup_context();
    let run_id = upgrade_interrupted_run(&ctx, "test_pipeline", Utc::now(), |_| {});

    let provider = SingleWorkspace(ctx.runtime.clone());
    let machine = RoutineMachineIdentity {
        machine_id: "test-mach".into(),
        machine_name: "test-host".into(),
    };
    let tick = || {
        let sweep = run_sweep_at_with_providers(
            &ctx.global,
            SweepOptions::default(),
            machine.clone(),
            &provider,
        )
        .expect("sweep runs");
        assert!(!sweep.lock_busy);
    };

    // An undecodable claim row makes claim resolution fail, so whether this
    // run is a claimed execution cannot be known.
    let coordination_row = |sql: &str| {
        ctx.runtime
            .sqlite_store()
            .unwrap()
            .connection()
            .lock()
            .unwrap()
            .execute(sql, rusqlite::params![ctx.runtime.workspace_id().unwrap()])
            .unwrap()
    };
    coordination_row(
        "INSERT INTO task_coordination_rows (workspace_id, kind, row_id, payload_json, journal_id, created_at)
         VALUES (?1, 'distributed-execution-claim-v1', 'corrupt-claim', 'not-json', 'sql-only', '2026-10-08T00:00:00Z')",
    );

    tick();
    assert!(
        ctx.jobs.job_run_retries(&run_id, 1).unwrap().is_empty(),
        "a run whose claim cannot be read must not be resumed as unclaimed"
    );
    assert!(
        decisions(&ctx, &run_id).is_empty(),
        "a deferred run is not a decision: {:?}",
        decisions(&ctx, &run_id)
    );

    // Once the claim store reads again, the next tick decides the run.
    coordination_row(
        "DELETE FROM task_coordination_rows WHERE workspace_id = ?1 AND row_id = 'corrupt-claim'",
    );
    tick();
    let decided = decisions(&ctx, &run_id);
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0]["decision"], "resumed", "{decided:?}");
    assert_eq!(ctx.jobs.job_run_retries(&run_id, 10).unwrap().len(), 1);
}
