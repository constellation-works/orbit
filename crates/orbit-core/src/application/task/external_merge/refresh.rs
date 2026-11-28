//! Bring a held or review task's one open Orbit delivery pull request up to
//! the tip of its base branch as one conflict-free merge commit, starting its
//! required checks.
//!
//! A refresh never rebases, rewrites or force-pushes, makes no code edits, and
//! lands nothing. Any conflict is refused with the conflicting paths and
//! changes nothing; a head that already contains the base tip is a no-op.

use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_store::contract::AuditEventFilter;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use serde::Serialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskWorkspaceAutoPort;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};
use crate::runtime::task_pr_forge::PullRequestRefreshOutcome;
use orbit_types::workflow::DeliveryRefreshRequest;

use super::land::{OpenDelivery, TASK_LANDING_REFUSED, ensure_delivery_target_branch};

/// The stable code a refused refresh's error starts with.
pub const TASK_REFRESH_REFUSED: &str = "task_refresh_refused";

/// The audit command every Orbit-initiated pull request refresh records.
const PR_REFRESH_AUDIT_COMMAND: &str = "pr_refresh";

/// The audit step a refresh requested through `orbit.workflow.refresh` names.
const REFRESH_TOOL_STEP: &str = "workflow.refresh";

/// The audit step the delivery pipeline's base-inherited refresh names.
const PIPELINE_REFRESH_STEP: &str = "pipeline.base_inherited_refresh";

/// Outcome of refreshing a task's open delivery pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPullRequestRefreshOutcome {
    /// Merged the base branch tip into the pull request head as one merge
    /// commit and started required checks on the new head.
    Updated,
    /// The pull request head already contains the base branch tip; nothing changed.
    AlreadyUpToDate,
}

/// Result of refreshing a delivery pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPullRequestRefresh {
    /// The refreshed task.
    pub task_id: String,
    /// The refreshed pull request.
    pub pull_request: u64,
    /// The base branch merged into the pull request head.
    pub base_branch: String,
    /// The pull request head commit before the call.
    pub previous_head: String,
    /// The pull request head commit after the call.
    pub head_sha: String,
    /// Whether the head moved or was already up to date.
    pub outcome: TaskPullRequestRefreshOutcome,
    /// Whether required checks were started on a new head commit.
    pub checks_started: bool,
}

