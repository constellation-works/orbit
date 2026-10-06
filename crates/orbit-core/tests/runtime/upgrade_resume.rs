#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

//! [ORB-14273] Clock sweep auto-resume for upgrade-interrupted runs:
//! after the executable generation settles, the clock resumes each
//! upgrade-interrupted run at most once. Runs interrupted for other
//! reasons, claimed follower leaves, and live workers are untouched.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
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
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, JobRunStepParams, JobRunStoreBackend, LocalPullAdmission,
    LocalPullPhase, PullDestination,
};
use orbit_types::workflow::{JobRunState, JobTargetType};
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
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
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

    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );

    TestContext {
        _root: root,
        global,
        runtime,
        jobs,
    }
}

#[test]
fn clock_sweep_resumes_upgrade_interrupted_run_once_after_generation_settles() {
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
    ctx.runtime
        .record_upgrade_interruption(&run_upgrade.run_id, 1001, ParticipantRole::Drain);
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
    ctx.runtime
        .record_upgrade_interruption(&run_claimed.run_id, 1002, ParticipantRole::Drain);
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
    ctx.runtime
        .record_upgrade_interruption(&run_dry.run_id, 1003, ParticipantRole::Drain);

    let dry_sweep = run_sweep_at_with_providers(
        &ctx.global,
        SweepOptions {
            dry_run: true,
            sweep_cadence_seconds: 60,
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
