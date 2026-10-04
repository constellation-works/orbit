//! Destination discovery decoding and structured remote error projection.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::workspace::Workspace;
use serde_json::{Map, Value};

use super::super::config::Destination;
use super::contracts::DestinationSnapshot;

pub(crate) fn snapshot_from_discovery_content(
    destination: &Destination,
    content: &Value,
) -> Result<DestinationSnapshot, OrbitError> {
    let machine_id = content["machine_id"]
        .as_str()
        .ok_or_else(|| {
            unreachable(
                destination,
                "discovery answer carried no machine_id".to_string(),
            )
        })?
        .to_string();
    // Crew keys ride on the rows but are not workspace record fields, which
    // refuse unknown keys; lift them out before the rows are read.
    let mut rows = content["workspaces"].clone();
    let mut crews = BTreeMap::new();
    if let Some(rows) = rows.as_array_mut() {
        for row in rows.iter_mut().filter_map(Value::as_object_mut) {
            let lifted = ["crews", "crews_error"]
                .into_iter()
                .filter_map(|key| row.remove(key).map(|value| (key.to_string(), value)))
                .collect::<Map<_, _>>();
            if let (false, Some(id)) = (lifted.is_empty(), row.get("id").and_then(Value::as_str)) {
                crews.insert(id.to_string(), lifted);
            }
        }
    }
    let workspaces: Vec<Workspace> = serde_json::from_value(rows).map_err(|error| {
        unreachable(
            destination,
            format!("discovery answer was not a workspace list: {error}"),
        )
    })?;
    Ok(DestinationSnapshot {
        machine_id,
        workspaces,
        crews,
    })
}

pub(super) fn unreachable(destination: &Destination, reason: String) -> OrbitError {
    OrbitError::UnreachableDestination(format!("{}: {reason}", destination.machine_id))
}

/// Preserve a destination's structured tool error as-is.
pub(super) fn remote_tool_error(destination: &Destination, payload: &Value) -> OrbitError {
    let code = payload["code"]
        .as_str()
        .unwrap_or("execution_failed")
        .to_string();
    let message = payload["message"].as_str().unwrap_or_default();
    OrbitError::RemoteTool {
        code,
        message: format!("{}: {message}", destination.machine_id),
        payload: payload.clone(),
    }
}
