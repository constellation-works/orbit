//! The scripted host and audit-run harness for the final-recovery
//! scenarios: `RecoveryHost` stands in for Core, and `run*` executes a job
//! through `execute_job_with_resume` while collecting audit attempts.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use orbit_agent::loop_engine::InMemorySink;
use orbit_common::{OrbitError, RecoverableVcsConflict};
use orbit_engine::{
    CompletionObservationAdmission, DispatchError, FinalRecoveryAdmission,
    FinalRecoveryAdmissionRequest, FinalRecoveryApplication, FinalRecoveryApplied, JobOutcome,
    TaskAutomationUpdate, V2AuditWriter, V2SqliteSink, execute_job_with_resume,
};
use orbit_types::workflow::activity_job::{JobV2, V2AuditEventKind};
use orbit_types::workflow::{FinalRecoveryDecision, PipelineState};
use serde_json::{Value, json};

use super::RUN_ID;

/// How a scripted deterministic call ends.
#[derive(Clone)]
pub(super) enum Reply {
    Ok(Value),
    Fail,
    Permanent,
    Observation,
    Diagnostic(String),
    Conflict(RecoverableVcsConflict),
    /// A settled review awaiting named external evidence.
    EvidenceHold,
    /// A child run the step waited on was cancelled.
    ChildCancelled,
}

pub(super) struct RecoveryHost {
    pub(super) replies: Mutex<HashMap<String, VecDeque<Reply>>>,
    pub(super) calls: Mutex<Vec<(String, Value)>>,
    /// Success checkpoints, in call order: `(step_id, output)`.
    pub(super) checkpoints: Mutex<Vec<(String, Value)>>,
    /// Diagnostic checkpoints, in call order: `(step_id, output)`.
    pub(super) diagnostics: Mutex<Vec<(String, Value)>>,
    pub(super) admission: FinalRecoveryAdmission,
    pub(super) admissions: Mutex<Vec<FinalRecoveryAdmissionRequest>>,
    pub(super) applications: Mutex<Vec<FinalRecoveryApplication>>,
    pub(super) updates: Mutex<Vec<(String, TaskAutomationUpdate)>>,
    pub(super) logs: HashMap<String, String>,
    pub(super) workspace_path: String,
    pub(super) primary_root: Option<std::path::PathBuf>,
    pub(super) cli: Option<orbit_engine::ResolvedCliExecutor>,
    pub(super) completion_admission: CompletionObservationAdmission,
}

impl RecoveryHost {
    pub(super) fn new<const N: usize>(replies: [(&str, Vec<Reply>); N]) -> Self {
        Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(a, q)| (a.to_string(), q.into()))
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
            checkpoints: Mutex::new(Vec::new()),
            diagnostics: Mutex::new(Vec::new()),
            admission: FinalRecoveryAdmission::Admitted,
            admissions: Mutex::new(Vec::new()),
            applications: Mutex::new(Vec::new()),
            updates: Mutex::new(Vec::new()),
            logs: HashMap::from([(RUN_ID.to_string(), "this run's failure log".to_string())]),
            workspace_path: "/worktrees/run".to_string(),
            primary_root: None,
            cli: None,
            completion_admission: CompletionObservationAdmission::Unavailable,
        }
    }

    /// Answer admission like a host whose `workflow.final_recovery_crews` is
    /// `[]`.
    pub(super) fn with_empty_pool(mut self) -> Self {
        self.admission = FinalRecoveryAdmission::Skipped {
            reason: "final recovery is disabled: `workflow.final_recovery_crews` is []".to_string(),
        };
        self
    }

    pub(super) fn inputs(&self, action: &str) -> Vec<Value> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(n, _)| n == action)
            .map(|(_, i)| i.clone())
            .collect()
    }

    pub(super) fn count(&self, action: &str) -> usize {
        self.inputs(action).len()
    }

    pub(super) fn actions(&self) -> Vec<String> {
        let calls = self.calls.lock().unwrap();
        calls.iter().map(|(name, _)| name.clone()).collect()
    }

    pub(super) fn applications(&self) -> Vec<FinalRecoveryApplication> {
        self.applications.lock().unwrap().clone()
    }
}

