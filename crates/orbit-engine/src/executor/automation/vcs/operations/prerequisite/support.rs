use std::path::Path;

use orbit_common::OrbitError;
use serde_json::Value;

use super::super::super::completion_deadline::Deadline;

pub(in super::super) fn unreadable(reason: &str) -> OrbitError {
    OrbitError::workflow_evidence_unavailable(reason)
}
pub(in super::super) fn api(
    deadline: Deadline,
    workspace: &Path,
    endpoint: &str,
) -> Result<Value, OrbitError> {
    let response = deadline.execute(
        "gh",
        vec!["api".into(), endpoint.into()],
        workspace,
        &format!("workflow prerequisite read {endpoint}"),
    )?;
    serde_json::from_str(&response.stdout).map_err(|_| unreadable("invalid API JSON"))
}
pub(in super::super) fn complete_list<'a>(
    value: &'a Value,
    key: &str,
) -> Result<&'a Vec<Value>, OrbitError> {
    let list = value[key].as_array().ok_or_else(|| unreadable(key))?;
    if value["total_count"].as_u64() != Some(list.len() as u64) || list.len() > 100 {
        return Err(unreadable(&format!("truncated {key}")));
    }
    Ok(list)
}
pub(in super::super) fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
