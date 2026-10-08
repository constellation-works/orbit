use std::sync::{Arc, Mutex, PoisonError};

use orbit_agent::loop_engine::audit::{AuditSink, LoopAuditEvent};
use orbit_types::workflow::activity_job::{V2AuditEventKind, tool_allowed};

use super::audit_writer::V2AuditWriter;

/// Decision emitted when enforcement fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnforcementDecision {
    Allowed,
    Denied { tool_name: String, reason: String },
}

/// AuditSink wrapper that enforces a tool allowlist at the Orbit layer.
///
/// Usage: Build the inner sink (e.g. V2SqliteSink / InMemorySink), wrap it
/// with an EnforcedAuditSink at construction, and pass the wrapper into
/// the event producer as its audit sink. The wrapper intercepts
/// ToolCallRequested events, checks the name against the allowlist, and (on
/// deny) substitutes a PolicyDenial event and signals the caller to
/// terminate.
pub struct EnforcedAuditSink {
    inner: Arc<dyn AuditSink>,
    allowlist: Vec<String>,
    writer: Arc<V2AuditWriter>,
    run_id: String,
    session_id: String,
    tripped: Mutex<Option<EnforcementDecision>>,
}

impl EnforcedAuditSink {
    pub fn new(
        inner: Arc<dyn AuditSink>,
        allowlist: Vec<String>,
        writer: Arc<V2AuditWriter>,
        run_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            allowlist,
            writer,
            run_id: run_id.into(),
            session_id: session_id.into(),
            tripped: Mutex::new(None),
        }
    }

    pub fn tripped(&self) -> Option<EnforcementDecision> {
        self.tripped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl AuditSink for EnforcedAuditSink {
    fn emit(&self, event: &LoopAuditEvent) {
        // Mirror explicit PolicyDenial events into the workflow envelope trail.
        // Also reject ToolCallRequested events outside the allowlist, emitting
        // both an envelope denial and a retained provider-level denial event.
        match event {
            LoopAuditEvent::PolicyDenial {
                tool_name, reason, ..
            } => {
                self.writer.emit_lossy(V2AuditEventKind::ToolDenied {
                    tool_name: tool_name.clone(),
                    reason: reason.clone(),
                });
                *self.tripped.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(EnforcementDecision::Denied {
                        tool_name: tool_name.clone(),
                        reason: reason.clone(),
                    });
                self.inner.emit(event);
            }
            LoopAuditEvent::ToolCallRequested { tool_name, .. }
                if !tool_allowed(tool_name, &self.allowlist) =>
            {
                let reason = format!("tool `{tool_name}` not in allowlist");
                self.writer.emit_lossy(V2AuditEventKind::ToolDenied {
                    tool_name: tool_name.clone(),
                    reason: reason.clone(),
                });
                self.inner.emit(event);
                let denial = LoopAuditEvent::PolicyDenial {
                    ts: chrono::Utc::now(),
                    run_id: self.run_id.clone(),
                    session_id: self.session_id.clone(),
                    iteration: 0,
                    tool_name: tool_name.clone(),
                    reason: reason.clone(),
                };
                self.inner.emit(&denial);
                *self.tripped.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(EnforcementDecision::Denied {
                        tool_name: tool_name.clone(),
                        reason,
                    });
            }
            _ => self.inner.emit(event),
        }
    }

    fn write_blob(&self, content: &[u8]) -> String {
        self.inner.write_blob(content)
    }
}
