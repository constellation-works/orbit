//! Keep a stopped run's candidate before its worktree is torn down.
//!
//! A worker stopped externally — its scope killed by the host manager, or
//! found dead by orphan reconciliation — leaves its candidate only as
//! uncommitted changes in a worktree a later sweep removes. Before the run
//! is finalized, the host that stopped it commits those changes detached
//! (or pins `HEAD` when clean) under `refs/orbit/rescue/<run>` and records
//! the ref and commit in the run's pipeline state, so `orbit run show`
//! names where the candidate survives after teardown.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_engine::review_gate::{RESCUE_CANDIDATE_KEY, preserve_rescue_candidate};
use orbit_engine::run_worktree_paths;
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Keep `run`'s candidate under its rescue ref, if it has a worktree.
    /// Best effort: a run is stopped whether or not its candidate could be
    /// kept. Returns a clause naming what was kept, for the run's
    /// diagnostic, or `None` when there was no worktree to keep.
    pub(crate) fn keep_rescue_candidate(&self, run: &JobRun) -> Option<String> {
        match self.try_keep_rescue_candidate(run) {
            Ok(clause) => clause,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.job_run",
                    run_id = %run.run_id,
                    %error,
                    "the stopped run's candidate could not be kept"
                );
                Some(format!("rescue candidate could not be kept: {error}"))
            }
        }
    }

    fn try_keep_rescue_candidate(&self, run: &JobRun) -> Result<Option<String>, OrbitError> {
        if let Some(record) = self.rescue_candidate_record(&run.run_id)
            && let Some(reference) = record["rescue_ref"].as_str()
        {
            let commit = record["rescue_commit"].as_str().unwrap_or_default();
            return Ok(Some(format!(
                "rescue candidate kept at `{reference}` (commit `{commit}`)"
            )));
        }
        let Some(workspace) = self.rescue_candidate_workspace(run) else {
            return Ok(None);
        };
        let mut record = json!({
            "run_id": run.run_id,
            "rescue_ref": null,
            "rescue_commit": null,
            "partial_commit": null,
        });
        let clause = match preserve_rescue_candidate(&workspace, &run.run_id) {
            Ok(kept) => {
                record["rescue_ref"] = json!(kept.rescue_ref);
                record["rescue_commit"] = json!(kept.commit);
                record["partial_commit"] = json!(kept.partial_commit);
                format!(
                    "rescue candidate kept at `{}` (commit `{}`)",
                    kept.rescue_ref, kept.commit
                )
            }
            Err(error) => {
                record["failure"] = json!(error.to_string());
                format!("rescue candidate could not be kept: {error}")
            }
        };
        // A run with no pipeline state yet keeps the clause only in its
        // diagnostic step; there is no run record to attach the ref to.
        if self.read_run_state(&run.run_id)?.is_some() {
            self.stores()
                .jobs()
                .update_run_state(&run.run_id, &mut |_, state| {
                    if let Some(pipeline) = state.pipeline.as_object_mut() {
                        pipeline.insert(RESCUE_CANDIDATE_KEY.to_string(), record.clone());
                    }
                    Ok(())
                })?;
        }
        Ok(Some(clause))
    }

    /// The worktree a stopped run's candidate lives in: the one its
    /// pipeline recorded, else the path its setup derived for the run.
    fn rescue_candidate_workspace(&self, run: &JobRun) -> Option<PathBuf> {
        if let Ok(Some(state)) = self.read_run_state(&run.run_id)
            && let Some(path) = state
                .pipeline
                .pointer("/worktree/workspace_path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
        {
            return Some(path);
        }
        run_worktree_paths(&self.paths().repo_root, run)
            .ok()?
            .into_iter()
            .find(|path| path.is_dir())
    }

    /// What an earlier stop kept for `run_id`, recorded in its pipeline
    /// state; `None` when nothing was recorded.
    pub(crate) fn rescue_candidate_record(&self, run_id: &str) -> Option<Value> {
        self.read_run_state(run_id)
            .ok()
            .flatten()
            .and_then(|state| state.pipeline.get(RESCUE_CANDIDATE_KEY).cloned())
            .filter(|record| record["run_id"].as_str() == Some(run_id))
    }
}
