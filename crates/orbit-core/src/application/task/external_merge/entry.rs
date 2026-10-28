use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::application::task::TaskWorkspaceAutoPort;

use super::model::{
    ExternalMergeBatchSettlement, ExternalMergeSettlement, ExternalMergeSkippedTask,
    SETTLEABLE_STATUSES,
};
use super::{candidate, refusal};

/// The reason a per-task refusal names, without its `task '<id>' does not
/// settle …:` prefix; any other error keeps its full message.
fn external_merge_skip_reason(error: &OrbitError, id: &str) -> String {
    let OrbitError::InvalidInput(message) = error else {
        return error.to_string();
    };
    let prefix = format!("task '{id}' does not settle from a merged delivery pull request: ");
    message.strip_prefix(&prefix).unwrap_or(message).to_string()
}

impl OrbitRuntime {
    /// Settle every task whose delivery PR is among the forge's most recently
    /// merged ones (the owner pass; see the module docs). One task that cannot
    /// be written is logged and skipped; the others still settle.
    pub fn settle_externally_merged_tasks(
        &self,
    ) -> Result<Vec<ExternalMergeSettlement>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let carried = self.live_leaf_task_ids()?;
        let mut candidates = Vec::new();
        for status in SETTLEABLE_STATUSES {
            for task in self.stores().tasks().list_tasks_filtered(
                Some(status),
                None,
                None,
                None,
                None,
                None,
            )? {
                if let Ok(candidate) = self.settlement_candidate(task, &carried) {
                    candidates.push(candidate);
                }
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let merged = forge
            .merged_pull_requests(&repo_root)?
            .into_iter()
            .filter(|merged| {
                candidates.iter().any(|candidate| {
                    candidate::names_delivery(
                        &self
                            .context
                            .settings()
                            .pr_settings()
                            .conventions
                            .branch_name,
                        merged,
                        &candidate.task.id,
                    )
                })
            })
            .collect::<Vec<_>>();
        // The delivery identity costs a forge call; only a merge that names a
        // candidate needs it.
        if merged.is_empty() {
            return Ok(Vec::new());
        }
        let authors = self.settlement_authors(&repo_root);
        let mut settled = Vec::new();
        for candidate in candidates {
            let Some(landing) = self.delivery_merge(&candidate, &merged, &authors) else {
                continue;
            };
            match self.write_settlement(&candidate, &landing) {
                Ok(settlement) => settled.push(settlement),
                Err(error) => tracing::warn!(
                    target: "orbit.core.external_merge",
                    task_id = %candidate.task.id,
                    pr = landing.pull.number,
                    error = %error,
                    "could not settle a task from its merged delivery pull request"
                ),
            }
        }
        Ok(settled)
    }

    /// Settle every `blocked`, `in-progress` and `review` task from its own
    /// recorded PRs, as [`Self::settle_externally_merged_task_as`] does per
    /// task, so a merge older than the recent-merges listing also settles.
    /// One task that cannot settle is reported in `skipped` and left
    /// unchanged; the rest still settle.
    pub fn settle_externally_merged_tasks_from_recorded_prs_as(
        &self,
        actor: Option<&str>,
    ) -> Result<ExternalMergeBatchSettlement, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut ids = Vec::new();
        for status in SETTLEABLE_STATUSES {
            for task in self.stores().tasks().list_tasks_filtered(
                Some(status),
                None,
                None,
                None,
                None,
                None,
            )? {
                ids.push(task.id);
            }
        }
        let mut batch = ExternalMergeBatchSettlement::default();
        for id in ids {
            match self.settle_externally_merged_task_as(&id, actor) {
                Ok(settlement) => batch.settled.push(settlement),
                Err(error) => batch.skipped.push(ExternalMergeSkippedTask {
                    reason: external_merge_skip_reason(&error, &id),
                    task_id: id,
                }),
            }
        }
        Ok(batch)
    }

    /// Settle task `id` from its merged delivery PR, reading each PR Orbit
    /// recorded for it (the operator command). Refuses, naming why, a task
    /// that cannot settle.
    pub fn settle_externally_merged_task(
        &self,
        id: &str,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        self.settle_externally_merged_task_as(id, None)
    }

    /// Like [`Self::settle_externally_merged_task`], recording `actor` in task
    /// history instead of the system label (the dashboard passes its own label).
    pub fn settle_externally_merged_task_as(
        &self,
        id: &str,
        actor: Option<&str>,
    ) -> Result<ExternalMergeSettlement, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let carried = self.live_leaf_task_ids()?;
        let task = self.get_task(id)?;
        let candidate = self
            .settlement_candidate(task, &carried)
            .map_err(|reason| refusal::refused(id, &reason))?;

        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let mut fetched = Vec::new();
        for number in candidate.pr_numbers() {
            fetched.extend(forge.merged_pull_request(&repo_root, number)?);
        }
        let merged = fetched
            .iter()
            .filter(|merged| {
                candidate::names_delivery(
                    &self
                        .context
                        .settings()
                        .pr_settings()
                        .conventions
                        .branch_name,
                    merged,
                    id,
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        // A revoked handoff's PR proves itself by the handoff, not its author.
        let authors = if merged.is_empty() || candidate.revoked.is_some() {
            Vec::new()
        } else {
            self.settlement_authors(&repo_root)
        };
        let Some(landing) = self.delivery_merge(&candidate, &merged, &authors) else {
            let reason = match &candidate.revoked {
                Some(revoked) => candidate::revoked_handoff_unmerged(
                    &self
                        .context
                        .settings()
                        .pr_settings()
                        .conventions
                        .branch_name,
                    revoked,
                    fetched.first(),
                    id,
                ),
                None => format!(
                    "none of its delivery pull requests is merged into its target branch {}",
                    self.delivery_target_branch(&candidate.task)
                ),
            };
            return Err(refusal::refused(id, &reason));
        };
        self.write_settlement_as(&candidate, &landing, actor)
    }
}
