//! Compact automation diagnostics and on-demand state and coverage evidence.

use super::{bad_request, blocking, not_found, routines::OperationsQuery};
use crate::state::DashboardState;
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

const SAMPLE_LIMIT: usize = 20;

/// Project inspection state for polling without copying retained membership.
pub(super) fn summary(diagnostic: &Value) -> Value {
    let Some(fields) = diagnostic.as_object() else {
        return diagnostic.clone();
    };
    let mut result: serde_json::Map<String, Value> = fields
        .iter()
        .filter(|(name, _)| name.as_str() != "state")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let Some(original) = diagnostic["state"].as_object() else {
        result.insert("state".into(), Value::Null);
        return Value::Object(result);
    };
    let inventories = [
        "pending",
        "pending_commits",
        "waived",
        "excluded",
        "unresolved",
        "associations",
        "members",
    ];
    let mut state: serde_json::Map<String, Value> = original
        .iter()
        .filter(|(name, _)| !inventories.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut counts = serde_json::Map::new();
    for name in ["pending", "pending_commits", "waived", "excluded"] {
        if let Some(Value::Array(entries)) = original.get(name) {
            counts.insert(name.into(), json!(entries.len()));
            if name == "excluded" {
                state.insert(
                    name.into(),
                    json!(entries.iter().take(SAMPLE_LIMIT).collect::<Vec<_>>()),
                );
            }
        }
    }
    counts.insert(
        "unresolved".into(),
        json!(object_len(&diagnostic["state"]["unresolved"])),
    );
    state.insert(
        "unresolved".into(),
        sample_map(&diagnostic["state"]["unresolved"], Clone::clone),
    );
    state.insert("counts".into(), Value::Object(counts));
    if let Some(members) = original
        .get("members")
        .filter(|members| members.is_object())
    {
        let pending = &members["pending"];
        let fresh = members["assessed"]
            .as_object()
            .into_iter()
            .flat_map(|entries| entries.iter())
            .filter(|(key, assessment)| {
                pending.get(*key).is_none_or(|member| {
                    member["fingerprint"] == assessment["resulting_fingerprint"]
                })
            })
            .map(|(_, assessment)| assessment)
            .collect::<Vec<_>>();
        state.insert("members".into(), json!({
            "counts": {
                "pending": object_len(pending),
                "fresh": fresh.len(),
                "ready": fresh.iter().filter(|assessment| assessment["ready"] == true).count(),
                "withheld": object_len(&members["withheld"]),
                "failed": object_len(&members["failed"]),
            },
            "withheld": sample_map(&members["withheld"], Clone::clone),
            "failed": sample_map(&members["failed"], member_attempt),
            "active": members.get("active").filter(|active| !active.is_null()).map(member_attempt),
            "scan_after": members["scan_after"],
        }));
    }
    result.insert("state".into(), Value::Object(state));
    Value::Object(result)
}

fn object_len(value: &Value) -> usize {
    value.as_object().map_or(0, serde_json::Map::len)
}

fn sample_map(value: &Value, project: impl Fn(&Value) -> Value) -> Value {
    Value::Object(
        value
            .as_object()
            .into_iter()
            .flat_map(|entries| entries.iter())
            .take(SAMPLE_LIMIT)
            .map(|(key, value)| (key.clone(), project(value)))
            .collect(),
    )
}

fn member_attempt(attempt: &Value) -> Value {
    json!({
        "member": {"key": attempt["member"]["key"]},
        "members": attempt["members"].as_array().map(|members| members.iter().map(|member| json!({"key": member["key"]})).collect::<Vec<_>>()).unwrap_or_default(),
        "attempt": attempt["attempt"],
        "max_attempts": attempt["max_attempts"],
        "deadline": attempt["deadline"],
        "action_id": attempt["action_id"],
    })
}

/// Full persisted membership is a read-only, explicitly requested resource.
pub(super) async fn full_state(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Path((kind, name)): Path<(String, String)>,
) -> Response {
    let Some(workspace) = query
        .workspace
        .filter(|workspace| !workspace.trim().is_empty())
    else {
        return bad_request("select a workspace".into());
    };
    if !matches!(kind.as_str(), "auto-task" | "routine") {
        return not_found("automation state not found".into());
    }
    match blocking("automation state", move || {
        let runtime = match super::auto_tasks::resolve_workspace(&state, &workspace) {
            Ok((_, runtime)) => runtime,
            Err(reason) => return Ok(Err(reason)),
        };
        let consumer = orbit_core::application::automation::consumer_key(&runtime, &kind, &name)?;
        Ok(Ok(runtime
            .automation_store()?
            .automation_state(&consumer)?))
    })
    .await
    {
        Ok(Ok(Some(state))) => axum::Json(json!({"state": state})).into_response(),
        Ok(Ok(None)) => not_found("automation state not found".into()),
        Ok(Err(reason)) => not_found(reason),
        Err(response) => *response,
    }
}

pub(super) async fn accepted_evidence(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Path((kind, name, batch)): Path<(String, String, String)>,
) -> Response {
    let Some(workspace) = query.workspace.as_deref() else {
        return bad_request("select a workspace".to_string());
    };
    if !matches!(kind.as_str(), "auto-task" | "routine") {
        return not_found("coverage evidence not found".to_string());
    }
    let workspace = workspace.to_string();
    match blocking("coverage evidence", move || {
        let runtime = match super::auto_tasks::resolve_workspace(&state, &workspace) {
            Ok((_, runtime)) => runtime,
            Err(reason) => return Ok(EvidenceLookup::UnknownWorkspace(reason)),
        };
        match (|| {
            let consumer =
                orbit_core::application::automation::consumer_key(&runtime, &kind, &name)?;
            runtime
                .automation_store()?
                .automation_receipt(&consumer, &batch)
        })() {
            Ok(Some(receipt)) => Ok(EvidenceLookup::Body(receipt.evidence)),
            Ok(None) => Ok(EvidenceLookup::Missing),
            Err(error) => Err(error),
        }
    })
    .await
    {
        Ok(EvidenceLookup::Body(evidence)) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            evidence,
        )
            .into_response(),
        Ok(EvidenceLookup::Missing) => not_found("coverage evidence not found".to_string()),
        Ok(EvidenceLookup::UnknownWorkspace(reason)) => not_found(reason),
        Err(response) => *response,
    }
}

enum EvidenceLookup {
    Body(Vec<u8>),
    Missing,
    UnknownWorkspace(String),
}
