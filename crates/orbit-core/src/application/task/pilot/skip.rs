//! An operator's skip of the task pilot: the `no-pilot` tag [ORB-21238].
//!
//! Filed on a task, `no-pilot` keeps automatic preparation from assessing it.
//! Admission also respects it: a task carrying the tag bypasses a stale
//! assessment and ordinary findings its latest receipt recorded. A current
//! `host_operational` hold still applies until a trusted operator decision or
//! newer assessment resolves the owner-side condition. Unverified review
//! findings, readiness gaps, context locks, dependencies and delivery holds
//! still apply.
//!
//! Adding the tag to a task a pilot has assessed, or one a pilot is preparing,
//! bypasses that gate, so it is an operator decision: any other writer is
//! refused, never stripped. The operator's write records a `pilot_skipped`
//! history entry naming the actor and the requirements digest it dispatches
//! without assessment. Filing a task with the tag stays open to every caller.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry};
use serde_json::{Value, json};

use super::super::TaskUpdateParams;
use super::super::update::TaskFieldWriter;
use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;

/// The tag an operator adds to dispatch a task without a new pilot.
const NO_PILOT_TAG: &str = "no-pilot";

/// History event an operator's pilot skip records.
const PILOT_SKIPPED_EVENT: &str = "pilot_skipped";

/// Whether `tags` carry the operator's pilot skip.
pub(super) fn skips_pilot(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == NO_PILOT_TAG)
}

/// The remedy text naming both ways past a pilot hold on `task`: a new
/// assessment, or the operator's skip.
pub(super) fn pilot_hold_remedies(task: &Task) -> String {
    let mut tags = task
        .tags
        .iter()
        .filter(|tag| tag.as_str() != NO_PILOT_TAG)
        .cloned()
        .collect::<Vec<_>>();
    tags.push(NO_PILOT_TAG.to_string());
    format!(
        "Run `orbit task repilot {id}` on the owner, or have an operator skip the pilot by \
         adding the `no-pilot` tag (`orbit task update {id} --tag {tags}`; `--tag` replaces \
         the list).",
        id = task.id,
        tags = tags.join(","),
    )
}

/// Whether `params` adds `no-pilot` to `task`.
fn adds_no_pilot(task: &Task, tags: Option<&[String]>) -> bool {
    tags.is_some_and(skips_pilot) && !skips_pilot(&task.tags)
}

/// What an operator's skip bypasses: the latest receipt and the requirements
/// it sealed, and the live runs preparing the task.
struct PilotSkipTarget {
    receipt: Option<String>,
    assessed_requirements: Value,
    pending_runs: Vec<String>,
}

impl OrbitRuntime {
    /// The pilot state `no-pilot` would bypass on `task`, or `None` when no
    /// pilot has assessed or is preparing it.
    fn pilot_skip_target(&self, task: &Task) -> Result<Option<PilotSkipTarget>, OrbitError> {
        let comments = self.get_task_pilot_comments(&task.id)?;
        let latest = super::pilot_receipts(&comments).next();
        let receipt = latest.map(|(_, receipt)| receipt.operation_id.to_string());
        // An undecodable receipt still counts as an assessment; it only has
        // no sealed digest to report.
        let assessed_requirements = latest
            .and_then(|(_, receipt)| serde_json::from_str::<Value>(receipt.body).ok())
            .and_then(|audit| audit.get("assessed_requirements").cloned())
            .unwrap_or(Value::Null);
        let pending_runs = self
            .pilot_preparation_holders()?
            .remove(&task.id)
            .map(|runs| runs.into_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(
            (receipt.is_some() || !pending_runs.is_empty()).then_some(PilotSkipTarget {
                receipt,
                assessed_requirements,
                pending_runs,
            }),
        )
    }

    /// Refuse a writer other than an operator that adds `no-pilot` (in the
    /// normalized `tags` it would store) to a task a pilot has assessed or is
    /// preparing.
    pub(in crate::application::task) fn refuse_unauthorized_pilot_skip(
        &self,
        task: &Task,
        tags: &[String],
        writer: TaskFieldWriter,
    ) -> Result<(), OrbitError> {
        if writer == TaskFieldWriter::Operator
            || !adds_no_pilot(task, Some(tags))
            || self.pilot_skip_target(task)?.is_none()
        {
            return Ok(());
        }
        Err(OrbitError::CapabilityDenied(format!(
            "only an operator can add `{NO_PILOT_TAG}` to task {}: a pilot has assessed or is \
             preparing it, and the tag dispatches it without a new assessment. Ask an operator \
             to skip the pilot, or request `orbit task repilot {}`",
            task.id, task.id
        )))
    }

    /// The `pilot_skipped` history entry for an operator's write that adds
    /// `no-pilot` to an assessed or preparing task: the actor, and the
    /// requirements digest this write leaves, which dispatch proceeds on
    /// without assessment.
    pub(in crate::application::task) fn pilot_skip_history(
        &self,
        task: &Task,
        params: &TaskUpdateParams,
        actor: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        if !adds_no_pilot(task, params.tags.as_deref()) {
            return Ok(None);
        }
        let Some(target) = self.pilot_skip_target(task)? else {
            return Ok(None);
        };
        let mut written = task.clone();
        if let Some(description) = &params.description {
            written.description.clone_from(description);
        }
        if let Some(criteria) = &params.acceptance_criteria {
            written.acceptance_criteria.clone_from(criteria);
        }
        let note = json!({
            "tag": NO_PILOT_TAG,
            "requirements_digest": super::freshness::pilot_requirements_material(&written),
            "pilot_operation_id": target.receipt,
            "assessed_requirements": target.assessed_requirements,
            "pending_pilot_runs": target.pending_runs,
        });
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: actor.to_string(),
            event: PILOT_SKIPPED_EVENT.to_string(),
            note: Some(note.to_string()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        }))
    }
}
