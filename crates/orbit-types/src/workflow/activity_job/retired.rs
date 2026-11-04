//! Reject retired v2 asset declarations that would otherwise silently change
//! an activity or job’s execution semantics.

use super::activity_v2::RETIRED_BACKEND_MIGRATION;
use super::job_v2::{JobV2, JobV2Step, JobV2StepBody, LoopBlock};

/// Rejection of a retired declaration found while loading a job asset.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RetiredFeatureError {
    #[error(
        "asset_load: {asset_path} — step `{step_id}` declares `session: {session_name}`. \
         Cross-iteration sessions were provided only by the retired HTTP agent loop: \
         {RETIRED_BACKEND_MIGRATION}. Fix: remove the `session:` binding and accept \
         cold-start cost per iteration."
    )]
    SessionBinding {
        asset_path: String,
        step_id: String,
        session_name: String,
    },
}

/// Reject every retired `session:` binding in a job, at any nesting depth.
///
/// Runs at load time, before the DAG executor starts, so a run never begins
/// work it cannot finish as written.
pub fn validate_job_retired_sessions(
    job: &JobV2,
    asset_path: &str,
) -> Result<(), RetiredFeatureError> {
    for step in &job.steps {
        validate_step(step, asset_path)?;
    }
    Ok(())
}

fn validate_step(step: &JobV2Step, asset_path: &str) -> Result<(), RetiredFeatureError> {
    match &step.body {
        JobV2StepBody::Target(target) => reject_session(&target.session, &step.id, asset_path),
        JobV2StepBody::TargetRef(target_ref) => {
            // A ref carries its own `session:` through resolution, so check it
            // here too: an unresolved ref is structural breakage the
            // dispatcher's `UnresolvedTargetRef` surfaces separately.
            reject_session(&target_ref.session, &step.id, asset_path)
        }
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                validate_step(branch, asset_path)?;
            }
            Ok(())
        }
        JobV2StepBody::FanOut { fan_out, .. } => validate_step(&fan_out.worker, asset_path),
        JobV2StepBody::Loop { loop_ } => validate_loop_block(loop_, asset_path),
    }
}

fn validate_loop_block(block: &LoopBlock, asset_path: &str) -> Result<(), RetiredFeatureError> {
    for step in &block.steps {
        validate_step(step, asset_path)?;
    }
    Ok(())
}

fn reject_session(
    session: &Option<String>,
    step_id: &str,
    asset_path: &str,
) -> Result<(), RetiredFeatureError> {
    match session {
        Some(session_name) => Err(RetiredFeatureError::SessionBinding {
            asset_path: asset_path.to_string(),
            step_id: step_id.to_string(),
            session_name: session_name.clone(),
        }),
        None => Ok(()),
    }
}
