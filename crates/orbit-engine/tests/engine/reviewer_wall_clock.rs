//! [ORB-15094] The reviewer process runs under the deadline the host answers
//! a start with, not the activity's own `wall_clock_timeout_seconds`: a
//! `review.minutes` above the activity default is no longer cut off at it.
//! Other activities, and hosts that do not bound reviews, keep the wall clock
//! the activity declares.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::{
    DispatchError, ResolvedCliExecutor, ReviewerInvocationRequest, RuntimeHost,
    execute_job_with_resume,
};
use orbit_types::workflow::JobScheduleState;
use orbit_types::workflow::ReviewerInvocationEvent;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, JobKind, JobV2, JobV2Step, JobV2StepBody, TargetStep, V2AuditEventKind,
};
use serde_json::{Value, json};

use super::v2_cli_agent::{build_writer, cli_agent_loop_spec, events_snapshot, fake_cli};

/// The wall clock the activity declares, standing in for the shipped reviewer's.
const ACTIVITY_WALL_CLOCK_SECONDS: u64 = 3600;

/// Runs the substituted `claude` CLI and answers a reviewer start with `bound`.
struct BoundingHost {
    command: PathBuf,
    bound: Option<u64>,
    invocations: Mutex<Vec<ReviewerInvocationEvent>>,
}

impl BoundingHost {
    fn new(command: &Path, bound: Option<u64>) -> Self {
        Self {
            command: command.to_path_buf(),
            bound,
            invocations: Mutex::default(),
        }
    }
}

impl RuntimeHost for BoundingHost {
    fn run_deterministic(
        &self,
        _action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Err(DispatchError::DeterministicActionNotRegistered(
            "unused".to_string(),
        ))
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.to_string_lossy().into_owned(),
            args: Vec::new(),
        })
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext::default()
    }

    fn record_reviewer_invocation(
        &self,
        request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        self.invocations.lock().unwrap().push(request.event);
        Ok(match request.event {
            ReviewerInvocationEvent::Started => self.bound,
            _ => None,
        })
    }
}

/// One step running `activity` as an agent loop with the declared wall clock.
fn single_step_job(activity: &str) -> JobV2 {
    let mut spec = cli_agent_loop_spec(None);
    spec.wall_clock_timeout_seconds = ACTIVITY_WALL_CLOCK_SECONDS;
    JobV2 {
        state: JobScheduleState::Enabled,
        owns_task_worktree: false,
        task_delivery: None,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        final_recovery_activity: None,
        resolved_final_recovery_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![JobV2Step {
            id: "review".to_string(),
            when: None,
            retry: None,
            recovery_activity: None,
            resolved_recovery_activity: None,
            body: JobV2StepBody::Target(TargetStep {
                spec: ActivityV2Spec::AgentLoop(spec),
                activity_name: Some(activity.to_string()),
                input_schema_json: None,
                fs_profile: None,
                default_input: None,
                timeout_seconds: 0,
                session: None,
            }),
        }],
    }
}

/// Run the step once and return the wall clock, in seconds, its CLI process
/// was started with, plus the invocation events the host saw.
fn run(activity: &str, bound: Option<u64>) -> (u64, Vec<ReviewerInvocationEvent>) {
    let run_id = "reviewer-wall-clock";
    let audit = tempfile::tempdir().unwrap();
    let (writer, store) = build_writer(audit.path(), run_id).unwrap();
    let fake = fake_cli(
        "claude",
        "#!/bin/sh\ncat > /dev/null\necho '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    )
    .unwrap();
    let host = BoundingHost::new(fake.cli_path(), bound);
    let outcome = execute_job_with_resume(
        &single_step_job(activity),
        json!({ "prompt": "review", "lineage_key": "lineage-1", "attempt_id": "rvw-1" }),
        run_id,
        writer,
        &host,
        None,
    )
    .unwrap();
    assert!(outcome.success, "{outcome:?}");
    let wall_clock_ms = events_snapshot(&store, run_id)
        .unwrap()
        .into_iter()
        .find_map(|event| match event.kind {
            V2AuditEventKind::CliInvocationStarted {
                wall_clock_timeout_ms,
                ..
            } => Some(wall_clock_timeout_ms),
            _ => None,
        })
        .expect("cli.invocation.started event");
    let invocations = host.invocations.lock().unwrap().clone();
    (wall_clock_ms / 1000, invocations)
}

#[test]
fn a_reviewer_runs_under_the_hosts_deadline_above_or_below_the_activity_wall_clock() {
    for bound in [
        ACTIVITY_WALL_CLOCK_SECONDS * 2,
        ACTIVITY_WALL_CLOCK_SECONDS / 2,
    ] {
        let (wall_clock, invocations) = run("agent_review_repair", Some(bound));
        assert_eq!(
            wall_clock, bound,
            "the reviewer process must get the host's deadline, not the activity's \
             {ACTIVITY_WALL_CLOCK_SECONDS} s (jrun-20261009-1559-c1 timed out at 3600 s under a \
             7200 s review budget)"
        );
        assert!(matches!(
            invocations.as_slice(),
            [
                ReviewerInvocationEvent::Started,
                ReviewerInvocationEvent::Finished { .. }
            ]
        ));
    }
}

#[test]
fn a_reviewer_keeps_the_activity_wall_clock_when_the_host_does_not_bound_reviews() {
    let (wall_clock, _) = run("agent_review_repair", None);
    assert_eq!(wall_clock, ACTIVITY_WALL_CLOCK_SECONDS);
}

#[test]
fn other_activities_keep_their_own_wall_clock() {
    let (wall_clock, invocations) = run("agent_implement", Some(ACTIVITY_WALL_CLOCK_SECONDS * 2));
    assert_eq!(wall_clock, ACTIVITY_WALL_CLOCK_SECONDS);
    assert!(
        invocations.is_empty(),
        "only the reviewer activity reports invocations"
    );
}
