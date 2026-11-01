//! [ORB-20866] A `parallel:` branch whose script step kept output files is
//! checkpointed with that exact output set when it succeeds before a
//! sibling auto-task branch parks. The resumed run restores the set (the
//! same `steps.<id>.files` references) only after the host verifies it, and
//! refuses with `script_outputs_unavailable` when the stored set is gone,
//! never re-running the script to mint a replacement.

use std::collections::BTreeMap;
use std::sync::Mutex as StdMutex;

use chrono::{Duration, Utc};
use orbit_engine::{
    AutoTaskAdmission, AutoTaskObservation, AutoTaskReadiness, AutoTaskStepRequest, DispatchError,
    ScriptStepEnd, ScriptStepOutputs, ScriptStepRan, ScriptStepRequest, ScriptStepRun,
    execute_job_with_resume,
};
use orbit_types::workflow::activity_job::{
    JobV2, JobV2Step, JobV2StepBody, JoinMode, ParallelBlock, ScriptExecutionRecord,
    ScriptOutputFile, ScriptOutputSet, TargetRef, script_failure, script_output_set_id,
};
use orbit_types::workflow::{
    AUTO_TASK_CHECKPOINT_VERSION, AutoTaskReceipt, AutoTaskRunCheckpoint, AutoTaskStepRecord,
    PipelineState,
};
use serde_json::{Value, json};

use super::job_support::*;

/// Host that parks the `wait` branch until its phase settles it, keeps parallel branch
/// checkpoints, and stores each script output set it "kept". Verification
/// accepts a set only while it is stored, unchanged, and produced by the
/// consuming run.
struct ScriptBranchHost {
    /// 0 waiting, 1 ready to admit, 2 accepted receipt.
    phase: StdMutex<u8>,
    branches: StdMutex<BTreeMap<String, BTreeMap<String, Value>>>,
    records: StdMutex<BTreeMap<String, AutoTaskStepRecord>>,
    stored: StdMutex<BTreeMap<String, ScriptOutputSet>>,
    verified: StdMutex<Vec<String>>,
    scripts: StdMutex<Vec<ScriptStepRequest>>,
}

impl ScriptBranchHost {
    fn new() -> Self {
        Self {
            phase: StdMutex::new(0),
            branches: StdMutex::new(BTreeMap::new()),
            records: StdMutex::new(BTreeMap::new()),
            stored: StdMutex::new(BTreeMap::new()),
            verified: StdMutex::new(Vec::new()),
            scripts: StdMutex::new(Vec::new()),
        }
    }

    fn script_runs(&self, step_id: &str) -> usize {
        self.scripts
            .lock()
            .expect("scripts")
            .iter()
            .filter(|request| request.step_id == step_id)
            .count()
    }

    fn script_input(&self, step_id: &str) -> Value {
        self.scripts
            .lock()
            .expect("scripts")
            .iter()
            .find(|request| request.step_id == step_id)
            .map(|request| request.input.clone())
            .expect("script ran")
    }

    fn resume_state(&self, run_id: &str) -> PipelineState {
        let now = Utc::now();
        let mut state = PipelineState::new(run_id.to_string(), "script-branch".into(), json!({}));
        state.auto_task = Some(AutoTaskRunCheckpoint {
            version: AUTO_TASK_CHECKPOINT_VERSION,
            frozen_job: String::new(),
            frozen_targets: BTreeMap::new(),
            started_at: now,
            source_revision: None,
            expires_at: now + Duration::hours(24),
            steps: self.records.lock().expect("records").clone(),
            branches: self.branches.lock().expect("branches").clone(),
            parked: None,
        });
        state
    }
}

fn kept_set(run_id: &str, step_id: &str) -> ScriptOutputSet {
    let files = vec![ScriptOutputFile {
        name: "shot.png".to_string(),
        bytes: 70,
        media_type: "image/png".to_string(),
        sha256: "a".repeat(64),
        path: format!("/store/{run_id}/{step_id}/files/shot.png"),
    }];
    ScriptOutputSet {
        workspace_id: "ws_scripts".to_string(),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
        set_id: script_output_set_id("ws_scripts", run_id, step_id, &files),
        files,
    }
}

