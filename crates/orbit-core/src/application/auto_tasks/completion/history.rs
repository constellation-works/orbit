//! Persist and read owner-local accepted-result history.

use std::io::Read;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_common::fs::open_read_only_no_follow;
use orbit_types::workflow::{
    AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION, AutoTaskAcceptedHistory,
    AutoTaskAcceptedHistoryFile, AutoTaskCompletionReceipt, AutoTaskResultStatus,
};

use crate::OrbitRuntime;

/// Where the latest accepted receipt per definition is kept. Host-local, like
/// the schedule cursor beside it: the owner is the one host that accepts.
fn accepted_history_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime
        .paths()
        .state_dir
        .join("auto-task-accepted-results.json")
}

/// Advance a definition's accepted-result history. Only a settled record that
/// carries a passed result reaches it, so a successful probe outcome, which the
/// adapter never passes, cannot. Time only moves forward.
pub(in crate::application::auto_tasks) fn record_accepted_result(
    runtime: &OrbitRuntime,
    definition: &str,
    result: &AutoTaskCompletionReceipt,
) -> Result<(), OrbitError> {
    if result.status != AutoTaskResultStatus::Passed {
        return Ok(());
    }
    let path = accepted_history_path(runtime);
    with_exclusive_file_lock(&path, "auto-task accepted results", || {
        let mut history = read_accepted_history_file(&path)?.unwrap_or_default();
        let current = history.definitions.get(definition);
        if current.is_some_and(|current| current.completed_at >= result.completed_at) {
            return Ok(());
        }
        history
            .definitions
            .insert(definition.to_string(), result.clone());
        let encoded = serde_json::to_string_pretty(&history)
            .map_err(|error| OrbitError::Io(format!("encode accepted results: {error}")))?;
        atomic_write_text(&path, &encoded).map_err(|error| OrbitError::from_write_io(&path, error))
    })
}

fn read_accepted_history_file(
    path: &std::path::Path,
) -> Result<Option<AutoTaskAcceptedHistoryFile>, OrbitError> {
    let unreadable =
        |error: std::io::Error| OrbitError::Io(format!("read {}: {error}", path.display()));
    // Not following the final component, as for the schedule cursor beside it.
    let mut file = match open_read_only_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(error)),
    };
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(unreadable)?;
    let history: AutoTaskAcceptedHistoryFile = serde_json::from_str(&raw)
        .map_err(|error| OrbitError::Io(format!("parse {}: {error}", path.display())))?;
    if history.schema_version != AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION {
        return Err(OrbitError::Io(format!(
            "{} has schema_version {}",
            path.display(),
            history.schema_version
        )));
    }
    Ok(Some(history))
}

/// A definition's latest accepted result. History lives with the owner, so a
/// replica checkout answers unreadable rather than a misleading missing.
pub(in crate::application::auto_tasks) fn accepted_history(
    runtime: &OrbitRuntime,
    name: &str,
) -> AutoTaskAcceptedHistory {
    if let Some(owner) = runtime.coordination_write_owner() {
        return AutoTaskAcceptedHistory::Unreadable(format!(
            "accepted results live with the owner {owner}, not this replica"
        ));
    }
    match read_accepted_history_file(&accepted_history_path(runtime)) {
        Ok(None) => AutoTaskAcceptedHistory::Missing,
        Ok(Some(mut history)) => match history.definitions.remove(name) {
            None => AutoTaskAcceptedHistory::Missing,
            Some(receipt) if receipt.definition != name => AutoTaskAcceptedHistory::Unreadable(
                format!("the entry for `{name}` names `{}`", receipt.definition),
            ),
            Some(receipt) if receipt.status != AutoTaskResultStatus::Passed => {
                AutoTaskAcceptedHistory::Unreadable(format!(
                    "the entry for `{name}` is {}, which is never accepted",
                    receipt.status.as_str()
                ))
            }
            Some(receipt) => AutoTaskAcceptedHistory::Latest(Box::new(receipt)),
        },
        Err(error) => AutoTaskAcceptedHistory::Unreadable(error.to_string()),
    }
}
