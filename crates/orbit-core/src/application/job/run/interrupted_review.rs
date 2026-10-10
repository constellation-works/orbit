//! Keep the repair of a review a run was stopped inside [ORB-15314].
//!
//! Cancelling a run, or reconciling one whose worker died, while a review
//! gate's reviewer is working would leave the reviewer's uncommitted edits
//! in a worktree no later run reads. Before the run is finalized, and so
//! before a claimed leaf's settlement is computed, the host that stopped it
//! commits those edits as a marked partial-repair commit on the candidate
//! head, keeps it under a ref beside the candidate's, and records what it kept
//! in the run's pipeline state. A cancel also pushes that ref to `origin`;
//! reconciliation, which also runs when a workspace opens, keeps it local. The
//! settlement hands the record to the owner, which adds the attempt's
//! findings, and the task's next claim resumes from the repair. An owner
//! run's task gets a comment naming the ref.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::review_gate::{
    INTERRUPTED_REVIEW_KEY, carry_interrupted_review, preserve_interrupted_review, review_in_flight,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::TaskComment;
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};

use super::projection::job_run_task_ids;
use crate::OrbitRuntime;

/// Claimed leaves, whose task lives on the owner: their record reaches it
/// through the settlement, not a comment here.
const CLAIMED_PIPELINES: [&str; 2] = ["task_claimed_pr_pipeline", "task_claimed_local_pipeline"];

impl OrbitRuntime {
    /// Keep the uncommitted repair of the review `run` stopped inside, if
    /// any; `carry` also pushes it to `origin`. Best effort: a run is
    /// stopped whether or not its repair could be kept.
    pub(super) fn keep_interrupted_review(&self, run: &JobRun, carry: bool) {
        if let Err(error) = self.try_keep_interrupted_review(run, carry) {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id = %run.run_id,
                %error,
                "the stopped review's repair could not be kept"
            );
        }
    }

    fn try_keep_interrupted_review(&self, run: &JobRun, carry: bool) -> Result<(), OrbitError> {
        let Some(state) = self.read_run_state(&run.run_id)? else {
            return Ok(());
        };
        if state
            .pipeline
            .get(INTERRUPTED_REVIEW_KEY)
            .is_some_and(|kept| !kept.is_null())
        {
            return Ok(());
        }
        let Some(review) = review_in_flight(&state.pipeline) else {
            return Ok(());
        };
        let task_ids = job_run_task_ids(run);
        let [task_id] = task_ids.as_slice() else {
            return Ok(());
        };
        let Some(workspace) = state
            .pipeline
            .pointer("/worktree/workspace_path")
            .and_then(Value::as_str)
            .map(Path::new)
            .filter(|path| path.is_dir())
        else {
            return Ok(());
        };
        let mut record = json!({
            "step_id": review.step_id,
            "attempt_id": review.attempt_id,
            "repair_ref": null,
            "repair_commit": null,
            "carried": false,
        });
        match preserve_interrupted_review(workspace, task_id, &run.run_id, &review) {
            Ok(kept) => {
                if let Some(commit) = &kept.commit {
                    record["repair_ref"] = json!(kept.repair_ref);
                    record["repair_commit"] = json!(commit);
                    if carry {
                        match carry_interrupted_review(self, workspace, &kept) {
                            Ok(()) => record["carried"] = json!(true),
                            Err(why) => record["carry_failure"] = json!(why),
                        }
                    }
                }
            }
            Err(error) => record["failure"] = json!(error.to_string()),
        }
        self.stores()
            .jobs()
            .update_run_state(&run.run_id, &mut |_, state| {
                if let Some(pipeline) = state.pipeline.as_object_mut() {
                    pipeline.insert(INTERRUPTED_REVIEW_KEY.to_string(), record.clone());
                }
                Ok(())
            })?;
        if !CLAIMED_PIPELINES.contains(&run.job_id.as_str())
            && let Some(commit) = record["repair_commit"].as_str()
        {
            let at = if record["carried"] == json!(true) {
                "here and on origin"
            } else {
                "in this repository only"
            };
            RuntimeHost::apply_task_automation_update(
                self,
                task_id,
                TaskAutomationUpdate {
                    append_comments: vec![TaskComment {
                        at: Utc::now(),
                        by: "system".to_string(),
                        message: format!(
                            "Run {} was stopped during review step `{}` (attempt `{}`). The \
                             reviewer's uncommitted repair is kept as commit `{commit}` at `{}`, \
                             {at}; its parent is the reviewed candidate. The attempt's report \
                             stays on this task.",
                            run.run_id,
                            review.step_id,
                            review.attempt_id,
                            record["repair_ref"].as_str().unwrap_or_default(),
                        ),
                    }],
                    ..TaskAutomationUpdate::default()
                },
            )?;
        }
        Ok(())
    }
}