impl orbit_engine::test_support::NullRuntimeHost for ScriptBranchHost {
    fn run_script_step(&self, request: &ScriptStepRequest) -> ScriptStepRun {
        self.scripts.lock().expect("scripts").push(request.clone());
        let outputs = if request.step_id == "shoot" {
            let set = kept_set(&request.run_id, &request.step_id);
            self.stored
                .lock()
                .expect("stored")
                .insert(set.set_id.clone(), set.clone());
            ScriptStepOutputs::Kept(set)
        } else {
            ScriptStepOutputs::None
        };
        ScriptStepRun::Ran(Box::new(ScriptStepRan {
            end: ScriptStepEnd::Exited(0),
            stdout: br#"{"ok": true}"#.to_vec(),
            stdout_over_cap: false,
            output_schema: Value::Null,
            log: Vec::new(),
            record: ScriptExecutionRecord {
                script: request.script.clone(),
                ..ScriptExecutionRecord::default()
            },
            outputs,
        }))
    }

    fn verify_script_output_set(
        &self,
        consumer_run_id: &str,
        set: &ScriptOutputSet,
    ) -> Result<(), String> {
        self.verified
            .lock()
            .expect("verified")
            .push(set.set_id.clone());
        if set.run_id != consumer_run_id {
            return Err(format!("was produced by run {}", set.run_id));
        }
        match self.stored.lock().expect("stored").get(&set.set_id) {
            Some(stored) if stored == set => Ok(()),
            Some(_) => Err("does not match its producer record".to_string()),
            None => Err("has no producer record".to_string()),
        }
    }

    fn observe_auto_task_step(
        &self,
        _run_id: &str,
        request: &AutoTaskStepRequest,
    ) -> Result<AutoTaskObservation, DispatchError> {
        let record = self
            .records
            .lock()
            .expect("records")
            .get(&request.step_id)
            .cloned();
        let now = Utc::now();
        let phase = *self.phase.lock().expect("phase");
        let (readiness, receipt) = if phase == 1 {
            (AutoTaskReadiness::Ready, None)
        } else if phase == 2 {
            (
                AutoTaskReadiness::Ready,
                Some(AutoTaskReceipt {
                    task_id: "T-HOLD".to_string(),
                    task_status: "done".to_string(),
                    accepted: true,
                    settled_at: now,
                    observed_at: now,
                }),
            )
        } else {
            (AutoTaskReadiness::Waiting("board.empty".to_string()), None)
        };
        Ok(AutoTaskObservation {
            now,
            record,
            action_key: format!("run:{}", request.step_id),
            input_digest: "digest".to_string(),
            definition_hold: None,
            receipt,
            bound_status: None,
            deadline: Some(now + Duration::hours(24)),
            dynamic_expiry: false,
            expired: None,
            readiness,
            recheck_at: now + Duration::seconds(30),
        })
    }

    fn checkpoint_auto_task_step(
        &self,
        _run_id: &str,
        step_id: &str,
        record: &AutoTaskStepRecord,
    ) -> Result<(), DispatchError> {
        self.records
            .lock()
            .expect("records")
            .insert(step_id.to_string(), record.clone());
        Ok(())
    }

    fn admit_auto_task_step(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> Result<AutoTaskAdmission, DispatchError> {
        Ok(AutoTaskAdmission::Bound {
            task_id: "T-HOLD".to_string(),
            minted: true,
        })
    }

    fn checkpoint_parallel_branch(
        &self,
        _run_id: &str,
        step_id: &str,
        branch_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        self.branches
            .lock()
            .expect("branches")
            .entry(step_id.to_string())
            .or_default()
            .insert(branch_id.to_string(), output.clone());
        Ok(())
    }

    fn park_job_run(
        &self,
        _run_id: &str,
        _park: &orbit_types::workflow::ParkedWait,
    ) -> Result<(), DispatchError> {
        Ok(())
    }
}

fn step(id: &str, target: &str, default_input: Option<Value>) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        stage: None,
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        wait_until: None,
        expire_when: None,
        reuse_when: None,
        require_result: None,
        body: JobV2StepBody::TargetRef(TargetRef {
            target: target.to_string(),
            default_input,
            timeout_seconds: 0,
            session: None,
        }),
    }
}