impl orbit_engine::test_support::NullRuntimeHost for RecoveryHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .unwrap()
            .push((action.to_string(), input.clone()));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .get_mut(action)
            .and_then(VecDeque::pop_front);
        match reply {
            Some(Reply::Ok(value)) => Ok(value),
            Some(Reply::Observation) => Err(DispatchError::CompletionObservationUnavailable {
                action: action.to_string(),
                message: "TLS handshake timeout".into(),
            }),
            Some(Reply::Fail) => Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("{action} failed"),
            }),
            Some(Reply::Diagnostic(message)) => Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message,
            }),
            Some(Reply::Permanent) => Err(DispatchError::CliInvocationPermanent(format!(
                "{action}: sandbox unavailable"
            ))),
            Some(Reply::Conflict(conflict)) => {
                Err(DispatchError::RecoverableVcsConflict(Box::new(conflict)))
            }
            Some(Reply::EvidenceHold) => {
                Err(DispatchError::ReviewEvidenceHold(Box::new(evidence_hold())))
            }
            Some(Reply::ChildCancelled) => Err(DispatchError::ChildRunCancelled {
                message: "child run jrun-child was cancelled".to_string(),
            }),
            None if action == "setup" => Ok(json!({
                "workspace_path": self.workspace_path,
                "base_ref": "main",
                "base_sha": "0123456789abcdef0123456789abcdef01234567",
            })),
            None => Ok(json!({ "action": action })),
        }
    }

    fn checkpoint_step(
        &self,
        _: &str,
        _: u32,
        step_id: &str,
        output: &Value,
        _: &std::collections::BTreeMap<String, Value>,
        _: &std::collections::BTreeMap<
            String,
            orbit_types::workflow::activity_job::ScriptOutputSet,
        >,
    ) -> Result<(), DispatchError> {
        self.checkpoints
            .lock()
            .unwrap()
            .push((step_id.to_string(), output.clone()));
        Ok(())
    }

    fn checkpoint_step_diagnostic(
        &self,
        _: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        self.diagnostics
            .lock()
            .unwrap()
            .push((step_id.to_string(), output.clone()));
        Ok(())
    }

    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(self.logs.get(run_id).cloned())
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        self.updates
            .lock()
            .unwrap()
            .push((task_id.to_string(), update));
        Ok(())
    }

    fn resolve_cli_executor(
        &self,
        _provider: &str,
    ) -> Result<orbit_engine::ResolvedCliExecutor, DispatchError> {
        self.cli.clone().ok_or(DispatchError::JobExecution(
            "this fixture has no provider CLI".into(),
        ))
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext {
            workspace_root: self.primary_root.clone(),
            ..Default::default()
        }
    }

    fn completion_observation_admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<CompletionObservationAdmission, OrbitError> {
        Ok(self.completion_admission.clone())
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        self.admissions.lock().unwrap().push(request.clone());
        Ok(self.admission.clone())
    }

    fn apply_final_recovery(
        &self,
        _run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        self.applications.lock().unwrap().push(application.clone());
        Ok(match &application.decision {
            FinalRecoveryDecision::Resume { .. } => FinalRecoveryApplied::Resume,
            FinalRecoveryDecision::Escalate { .. } => FinalRecoveryApplied::Escalated {
                outcome: "blocked for a human".to_string(),
            },
            other => FinalRecoveryApplied::Settled {
                outcome: other.kind().to_string(),
            },
        })
    }
}

pub(super) fn evidence_hold() -> orbit_types::workflow::ReviewEvidenceHold {
    orbit_types::workflow::ReviewEvidenceHold {
        schema_version: 1,
        attempt_id: "attempt-1".to_string(),
        lineage_key: "lineage-1".to_string(),
        run_id: RUN_ID.to_string(),
        candidate: orbit_types::workflow::automation::SourceRevision {
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            tree: "89abcdef0123456789abcdef0123456789abcdef".to_string(),
        },
        task_meaning_digest: "meaning".to_string(),
        requirements: Vec::new(),
        task_spec_digest: None,
        published_ref: None,
        candidate_carry: None,
    }
}

pub(super) struct Run {
    pub(super) result: Result<JobOutcome, DispatchError>,
    /// `(outcome, decision)` of every `job.final_recovery_attempted` event.
    pub(super) attempts: Vec<(String, Option<String>)>,
    /// Detail of each attempt, in the same order as `attempts`.
    pub(super) details: Vec<Option<String>>,
    /// `outcome` of every `step.post_recovery_attempt` event.
    pub(super) post_recovery: Vec<String>,
}

pub(super) fn run(job: &JobV2, host: &RecoveryHost, resume: Option<&PipelineState>) -> Run {
    run_with_evidence(job, host, resume, Vec::new())
}

pub(super) fn run_with_evidence(
    job: &JobV2,
    host: &RecoveryHost,
    resume: Option<&PipelineState>,
    prior_events: Vec<(&str, V2AuditEventKind)>,
) -> Run {
    run_with_input(
        job,
        host,
        resume,
        prior_events,
        json!({ "task_ids": ["T-1"] }),
    )
}

pub(super) fn run_with_input(
    job: &JobV2,
    host: &RecoveryHost,
    resume: Option<&PipelineState>,
    prior_events: Vec<(&str, V2AuditEventKind)>,
    input: Value,
) -> Run {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let inner = Arc::new(InMemorySink::new(audit_root.path().join("blobs")));
    let store = Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite sink"));
    let audit_writer = |run_id: &str| {
        let sink = Arc::new(V2SqliteSink::for_audit_root(
            store.clone(),
            "ws_final_recovery",
            run_id,
            "test-agent",
            None,
            audit_root.path(),
        ));
        Arc::new(V2AuditWriter::new(run_id, "test-agent", inner.clone()).with_envelope_sink(sink))
    };
    let mut own_events = Vec::new();
    for (run_id, event) in prior_events {
        if run_id == RUN_ID {
            own_events.push(event);
            continue;
        }
        audit_writer(run_id)
            .emit(event)
            .expect("persist prior event");
    }
    let writer = audit_writer(RUN_ID);
    for event in own_events {
        writer.emit(event).expect("persist own prior event");
    }
    let result = execute_job_with_resume(job, input, RUN_ID, writer.clone(), host, resume);
    let mut attempts = Vec::new();
    let mut details = Vec::new();
    let mut post_recovery = Vec::new();
    for event in writer.events_snapshot().expect("audit events") {
        match event.kind {
            V2AuditEventKind::FinalRecoveryAttempted {
                outcome,
                decision,
                detail,
                ..
            } => {
                attempts.push((outcome, decision));
                details.push(detail);
            }
            V2AuditEventKind::StepPostRecoveryAttempt { outcome, .. } => {
                post_recovery.push(outcome);
            }
            _ => {}
        }
    }
    Run {
        result,
        attempts,
        details,
        post_recovery,
    }
}
