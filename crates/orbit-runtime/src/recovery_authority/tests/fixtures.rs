//! Forge fixtures for the host-only recovery authority.
//!
//! Every attack here models the same capability the sandbox actually grants a
//! managed leaf: arbitrary bytes in the shared run store. The certificate the
//! host wrote lives elsewhere, so the fixture edits the *checkpoint* and
//! asserts the record no longer matches.

use std::path::Path;

use orbit_engine::RebaseRecoveryAttemptScope;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::RecoveryAuthority;

pub(super) const RUN_ID: &str = "jrun-20260910-0001-1";
pub(super) const OTHER_RUN_ID: &str = "jrun-20260910-0002-1";
pub(super) const STEP_ID: &str = "sync_base";
pub(super) const HEAD_BEFORE: &str = "1111111111111111111111111111111111111111";
pub(super) const PINNED_BASE: &str = "3333333333333333333333333333333333333333";

pub(super) fn checkpoint(run_id: &str, step_id: &str, workspace: &Path) -> Value {
    json!({
        "run_id": run_id,
        "step_id": step_id,
        "task_ids": ["ORB-11977"],
        "workspace_path": workspace,
        "head": "orbit/ORB-11977",
        "head_sha_before": HEAD_BEFORE,
        "original_base_sha": "2222222222222222222222222222222222222222",
        "base_ref": "refs/remotes/origin/agent-main",
        "target_base_sha": PINNED_BASE,
        "base_sha": PINNED_BASE,
        "remote_sha_before": Value::Null,
        "head_sha": "4444444444444444444444444444444444444444",
        "rewritten": true,
        "recovery_attempt": 1,
    })
}

pub(super) fn scope(workspace: &Path) -> RebaseRecoveryAttemptScope {
    RebaseRecoveryAttemptScope {
        workspace_path: workspace.to_string_lossy().into_owned(),
        head_sha_before: HEAD_BEFORE.to_string(),
        target_base_sha: PINNED_BASE.to_string(),
    }
}

/// `previous`, completed again by a later attempt that landed another HEAD.
pub(super) fn later_attempt(previous: &Value, attempt: u64, head_sha: &str) -> Value {
    let mut later = previous.clone();
    later["recovery_attempt"] = json!(attempt);
    later["head_sha"] = json!(head_sha);
    later
}

pub(super) fn fixture() -> (TempDir, TempDir, Value) {
    let global = TempDir::new().expect("global root");
    let workspace = TempDir::new().expect("workspace");
    let accepted = checkpoint(RUN_ID, STEP_ID, workspace.path());
    let authority = RecoveryAuthority::open(global.path()).expect("open authority");
    let attempt = authority
        .begin_attempt(RUN_ID, STEP_ID, &scope(workspace.path()))
        .expect("reserve attempt");
    assert_eq!(attempt, 1);
    authority
        .issue(RUN_ID, STEP_ID, &accepted)
        .expect("issue certificate");
    (global, workspace, accepted)
}