impl OrbitRuntime {
    /// Successful refresh edges from the observed head back to the published
    /// head, in newest-first order. Completion independently checks each edge
    /// against Git before using it as delivery evidence.
    pub(crate) fn audited_delivery_refreshes(
        &self,
        task_id: &str,
        pr_number: &str,
        head_branch: &str,
        base_branch: &str,
        published: &str,
        reported: &str,
    ) -> Result<Vec<String>, OrbitError> {
        let mut filter = AuditEventFilter {
            target_type: Some("pull_request".into()),
            target_id: Some(pr_number.into()),
            workspace_id: Some(self.workspace_id()?),
            role: Some("system".into()),
            status: Some(AuditEventStatus::Success),
            limit: 100,
            ..Default::default()
        };
        let mut current = reported.to_string();
        let mut heads = Vec::new();
        let mut parents = HashMap::new();
        loop {
            let rows = crate::app::audit::list_audit_events_filtered(self, &filter)?;
            for row in &rows {
                if row.command != PR_REFRESH_AUDIT_COMMAND
                    || row.subcommand.as_deref() != Some("updated")
                    || row.task_id.as_deref() != Some(task_id)
                {
                    continue;
                }
                let Some(detail) = row
                    .arguments_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Value>(json).ok())
                else {
                    continue;
                };
                if detail["head_branch"].as_str() != Some(head_branch)
                    || detail["base_branch"].as_str() != Some(base_branch)
                {
                    continue;
                }
                if let (Some(previous), Some(next)) =
                    (detail["head_sha"].as_str(), detail["new_head_sha"].as_str())
                    && previous != next
                {
                    parents
                        .entry(next.to_string())
                        .or_insert_with(|| previous.to_string());
                }
                // The forge writes and their audit rows can finish in different
                // orders. Follow the exact SHA edges, not insertion order.
                while let Some(previous) = parents.remove(&current) {
                    heads.push(std::mem::replace(&mut current, previous));
                    if current == published {
                        return Ok(heads);
                    }
                }
            }
            if rows.len() < filter.limit {
                return Ok(Vec::new());
            }
            filter.before_id = rows.last().map(|row| row.id);
        }
    }

    /// Bring the held (`blocked`) or `review` task `id`'s one open Orbit
    /// delivery pull request up to the tip of its base branch as one
    /// conflict-free merge commit, starting its required checks.
    ///
    /// A refusal (status, live run ownership, non-Orbit PR, conflict with
    /// conflicting paths, or forge refusal) is an [`OrbitError::PolicyDenied`]
    /// starting with `TASK_REFRESH_REFUSED` and leaves the branch unchanged.
    pub fn refresh_task_pull_request(
        &self,
        id: &str,
        actor: Option<&str>,
    ) -> Result<TaskPullRequestRefresh, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let task = self.get_task(id)?;
        if !matches!(task.status, TaskStatus::Review | TaskStatus::Blocked) {
            return Err(refresh_refused(
                id,
                format_args!(
                    "it is '{}'; only a held (blocked) or review delivery task can be refreshed",
                    task.status
                ),
            ));
        }
        let carried = crate::app::port::TaskWorkspaceAuto(self).live_leaf_task_ids()?;
        let candidate = self
            .refresh_candidate(task, &carried)
            .map_err(|reason| refresh_refused(id, reason))?;

        let (number, expected_base) = match self
            .open_delivery_pull_request(&candidate)
            .map_err(|error| remap_landing_refusal(id, error))?
        {
            OpenDelivery::One {
                number,
                target_branch,
            } => (number, target_branch),
            OpenDelivery::Several(numbers) => {
                return Err(refresh_refused(
                    id,
                    format_args!("it has several open delivery pull requests: {numbers:?}"),
                ));
            }
            OpenDelivery::None => {
                return Err(refresh_refused(
                    id,
                    "it has no open Orbit delivery pull request to refresh",
                ));
            }
        };

        let repo_root = self.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let Some(blockers) = forge.pull_request_required_checks(&repo_root, number)? else {
            return Err(refresh_refused(
                id,
                format_args!("pull request #{number} is not open"),
            ));
        };
        let observed_base = forge
            .lease_pull_request(&repo_root, &format!("#{number}"))?
            .map(|bound| bound.base_branch);
        if let Err(reason) =
            ensure_delivery_target_branch(number, observed_base.as_deref(), &expected_base)
        {
            return Err(refresh_refused(id, reason));
        }

        let previous_head = blockers.head_sha.clone();
        let head_branch = blockers.pull.head_branch.clone();
        let outcome = self.refresh_open_delivery_head(
            id,
            OpenDeliveryHead {
                number,
                head_branch: &head_branch,
                base_branch: &expected_base,
                previous_head: &previous_head,
            },
            actor,
            REFRESH_TOOL_STEP,
        );

        match outcome? {
            PullRequestRefreshOutcome::Updated { head_sha } => Ok(TaskPullRequestRefresh {
                task_id: id.to_string(),
                pull_request: number,
                base_branch: expected_base,
                previous_head,
                head_sha,
                outcome: TaskPullRequestRefreshOutcome::Updated,
                checks_started: true,
            }),
            PullRequestRefreshOutcome::AlreadyUpToDate { head_sha } => Ok(TaskPullRequestRefresh {
                task_id: id.to_string(),
                pull_request: number,
                base_branch: expected_base,
                previous_head,
                head_sha,
                outcome: TaskPullRequestRefreshOutcome::AlreadyUpToDate,
                checks_started: false,
            }),
            PullRequestRefreshOutcome::Conflicting {
                conflicting_paths,
                reason,
            } => {
                let paths = if conflicting_paths.is_empty() {
                    "(paths unavailable)".to_string()
                } else {
                    conflicting_paths.join(", ")
                };
                Err(refresh_refused(
                    id,
                    format_args!(
                        "pull request #{number} conflicts with {expected_base} ({reason}); conflicting paths: {paths}"
                    ),
                ))
            }
            PullRequestRefreshOutcome::Refused { reason } => Err(refresh_refused(
                id,
                format_args!("the forge refused to refresh pull request #{number}: {reason}"),
            )),
        }
    }

    /// [ORB-26860] Refresh the open delivery pull request a live delivery run
    /// holds for its own tasks, after its check failure proved base-inherited.
    ///
    /// The pipeline's authority gate: only on the owning host, for a
    /// non-terminal task, and only while the pull request is open with the
    /// exact head, head branch and base branch the run pinned. Every refusal
    /// is a [`PullRequestRefreshOutcome::Refused`] and changes nothing; the
    /// write itself is [`Self::refresh_open_delivery_head`], the operation
    /// behind `orbit.workflow` action `refresh`.
    pub(crate) fn refresh_run_delivery_pull_request(
        &self,
        request: &DeliveryRefreshRequest,
    ) -> Result<PullRequestRefreshOutcome, OrbitError> {
        let refused = |reason: String| Ok(PullRequestRefreshOutcome::Refused { reason });
        if self.role().is_worker() {
            return refused(
                "a claimed worker cannot refresh a delivery pull request; the owning host does"
                    .to_string(),
            );
        }
        let Some(primary) = request.task_ids.iter().min() else {
            return refused("the request names no task".to_string());
        };
        let task = self.get_task(primary)?;
        if task.status.is_terminal() {
            return refused(format!("task '{primary}' is '{}'", task.status));
        }
        let number = request.pr_number;
        let repo_root = self.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let Some(blockers) = forge.pull_request_required_checks(&repo_root, number)? else {
            return refused(format!("pull request #{number} is not open"));
        };
        if blockers.head_sha != request.expected_head_sha {
            return refused(format!(
                "pull request #{number} head {} is not the run's head {}",
                blockers.head_sha, request.expected_head_sha
            ));
        }
        if blockers.pull.head_branch != request.head_branch {
            return refused(format!(
                "pull request #{number} head branch {} is not the run's branch {}",
                blockers.pull.head_branch, request.head_branch
            ));
        }
        let observed_base = forge
            .lease_pull_request(&repo_root, &format!("#{number}"))?
            .map(|bound| bound.base_branch);
        if let Err(reason) =
            ensure_delivery_target_branch(number, observed_base.as_deref(), &request.base_branch)
        {
            return refused(reason);
        }
        self.refresh_open_delivery_head(
            primary,
            OpenDeliveryHead {
                number,
                head_branch: &request.head_branch,
                base_branch: &request.base_branch,
                previous_head: &request.expected_head_sha,
            },
            Some(&request.run_id),
            PIPELINE_REFRESH_STEP,
        )
    }

    /// The one refresh operation behind `orbit.workflow` action `refresh`
    /// and the delivery pipeline's base-inherited refresh [ORB-26860]: merge
    /// the tip of `base_branch` into pull request `number`'s head
    /// (`previous_head` on `head_branch`) as one conflict-free merge commit,
    /// recording `pr_refresh` audit rows before and after. Callers own the
    /// authority gate.
    pub(crate) fn refresh_open_delivery_head(
        &self,
        id: &str,
        head: OpenDeliveryHead<'_>,
        actor: Option<&str>,
        step: &str,
    ) -> Result<PullRequestRefreshOutcome, OrbitError> {
        let OpenDeliveryHead {
            number,
            head_branch,
            base_branch,
            previous_head,
        } = head;
        let repo_root = self.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let pull_request = number.to_string();
        let request_detail = json!({
            "step": step,
            "task_id": id,
            "actor": actor,
            "pull_request": number,
            "head_branch": head_branch,
            "base_branch": base_branch,
            "head_sha": previous_head,
        });
        record_refresh_audit(
            self,
            id,
            &pull_request,
            "requested",
            AuditEventStatus::Success,
            request_detail.clone(),
        )?;

        let outcome = forge.update_pull_request_branch(
            &repo_root,
            number,
            head_branch,
            base_branch,
            previous_head,
        );
        let (phase, audit_status, extra) = match &outcome {
            Ok(PullRequestRefreshOutcome::Updated { head_sha }) => (
                "updated",
                AuditEventStatus::Success,
                json!({"new_head_sha": head_sha}),
            ),
            Ok(PullRequestRefreshOutcome::AlreadyUpToDate { head_sha }) => (
                "already_up_to_date",
                AuditEventStatus::Success,
                json!({"new_head_sha": head_sha}),
            ),
            Ok(PullRequestRefreshOutcome::Conflicting {
                conflicting_paths,
                reason,
            }) => (
                "refused",
                AuditEventStatus::Denied,
                json!({"reason": reason, "conflicting_paths": conflicting_paths}),
            ),
            Ok(PullRequestRefreshOutcome::Refused { reason }) => (
                "refused",
                AuditEventStatus::Denied,
                json!({"reason": reason}),
            ),
            Err(error) => (
                "failed",
                AuditEventStatus::Failure,
                json!({"error": error.to_string()}),
            ),
        };
        record_refresh_outcome(
            self,
            id,
            &pull_request,
            phase,
            audit_status,
            merged_fields(request_detail, extra),
        );
        outcome
    }
}

