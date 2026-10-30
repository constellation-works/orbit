//! Script-step attempts read back from the v2 audit trail: the
//! `script_execution` entries `orbit run show` lists for one run, and the
//! workspace-wide history `orbit script history <name>` reads.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::V2AuditEventFilter;
use orbit_types::workflow::activity_job::{ScriptExecutionRecord, V2_EVENT_TYPE_SCRIPT_EXECUTION};
use serde_json::Value;

use crate::RuntimeKernel;

use super::trail::RunAuditEvent;

const SCRIPT_EXECUTION_BODY_KIND: &str = "script_execution";
const SCRIPT_HISTORY_PAGE_SIZE: usize = 500;

/// One attempt of a script step, refusals included.
#[derive(Clone, Debug, PartialEq)]
pub struct RunScriptExecution {
    pub event_id: String,
    pub run_id: String,
    pub step_id: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub record: ScriptExecutionRecord,
}

impl RunScriptExecution {
    pub fn to_json(&self) -> Value {
        let mut value = serde_json::to_value(&self.record).unwrap_or(Value::Null);
        if let Some(object) = value.as_object_mut() {
            object.insert("event_id".into(), Value::String(self.event_id.clone()));
            object.insert("run_id".into(), Value::String(self.run_id.clone()));
            object.insert("step_id".into(), Value::String(self.step_id.clone()));
            object.insert(
                "timestamp".into(),
                self.timestamp
                    .map(|at| Value::String(at.to_rfc3339()))
                    .unwrap_or(Value::Null),
            );
        }
        value
    }

    /// One operator line: what ran, under which grant, and how it ended.
    pub fn describe(&self) -> String {
        let record = &self.record;
        let short = |digest: &str| digest.chars().take(12).collect::<String>();
        let outcome = match (
            record.refusal_code.as_deref(),
            record.failure_code.as_deref(),
        ) {
            (Some(code), _) => format!("refused {code}"),
            (None, Some(code)) => code.to_string(),
            (None, None) => match (record.exit_code, record.signal) {
                (Some(code), _) => format!("exit {code}"),
                (None, Some(signal)) => format!("signal {signal}"),
                (None, None) => record.outcome.clone(),
            },
        };
        let mut line = format!(
            "{} script `{}` digest {} grant {}: {outcome}",
            self.step_id,
            record.script,
            if record.digest.is_empty() {
                "?".to_string()
            } else {
                short(&record.digest)
            },
            record.grant_id.as_deref().unwrap_or("none"),
        );
        if let Some(duration_ms) = record.duration_ms {
            line.push_str(&format!(" in {:.3}s", duration_ms as f64 / 1000.0));
        }
        if let Some(log) = &record.log_blob_ref {
            line.push_str(&format!("; log {log}"));
        }
        line
    }
}

/// The script executions among one run's events, oldest first.
pub(super) fn script_executions_from_events(
    run_id: &str,
    events: &[RunAuditEvent],
) -> Vec<RunScriptExecution> {
    events
        .iter()
        .filter(|event| event.body_kind.as_deref() == Some(SCRIPT_EXECUTION_BODY_KIND))
        .filter_map(|event| script_execution(run_id, &event.event_id, event.timestamp, &event.raw))
        .collect()
}

fn script_execution(
    run_id: &str,
    event_id: &str,
    timestamp: Option<DateTime<Utc>>,
    raw: &Value,
) -> Option<RunScriptExecution> {
    let record = serde_json::from_value::<ScriptExecutionRecord>(raw.clone()).ok()?;
    Some(RunScriptExecution {
        event_id: event_id.to_string(),
        run_id: run_id.to_string(),
        step_id: raw.get("step_id").and_then(Value::as_str)?.to_string(),
        timestamp,
        record,
    })
}

impl RuntimeKernel {
    /// The newest `limit` attempts of script `name` in this workspace,
    /// newest first.
    pub fn script_execution_history(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<RunScriptExecution>, OrbitError> {
        let mut found = Vec::new();
        let mut offset = 0;
        while found.len() < limit {
            let page = self.list_v2_audit_events(V2AuditEventFilter {
                workspace_id: String::new(),
                event_type: Some(V2_EVENT_TYPE_SCRIPT_EXECUTION.to_string()),
                source: Some("v2_envelope".to_string()),
                limit: Some(SCRIPT_HISTORY_PAGE_SIZE),
                offset: Some(offset),
                ..Default::default()
            })?;
            let fetched = page.len();
            for row in page {
                let Ok(raw) = serde_json::from_str::<Value>(&row.payload_json) else {
                    continue;
                };
                if raw.get("script").and_then(Value::as_str) != Some(name) {
                    continue;
                }
                if let Some(execution) =
                    script_execution(&row.run_id, &row.event_id, Some(row.ts), &raw)
                {
                    found.push(execution);
                    if found.len() >= limit {
                        break;
                    }
                }
            }
            if fetched < SCRIPT_HISTORY_PAGE_SIZE {
                break;
            }
            offset += fetched;
        }
        Ok(found)
    }
}
