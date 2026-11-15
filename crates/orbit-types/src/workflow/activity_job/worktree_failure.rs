//! Human messages and retained diagnostics for worktree boundary failures.

use serde_json::Value;

/// A concise action for a worktree failure. Paths, identifiers and the full
/// reason remain in the diagnostic, including diagnostics from older runs.
pub fn worktree_failure_message(code: &str, diagnostic: &Value) -> &'static str {
    let reason = diagnostic
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match code {
        "worktree_mismatch" if reason.contains("worktree is missing") => {
            "Run failed because its worktree is missing. Restore the worktree or start a new run."
        }
        "worktree_mismatch"
            if diagnostic
                .get("requested_repo_root")
                .is_none_or(|root| root.is_null() || root.as_str() == Some("")) =>
        {
            "Run failed because no repo root was declared for its worktree. Declare matching workspace and repo roots, then retry."
        }
        "worktree_mismatch" if reason.contains("different Git checkouts") => {
            "Run failed because its workspace and repo roots name different checkouts. Declare matching roots, then retry."
        }
        "worktree_mismatch" if reason.contains("registered primary checkout") => {
            "Run failed because its primary checkout could not be verified. Check the workspace registration, then retry."
        }
        "worktree_mismatch" if reason.contains("collapses") => {
            "Run failed because its worktree is the primary checkout. Assign a separate worktree, then retry."
        }
        "worktree_mismatch" => {
            "Run failed because its worktree declaration could not be verified. Check the declared workspace and repo roots, then retry."
        }
        "primary_checkout_drift" => {
            "Run failed because the primary checkout changed during execution. Inspect and preserve its changes before retrying."
        }
        "worktree_content_conflict" => {
            "Run failed because the provider moved its worktree branch or HEAD. Inspect and preserve the candidate before retrying."
        }
        "resume_preservation_unverified" => {
            "Run failed because the preserved candidate could not be verified. Inspect its ownership and history before resuming."
        }
        "resume_checkpoint_refresh_exhausted" => {
            "Run failed because the delivery base kept advancing past the refresh limit. Preserve the candidate and inspect competing deliveries before retrying."
        }
        _ => {
            "Run failed its worktree integrity check. Inspect the diagnostic and preserve the candidate before retrying."
        }
    }
}

/// Split the historical message format without losing its original JSON.
/// Unrelated messages and malformed diagnostics are left unchanged.
pub fn legacy_worktree_failure(message: &str) -> Option<(String, Value)> {
    let (_, tail) = message.split_once("worktree integrity violation `")?;
    let (code, payload) = tail.split_once("`: ")?;
    let diagnostic: Value = serde_json::from_str(payload).ok()?;
    if !diagnostic.is_object() {
        return None;
    }
    Some((
        worktree_failure_message(code, &diagnostic).to_string(),
        diagnostic,
    ))
}