/// An open pull request's exact identity: its number, head and base
/// branches, and the head commit the refresh expects to merge onto.
#[derive(Clone, Copy)]
pub(crate) struct OpenDeliveryHead<'a> {
    pub(crate) number: u64,
    pub(crate) head_branch: &'a str,
    pub(crate) base_branch: &'a str,
    pub(crate) previous_head: &'a str,
}

fn refresh_refused(id: &str, reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "{TASK_REFRESH_REFUSED}: task '{id}' did not refresh: {reason}"
    ))
}

fn remap_landing_refusal(id: &str, error: OrbitError) -> OrbitError {
    match error {
        OrbitError::PolicyDenied(message) => {
            let prefix = format!("{TASK_LANDING_REFUSED}: task '{id}' did not land: ");
            if let Some(reason) = message.strip_prefix(&prefix) {
                refresh_refused(id, reason)
            } else {
                OrbitError::PolicyDenied(message)
            }
        }
        other => other,
    }
}

fn merged_fields(mut request: Value, detail: Value) -> Value {
    if let (Value::Object(request), Value::Object(detail)) = (&mut request, detail) {
        request.extend(detail);
    }
    request
}

fn record_refresh_outcome(
    runtime: &OrbitRuntime,
    task_id: &str,
    pull_request: &str,
    phase: &str,
    status: AuditEventStatus,
    detail: Value,
) {
    if let Err(error) = record_refresh_audit(runtime, task_id, pull_request, phase, status, detail)
    {
        tracing::warn!(
            target: "orbit.core.external_merge",
            pull_request,
            phase,
            %error,
            "could not record the pull request refresh outcome audit row"
        );
    }
}

fn record_refresh_audit(
    runtime: &OrbitRuntime,
    task_id: &str,
    pull_request: &str,
    phase: &str,
    status: AuditEventStatus,
    detail: Value,
) -> Result<(), OrbitError> {
    let workspace_id = runtime.workspace_id().ok();
    record_system_audit_event(
        runtime,
        SystemAuditEvent {
            execution_id_prefix: "audit-pr-refresh",
            command: PR_REFRESH_AUDIT_COMMAND,
            subcommand: Some(phase),
            tool_name: None,
            target_type: "pull_request",
            target_id: Some(pull_request).filter(|number| !number.is_empty()),
            role: "system",
            status,
            arguments_json: detail.to_string(),
            error_message: None,
            workspace_id: workspace_id.as_deref(),
            task_id: Some(task_id),
            attribution: AuditRunAttribution::Environment { run_id: None },
        },
    )
}
