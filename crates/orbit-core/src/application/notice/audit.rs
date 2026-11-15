//! Notice write auditing and persisted row conversions.

use orbit_common::OrbitError;
use orbit_store::contracts::NoticeRow;
use orbit_types::record::{AuthorClass, Notice};
use orbit_types::telemetry::AuditEventStatus;

use crate::OrbitRuntime;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};

/// The audit row a notice write or refusal leaves.
pub(crate) struct NoticeAudit {
    verb: &'static str,
    pub(super) target: Option<String>,
    pub(super) poster: Option<AuthorClass>,
    pub(super) detail: serde_json::Value,
}

impl NoticeAudit {
    pub(super) fn new(verb: &'static str, target: Option<String>) -> Self {
        Self {
            verb,
            target,
            poster: None,
            detail: serde_json::json!({}),
        }
    }

    /// Record the outcome. An audit failure is logged, never raised: it
    /// must not turn a committed write into a reported failure.
    pub(super) fn record(mut self, runtime: &OrbitRuntime, result: &Result<String, &OrbitError>) {
        let status = match result {
            Ok(id) => {
                self.target = Some(id.clone());
                AuditEventStatus::Success
            }
            Err(error) => {
                if let Some(object) = self.detail.as_object_mut() {
                    object.insert("error".into(), serde_json::json!(error.to_string()));
                }
                if error.is_policy_denial()
                    || matches!(
                        error,
                        OrbitError::NoticeRefused { .. } | OrbitError::CapabilityRefused(_)
                    )
                {
                    AuditEventStatus::Denied
                } else {
                    AuditEventStatus::Failure
                }
            }
        };
        if let Some(object) = self.detail.as_object_mut() {
            object.insert(
                "poster_class".into(),
                serde_json::json!(self.poster.map(AuthorClass::as_str)),
            );
        }
        let tool_name = format!("orbit.notice.{}", self.verb);
        let workspace_id = runtime.workspace_id().ok();
        if let Err(error) = record_system_audit_event(
            runtime,
            SystemAuditEvent {
                execution_id_prefix: "audit-notice",
                command: "notice",
                subcommand: Some(self.verb),
                tool_name: Some(&tool_name),
                target_type: "notice",
                target_id: self.target.as_deref(),
                role: self.poster.map_or("unidentified", AuthorClass::as_str),
                status,
                arguments_json: self.detail.to_string(),
                workspace_id: workspace_id.as_deref(),
                task_id: None,
                attribution: AuditRunAttribution::Environment { run_id: None },
            },
        ) {
            tracing::warn!(%error, verb = self.verb, "failed to record the notice audit row");
        }
    }
}

/// The stored row of a notice.
pub(crate) fn notice_row(notice: &Notice) -> Result<NoticeRow, OrbitError> {
    Ok(NoticeRow {
        workspace_id: notice.workspace.id.clone(),
        id: notice.id.clone(),
        scope: notice.scope.as_str().to_string(),
        kind: notice.kind.as_str().to_string(),
        dedupe_key: notice.dedupe_key.clone(),
        author_class: notice.author.class.as_str().to_string(),
        author_crew: notice.author.crew.clone(),
        author_task_id: notice.author.task_id.clone(),
        linked_task_id: notice.linked_task.as_ref().map(|task| task.id.clone()),
        close_on_linked_done: notice.close_on_linked_done,
        expires_at: notice.expires_at,
        opened_at: notice.opened_at,
        updated_at: notice.updated_at,
        closed_at: notice.closed.as_ref().map(|closed| closed.at),
        revision: notice.revision,
        record_json: serde_json::to_string(notice)
            .map_err(|error| OrbitError::Execution(format!("serialize notice: {error}")))?,
    })
}

/// The notice a stored row holds.
pub(crate) fn parse_notice_row(row: &NoticeRow) -> Result<Notice, OrbitError> {
    let mut notice: Notice = serde_json::from_str(&row.record_json)
        .map_err(|error| OrbitError::Store(format!("notice {} is unreadable: {error}", row.id)))?;
    if let Some(linked) = notice.linked_task.as_mut() {
        linked.status = None;
    }
    Ok(notice)
}
