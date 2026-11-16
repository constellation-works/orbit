//! `routine_tripped`: one item per routine whose circuit breaker is open on
//! this host [ORB-25103]. The breaker is host-local scheduler state, so every
//! checkout reports the routines its workspace defines.

use super::sources::attention_reason;
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::attention::cursor;
use orbit_store::contracts::RoutineBreakerState;
use orbit_types::tool::{
    AttentionHost, AttentionItem, AttentionKind, AttentionNext, AttentionSeverity,
    AttentionWorkspace,
};
use serde_json::json;

pub(super) fn routine_tripped_items(
    runtime: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    now: DateTime<Utc>,
) -> Result<Vec<AttentionItem>, OrbitError> {
    let breakers = crate::application::routines::host_routine_breakers(&runtime.global_root())?;
    let mut items = Vec::new();
    for breaker in breakers.into_values() {
        if breaker.state != RoutineBreakerState::Open || breaker.source_workspace != workspace.name
        {
            continue;
        }
        let since = breaker
            .tripped_at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map_or(now, |at| at.with_timezone(&Utc));
        let name = &breaker.routine_name;
        let class = breaker.error_class.as_deref().unwrap_or("failed");
        let next = AttentionNext {
            summary: "Fix the cause, then reset the routine's breaker".into(),
            tool: None,
            input: None,
            command: format!("orbit routine breaker reset {name}"),
            needs: Vec::new(),
            also: vec![
                AttentionNext {
                    summary: "Inspect the routine and its recent fires".into(),
                    tool: None,
                    input: None,
                    command: format!("orbit routine show {name}"),
                    needs: Vec::new(),
                    also: Vec::new(),
                },
                AttentionNext {
                    summary: "Fire it once now to test a fix".into(),
                    tool: None,
                    input: None,
                    command: format!("orbit routine breaker bypass {name}"),
                    needs: Vec::new(),
                    also: Vec::new(),
                },
            ],
        };
        let detail = json!({
            "routine": name,
            "error_class": breaker.error_class,
            "last_run_id": breaker.last_run_id,
            "trips": breaker.trips,
            "next_trial_at": breaker.next_trial_at,
        });
        let mut item = AttentionItem {
            id: cursor::attention_item_id(
                AttentionKind::RoutineTripped,
                &host.machine_id,
                &workspace.id,
                &json!(name),
            ),
            kind: AttentionKind::RoutineTripped,
            severity: AttentionSeverity::High,
            workspace: workspace.clone(),
            host: host.clone(),
            refs: json!({"routine": name, "run_id": breaker.last_run_id}),
            task: None,
            since: Some(since),
            age_seconds: Some((now - since).num_seconds().max(0) as u64),
            expires_at: None,
            reason: attention_reason(&format!(
                "Routine {name} stopped firing after repeated {class} failures"
            )),
            detail,
            next,
            fingerprint: String::new(),
        };
        item.fingerprint = cursor::attention_fingerprint(
            &item,
            json!({"tripped_at": breaker.tripped_at, "trips": breaker.trips}),
        );
        items.push(item);
    }
    Ok(items)
}
