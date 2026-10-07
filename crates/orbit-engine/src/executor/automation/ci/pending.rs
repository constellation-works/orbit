//! Red runs held back while a newer push run of the same workflow and branch is
//! still running on a descendant commit.
//!
//! Such a run may already carry the repair: the failure it would file is only
//! known to be current once the descendant's run completes. Nothing is filed
//! for it this sweep; it is listed in `pending_supersession` with both run ids,
//! and a later sweep files the newest completed run if it still fails.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::query::CiQueries;

/// Why a current failure was deferred rather than filed.
pub(super) const IN_FLIGHT_DESCENDANT_REASON: &str = "newer_descendant_run_in_flight";

/// Bound on ancestry checks per sweep. Each is a local Git query unless the
/// checkout first has to fetch a run commit it has not seen.
const MAX_ANCESTRY_CHECKS: usize = 25;

/// Move each current failure with an in-flight push successor at a descendant
/// commit out of `current`. A failure whose ancestry cannot be established
/// stays current, as before, and a note says why.
pub(super) fn defer_for_in_flight_descendants<Q: CiQueries + ?Sized>(
    queries: &Q,
    current: Vec<Value>,
    successors: &BTreeMap<u64, Vec<Value>>,
    notes: &mut Vec<String>,
) -> (Vec<Value>, Vec<Value>) {
    let mut kept = Vec::new();
    let mut pending = Vec::new();
    let mut checks = 0usize;
    let mut unchecked = 0usize;
    for failure in current {
        let newer = failure
            .get("run_id")
            .and_then(Value::as_u64)
            .and_then(|run_id| successors.get(&run_id));
        let commit = failure
            .get("event_reported_head_sha")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let (Some(newer), Some(commit)) = (newer, commit) else {
            kept.push(failure);
            continue;
        };
        let mut descendant = None;
        for successor in newer {
            let Some(sha) = successor.get("reported_head_sha").and_then(Value::as_str) else {
                continue;
            };
            if checks == MAX_ANCESTRY_CHECKS {
                unchecked += 1;
                break;
            }
            checks += 1;
            match queries.is_ancestor(&commit, sha) {
                Ok(true) => {
                    descendant = Some(successor);
                    break;
                }
                Ok(false) => {}
                Err(error) => notes.push(format!(
                    "run {} could not be compared with in-flight run {} ({error}); the failure \
                     stays current",
                    display_id(&failure),
                    display_id(successor),
                )),
            }
        }
        match descendant {
            Some(successor) => pending.push(pending_entry(
                failure,
                successor,
                &commit,
                sha_of(successor),
            )),
            None => kept.push(failure),
        }
    }
    if unchecked > 0 {
        notes.push(format!(
            "{unchecked} current failure(s) with an in-flight successor were not checked for \
             ancestry (cap {MAX_ANCESTRY_CHECKS}) and stay current"
        ));
    }
    if !pending.is_empty() {
        notes.push(format!(
            "{} current failure(s) are held in pending_supersession because a newer push run of \
             the same workflow on the same branch is still running at a descendant commit; \
             nothing is filed for them until that run completes",
            pending.len()
        ));
    }
    (kept, pending)
}

fn pending_entry(mut failure: Value, successor: &Value, commit: &str, newer_commit: &str) -> Value {
    failure["reason"] = json!(IN_FLIGHT_DESCENDANT_REASON);
    failure["evidence"] = json!(format!(
        "newer push run {} of workflow '{}' on branch '{}' is {} at {newer_commit}, a descendant \
         of the failing commit {commit}; this failure is filed only if it reproduces on the \
         newest completed run",
        display_id(successor),
        failure
            .get("workflow")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        failure
            .get("head_branch")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        successor
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("not completed"),
    ));
    failure["pending_on"] = json!({
        "run_id": successor.get("run_id"),
        "url": successor.get("url"),
        "created_at": successor.get("created_at"),
        "status": successor.get("status"),
        "event": successor.get("event"),
        "reported_head_sha": newer_commit,
    });
    failure
}

fn sha_of(run: &Value) -> &str {
    run.get("reported_head_sha")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn display_id(run: &Value) -> String {
    run.get("run_id")
        .and_then(Value::as_u64)
        .map_or_else(|| "unknown".to_string(), |id| id.to_string())
}
