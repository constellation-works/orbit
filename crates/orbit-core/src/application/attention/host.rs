//! Local `host_unhealthy`: the default `orbit doctor` workspace checks.
//!
//! The doctor's workspace checks include the follower's latest pull drain, so
//! an owner refusing this host's `orbit.drain.probe` on protocol skew is one
//! of the failing rows. Registered hosts' reachability, version and protocol
//! are judged by the federated aggregator, which probes them on every call.
use super::sources::attention_reason;
use crate::OrbitRuntime;
use orbit_common::{OrbitError, governance::attention::cursor};
use orbit_types::tool::{
    AttentionHealthCheck, AttentionHost, AttentionItem, AttentionKind, AttentionNext,
    AttentionSeverity, AttentionWorkspace,
};
use serde_json::json;

/// Checks kept in `detail.checks`; the fingerprint still covers every row.
const DETAIL_CHECK_CAP: usize = 32;

/// One item for `workspace` when a default doctor check fails or warns, or
/// `None` when every check passes or this runtime has no doctor source.
///
/// `catalog_runtime` lends the registry-backed catalog; `selected` is the
/// checkout the checks run against. Severity is `critical` when this host
/// owns the workspace, else `high`.
pub(super) fn read_doctor_host_item(
    catalog_runtime: &OrbitRuntime,
    selected: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    owner: bool,
) -> Result<Option<AttentionItem>, OrbitError> {
    let Some(catalog) = catalog_runtime.workspace_catalog() else {
        return Ok(None);
    };
    let Some(checks) = catalog.attention_doctor_checks(selected)? else {
        return Ok(None);
    };
    let mut failing = checks
        .into_iter()
        .filter(|check| check.status == "error" || check.status == "warning")
        .collect::<Vec<_>>();
    if failing.is_empty() {
        return Ok(None);
    }
    // Errors first, then report order.
    failing.sort_by_key(|check| check.status != "error");
    Ok(Some(doctor_item(failing, workspace, host, owner)))
}

fn doctor_item(
    failing: Vec<AttentionHealthCheck>,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    owner: bool,
) -> AttentionItem {
    let kind = AttentionKind::HostUnhealthy;
    let first = &failing[0];
    let reason = attention_reason(&format!("{}: {}", first.check, first.message));
    let mut state = failing
        .iter()
        .map(|check| json!([check.check, check.status]))
        .collect::<Vec<_>>();
    state.sort_by_key(|entry| entry.to_string());
    let command = format!("orbit --workspace '{}' doctor", workspace.id);
    let next = AttentionNext {
        summary: first
            .remediation
            .as_deref()
            .map(attention_reason)
            .unwrap_or_else(|| "Run orbit doctor and fix the failing checks".into()),
        tool: None,
        input: None,
        command,
        needs: Vec::new(),
        also: vec![AttentionNext {
            summary: "Inspect this host's registered peers".into(),
            tool: None,
            input: None,
            command: "orbit host list".into(),
            needs: Vec::new(),
            also: Vec::new(),
        }],
    };
    let mut item = AttentionItem {
        id: cursor::attention_item_id(kind, &host.machine_id, &workspace.id, &json!("doctor")),
        kind,
        severity: if owner {
            AttentionSeverity::Critical
        } else {
            AttentionSeverity::High
        },
        workspace: workspace.clone(),
        host: host.clone(),
        refs: json!({}),
        task: None,
        since: None,
        age_seconds: None,
        expires_at: None,
        reason,
        detail: json!({
            "cause": "doctor",
            "checks": failing.iter().take(DETAIL_CHECK_CAP).collect::<Vec<_>>(),
            "checks_truncated": failing.len() > DETAIL_CHECK_CAP,
        }),
        next,
        fingerprint: String::new(),
    };
    item.fingerprint = cursor::attention_fingerprint(&item, json!(state));
    item
}

/// The `degraded` entry for a session that may not read host health.
pub(super) fn host_capability_degraded(
    host: &AttentionHost,
) -> orbit_types::tool::AttentionDegraded {
    orbit_types::tool::AttentionDegraded {
        scope: format!("host:{}", host.name),
        kind: AttentionKind::HostUnhealthy.as_str().into(),
        code: "capability".into(),
        message: "Host health requires the Operator capability".into(),
    }
}
