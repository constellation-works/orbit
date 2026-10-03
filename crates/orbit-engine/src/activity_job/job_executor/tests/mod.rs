#![allow(missing_docs)]

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex as StdMutex;

use orbit_agent::loop_engine::audit::{AuditSink, NullSink};
use orbit_types::workflow::JobScheduleState;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, BackoffStrategy, DeterministicSpec, JobKind, JobV2, JobV2Step, JobV2StepBody,
    RetrySpec, TargetStep,
};
use serde_json::{Value, json};

use super::*;

mod resume;
mod step;
mod validate;

fn test_writer(run_id: &str) -> V2AuditWriter {
    let inner: std::sync::Arc<dyn AuditSink> = std::sync::Arc::new(NullSink);
    V2AuditWriter::new(run_id, "test-agent", inner)
}

// --------------------------------------------------------------------------
// Shared scripted host for executor-block tests
// --------------------------------------------------------------------------

/// Outcome a `ScriptedHost` returns for a particular call.
#[derive(Clone)]
pub(super) enum Action {
    Ok(Value),
}

/// A minimal `RuntimeHost` returning scripted outcomes per
/// deterministic-action name.
pub(super) struct ScriptedHost {
    responses: StdMutex<HashMap<String, VecDeque<Action>>>,
    call_log: StdMutex<Vec<String>>,
}

impl ScriptedHost {
    pub(super) fn new<const N: usize>(actions: [(&str, Vec<Action>); N]) -> Self {
        Self {
            responses: StdMutex::new(
                actions
                    .into_iter()
                    .map(|(name, queue)| (name.to_string(), queue.into_iter().collect()))
                    .collect(),
            ),
            call_log: StdMutex::new(Vec::new()),
        }
    }

    pub(super) fn call_count(&self, action: &str) -> usize {
        self.call_log
            .lock()
            .expect("call log")
            .iter()
            .filter(|name| *name == action)
            .count()
    }
}

impl RuntimeHost for ScriptedHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.call_log
            .lock()
            .expect("call log")
            .push(action.to_string());
        let next = self
            .responses
            .lock()
            .expect("responses")
            .get_mut(action)
            .and_then(VecDeque::pop_front);
        match next {
            Some(Action::Ok(value)) => Ok(value),
            // Default: succeed with `{ "action": <name> }` so untyped tests
            // don't have to script every call.
            None => Ok(json!({ "action": action })),
        }
    }
}

// --------------------------------------------------------------------------
// Job/step builders
// --------------------------------------------------------------------------

pub(super) fn deterministic_target(action: &str) -> TargetStep {
    TargetStep {
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.to_string(),
            config: Value::Null,
        }),
        activity_name: None,
        input_schema_json: None,
        fs_profile: None,
        default_input: None,
        timeout_seconds: 0,
        session: None,
    }
}

pub(super) fn target_step(id: &str, action: &str) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Target(deterministic_target(action)),
    }
}

pub(super) fn job_with_steps(steps: Vec<JobV2Step>) -> JobV2 {
    JobV2 {
        state: JobScheduleState::Enabled,
        owns_task_worktree: false,
        task_delivery: None,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps,
    }
}
