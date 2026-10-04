use chrono::{Duration, Utc};
use orbit_engine::{WorktreeGcOptions, WorktreeGcResult, WorktreeGcTaskLookup, collect_worktrees};
use orbit_store::contracts::{JobRunQuery, LocalPullPhase};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};

use crate::application::distributed::is_owner_transport_failure;
use crate::{OrbitError, OrbitRuntime};

impl OrbitRuntime {
    /// Every recorded run, without step rows. Worktree GC classifies live
    /// worktrees from non-terminal runs and never reads `agent_response_json`.
    pub(crate) fn list_job_runs_for_worktree_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.reconcile_stale_job_runs(None)?;
        self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            include_steps: false,
            ..JobRunQuery::default()
        })
    }

    /// Delivery jobs own their run worktree until the run is terminal. Reuse
    /// the collector here so delivery and the scheduled GC have identical
    /// task, run, registration, and clean-tree gates.
    pub(crate) fn cleanup_delivered_worktree(
        &self,
        run_id: &str,
    ) -> Result<Option<WorktreeGcResult>, OrbitError> {
        let run = self.show_job_run(run_id)?;
        if !self
            .load_v2_job_asset_by_name(&run.job_id)?
            .1
            .owns_task_worktree
        {
            return Ok(None);
        }

        collect_worktrees(
            &self.paths().repo_root,
            std::slice::from_ref(&run),
            self,
            &WorktreeGcOptions {
                delete: true,
                run_id: Some(run_id.to_string()),
                older_than: None,
                estimate_bytes: false,
                target_only: false,
            },
        )
        .map(Some)
    }

    /// A task's settlement state for worktree GC [ORB-13658].
    ///
    /// An owner checkout reads its own store. A replica holds no task records
    /// — they live on its owner — so it asks the owner over the owner's
    /// ordinary tool surface, through the route the run's own claim names
    /// (its admission's destination) or else this checkout's registered
    /// workspace on the owner. Only a transport failure is reported as the
    /// owner being unreachable [ORB-13920]: the owner not answering says
    /// nothing about whether the task exists, and a route this replica does
    /// not have is a configuration gap, not an outage.
    pub(crate) fn worktree_gc_task_lookup(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> WorktreeGcTaskLookup {
        let Some(owner_machine) = self.coordination_write_owner() else {
            return match self.get_task(task_id) {
                Ok(task) => WorktreeGcTaskLookup::Found {
                    status: task.status,
                    pr_status: task.pr_status,
                },
                Err(_) => WorktreeGcTaskLookup::Unresolved,
            };
        };
        let claim_selector = match self.stores().jobs().local_pull_for_run(run_id) {
            Ok(record) => record.map(|record| record.destination.selector),
            Err(error) => {
                tracing::warn!(
                    run_id,
                    %error,
                    "worktree GC could not read the run's claim admission; asking through the \
                     workspace route"
                );
                None
            }
        };
        let Some(selector) = claim_selector.or_else(|| {
            self.workspace_runtime_binding()
                .map(|binding| format!("{owner_machine}/{}", binding.logical_workspace_id))
        }) else {
            return WorktreeGcTaskLookup::NoOwnerRoute(
                "this replica checkout is not a registered workspace and the run holds no claim \
                 naming its owner; register it with `orbit workspace init --role replica`"
                    .into(),
            );
        };
        let Some(transport) = self.drain_owner_transport() else {
            return WorktreeGcTaskLookup::NoOwnerRoute(
                "this runtime has no federated owner route; add the owner to \
                 ~/.orbit/mcp-destinations.toml"
                    .into(),
            );
        };
        let answer = transport.show_task(
            &selector,
            json!({ "id": task_id, "fields": ["status", "pr_status"] }),
        );
        match answer {
            Ok(fields) => owner_task_fields(task_id, &fields),
            Err(OrbitError::NotFound { .. }) => WorktreeGcTaskLookup::Unresolved,
            Err(OrbitError::RemoteTool { code, .. }) if code == "not_found" => {
                WorktreeGcTaskLookup::Unresolved
            }
            Err(error) if is_owner_transport_failure(&error) => {
                WorktreeGcTaskLookup::OwnerUnreachable(format!("{selector}: {error}"))
            }
            Err(error) => WorktreeGcTaskLookup::NoOwnerRoute(format!("{selector}: {error}")),
        }
    }

    /// The settled claim behind a claimed leaf's worktree, if it has one
    /// [ORB-13920].
    ///
    /// The local admission record is the follower's durable account of the
    /// claim: once it is `Settled` the owner has accepted the leaf's outcome
    /// (or had already ended the claim), so the owner holds whatever the leaf
    /// delivered and nothing on this machine is still owed to it. Read
    /// without creating the pull schema, so a workspace that never pulled
    /// reads nothing.
    pub(crate) fn worktree_gc_settled_claim(&self, run_id: &str) -> Option<String> {
        let record = match self.stores().jobs().local_pull_for_run(run_id) {
            Ok(record) => record?,
            Err(error) => {
                tracing::warn!(run_id, %error, "worktree GC could not read the run's claim admission");
                return None;
            }
        };
        if record.phase != LocalPullPhase::Settled {
            return None;
        }
        let owner = &record.destination.selector;
        Some(match &record.refusal {
            Some(refusal) => format!("claim closed by its owner {owner}: {refusal}"),
            None => format!("claim settled with its owner {owner}"),
        })
    }

    /// Reclaim the `target/` build output of one terminal run's worktree,
    /// keeping its checkout. The collector's target-only gates apply: a
    /// terminal run, a registered worktree, no live or undecidable worker,
    /// and only Git-ignored content under `target/`.
    pub(crate) fn reclaim_run_build_output(
        &self,
        runs: &[JobRun],
        run_id: &str,
    ) -> Result<WorktreeGcResult, OrbitError> {
        collect_worktrees(
            &self.paths().repo_root,
            runs,
            self,
            &WorktreeGcOptions {
                delete: true,
                run_id: Some(run_id.to_string()),
                older_than: None,
                estimate_bytes: false,
                target_only: true,
            },
        )
    }

    pub fn gc_worktrees(
        &self,
        delete: bool,
        run_id: Option<String>,
        older_than_hours: Option<u64>,
        estimate_bytes: bool,
        target_only: bool,
    ) -> Result<WorktreeGcResult, OrbitError> {
        let older_than = older_than_hours
            .map(|hours| {
                let hours = i64::try_from(hours).map_err(|_| {
                    OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                })?;
                let duration = Duration::try_hours(hours).ok_or_else(|| {
                    OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                })?;
                Utc::now().checked_sub_signed(duration).ok_or_else(|| {
                    OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                })
            })
            .transpose()?;
        let runs = self.list_job_runs_for_worktree_gc()?;
        collect_worktrees(
            &self.paths().repo_root,
            &runs,
            self,
            &WorktreeGcOptions {
                delete,
                run_id,
                older_than,
                estimate_bytes,
                target_only,
            },
        )
    }
}

/// The owner's `status`/`pr_status` projection. An answer GC cannot read is
/// an answer, not an outage, so it retains the worktree as unresolved.
fn owner_task_fields(task_id: &str, fields: &Value) -> WorktreeGcTaskLookup {
    let status = fields
        .get("status")
        .cloned()
        .and_then(|status| serde_json::from_value::<TaskStatus>(status).ok());
    let Some(status) = status else {
        tracing::warn!(
            %task_id,
            answer = %fields,
            "the workspace owner's task answer carried no readable status; retaining the worktree"
        );
        return WorktreeGcTaskLookup::Unresolved;
    };
    WorktreeGcTaskLookup::Found {
        status,
        pr_status: fields
            .get("pr_status")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}