/// `gate` runs the `shoot` script beside a waiting auto-task branch; the
/// later `consume` script reads the kept file's references.
fn script_branch_job() -> JobV2 {
    let mut wait = step("wait", "auto-task:hold", None);
    wait.wait_until = Some("board.empty".to_string());
    let gate = JobV2Step {
        body: JobV2StepBody::Parallel {
            parallel: ParallelBlock {
                join: JoinMode::All,
                branches: vec![wait, step("shoot", "script:shoot", None)],
            },
        },
        ..step("gate", "unused", None)
    };
    let consume = step(
        "consume",
        "script:consume",
        Some(json!({
            "path": "{{ steps.shoot.files.shot.png.path }}",
            "sha256": "{{ steps.shoot.files.shot.png.sha256 }}",
        })),
    );
    job_with_steps(vec![gate, consume])
}

#[test]
fn parked_replay_restores_a_script_branch_output_set_and_refuses_a_lost_one() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::parked_replay_restores_a_script_branch_output_set_and_refuses_a_lost_one"
    )) {
        return;
    }

    let host = ScriptBranchHost::new();
    let job = script_branch_job();
    let run_id = "run-script-branch";
    let writer = std::sync::Arc::new(test_writer(run_id));
    let original = kept_set(run_id, "shoot");

    let first = execute_job_with_resume(&job, Value::Null, run_id, writer.clone(), &host, None)
        .expect("first pass parks");
    assert!(first.parked.is_some(), "the waiting branch parks the join");
    assert_eq!(host.script_runs("shoot"), 1);
    let checkpoint = host.branches.lock().expect("branches")["gate"]["shoot"].clone();
    assert_eq!(checkpoint["kind"], json!("script_branch"));
    assert_eq!(checkpoint["output"], json!({"ok": true}));
    assert_eq!(
        checkpoint["script_output_sets"]["shoot"],
        serde_json::to_value(&original).expect("set json"),
        "the branch checkpoint keeps the exact accepted output set"
    );

    let second = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&host.resume_state(run_id)),
    )
    .expect("second pass parks again");
    assert!(second.parked.is_some());
    assert_eq!(
        host.script_runs("shoot"),
        1,
        "a succeeded script branch is restored, not re-run, while its sibling parks"
    );
    assert_eq!(
        *host.verified.lock().expect("verified"),
        vec![original.set_id.clone()],
        "the restored set is verified before it is exposed"
    );

    *host.phase.lock().expect("phase") = 1;
    let admitted = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&host.resume_state(run_id)),
    )
    .expect("admission parks on the bound task");
    assert!(admitted.parked.is_some());
    assert_eq!(host.script_runs("shoot"), 1);

    *host.phase.lock().expect("phase") = 2;
    let done = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&host.resume_state(run_id)),
    )
    .expect("the settled join finishes");
    assert!(done.success, "{done:?}");
    assert_eq!(host.script_runs("shoot"), 1);
    let consumed = host.script_input("consume");
    assert_eq!(consumed["path"], json!(original.files[0].path));
    assert_eq!(consumed["sha256"], json!(original.files[0].sha256));

    // The stored set is lost: the resume refuses before any step runs and
    // the script is not run again to replace it.
    *host.phase.lock().expect("phase") = 0;
    host.stored.lock().expect("stored").clear();
    let scripts_before = host.scripts.lock().expect("scripts").len();
    let error = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&host.resume_state(run_id)),
    )
    .expect_err("a lost output set refuses the resume");
    assert_eq!(error.failure_code(), script_failure::OUTPUTS_UNAVAILABLE);
    assert_eq!(host.scripts.lock().expect("scripts").len(), scripts_before);

    // A genuine set another run produced is not substituted into this run's
    // branch checkpoint.
    let foreign = kept_set("run-unrelated", "shoot");
    host.stored
        .lock()
        .expect("stored")
        .insert(foreign.set_id.clone(), foreign.clone());
    let mut substituted = host.resume_state(run_id);
    let branches = &mut substituted.auto_task.as_mut().expect("checkpoint").branches;
    branches
        .get_mut("gate")
        .and_then(|gate| gate.get_mut("shoot"))
        .expect("shoot checkpoint")["script_output_sets"]["shoot"] =
        serde_json::to_value(&foreign).expect("set json");
    let error =
        execute_job_with_resume(&job, Value::Null, run_id, writer, &host, Some(&substituted))
            .expect_err("a foreign run's set refuses the resume");
    assert_eq!(error.failure_code(), script_failure::OUTPUTS_UNAVAILABLE);
    assert_eq!(host.scripts.lock().expect("scripts").len(), scripts_before);
}
