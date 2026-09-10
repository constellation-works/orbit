mod ci;
mod dependabot;
mod input;
pub(crate) mod review;
mod shell;
mod task_update;
pub(crate) mod vcs;

use std::collections::HashMap;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::workflow::{DeterministicAction, EngineDeterministicAction};
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct StateExecutionContext {
    pub run_id: Option<String>,
    pub step_index: Option<u32>,
    pub state_dir: Option<PathBuf>,
    pub agent: Option<String>,
    pub model: Option<String>,
    /// Name of the dispatching activity's `fsProfile`. Actions that spawn a
    /// subprocess resolve the executor sandbox against it, so a shell step is
    /// confined by the same filesystem policy as the activity that ran it
    /// [ORB-11294]. `None` means the caller supplied no profile and the host's
    /// default applies.
    pub fs_profile: Option<String>,
}

pub fn execute_action<
    H: crate::context::RuntimeHost
        + crate::context::RuntimeHost
        + crate::context::RuntimeHost
        + Sync
        + ?Sized,
>(
    host: &H,
    action: &str,
    config: &Value,
    input: &Value,
    _debug: bool,
    _steps_outputs: &HashMap<String, Value>,
    state_context: Option<&StateExecutionContext>,
) -> Result<Value, OrbitError> {
    let Some(DeterministicAction::Engine(action)) = DeterministicAction::parse(action) else {
        return Err(OrbitError::InvalidInput(format!(
            "unsupported automation action '{action}'"
        )));
    };
    execute_engine_action(host, action, config, input, state_context)
}

pub(crate) fn execute_engine_action<
    H: crate::context::RuntimeHost
        + crate::context::RuntimeHost
        + crate::context::RuntimeHost
        + Sync
        + ?Sized,
>(
    host: &H,
    action: EngineDeterministicAction,
    config: &Value,
    input: &Value,
    state_context: Option<&StateExecutionContext>,
) -> Result<Value, OrbitError> {
    match action {
        // ---- retained internal actions ----
        EngineDeterministicAction::UpdateTask => {
            task_update::update_task(host, input, state_context)
        }
        EngineDeterministicAction::TaskComplete => task_update::task_complete(host, input),

        // ---- generic built-in actions ----
        EngineDeterministicAction::CollectCiEvidence => ci::collect_ci_evidence(host, input),
        EngineDeterministicAction::CollectDependabotAlerts => {
            dependabot::collect_dependabot_alerts(host, input)
        }
        EngineDeterministicAction::GitCommit => vcs::git_commit(host, input),
        EngineDeterministicAction::GitRebase => vcs::rebase_pr_branch(host, input),
        EngineDeterministicAction::LocalShell => {
            shell::local_shell(host, config, input, state_context)
        }
        EngineDeterministicAction::PrFailureHandoff => vcs::pr_failure_handoff(host, input),
        EngineDeterministicAction::GitPush => vcs::push_batch_changes(host, input),
        EngineDeterministicAction::GitMerge => vcs::git_merge(host, input),
        EngineDeterministicAction::WorktreeSetup => vcs::setup_worktree(host, input),
        EngineDeterministicAction::WorktreeGc => {
            let runs = host.list_job_runs_for_gc()?;
            let repo_root = host.repo_root()?;
            let older_than = input
                .get("older_than_hours")
                .and_then(Value::as_u64)
                .map(|hours| {
                    let hours = i64::try_from(hours).map_err(|_| {
                        OrbitError::InvalidInput("older_than_hours is too large".to_string())
                    })?;
                    chrono::Utc::now()
                        .checked_sub_signed(chrono::Duration::hours(hours))
                        .ok_or_else(|| {
                            OrbitError::InvalidInput("older_than_hours is too large".to_string())
                        })
                })
                .transpose()?;
            let result = vcs::collect_worktrees(
                std::path::Path::new(&repo_root),
                &runs,
                host,
                &vcs::WorktreeGcOptions {
                    delete: true,
                    // [ORB-11998] Read the activity's own `target_run_id`
                    // field, not the engine-injected `run_id` (every step's
                    // own dispatching run id — see `inject_run_id` in
                    // `activity_job::dispatcher`). Reading `run_id` here
                    // would scope every real job/routine dispatch to itself,
                    // a run with no worktree of its own, and silently reap
                    // nothing on every invocation.
                    run_id: input
                        .get("target_run_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    older_than,
                },
            )?;
            serde_json::to_value(result).map_err(|error| {
                OrbitError::Execution(format!("failed to serialize worktree GC result: {error}"))
            })
        }
        EngineDeterministicAction::PrOpen => vcs::pr_open(host, input),
        EngineDeterministicAction::PrPrepare => vcs::prepare_pr_handoff(host, input),
        EngineDeterministicAction::PrPromote => vcs::pr_promote(host, input),
        EngineDeterministicAction::PrComplete => vcs::pr_complete(host, input),
    }
}

#[cfg(test)]
mod tests;
