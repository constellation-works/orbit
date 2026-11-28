//! Human messages and retained diagnostics for worktree boundary failures.

use serde_json::Value;

/// Changed primary paths a `primary_checkout_drift` message names. The full
/// list stays in the diagnostic.
const PRIMARY_PATH_LIMIT: usize = 5;

/// Bytes of one named primary path before the message cuts it short.
const PRIMARY_PATH_BYTES: usize = 120;

/// The fixed action for a worktree failure. Dynamic drift details belong in
/// [`worktree_failure_display_message`].
pub fn worktree_failure_message(code: &str, diagnostic: &Value) -> &'static str {
    fixed_worktree_failure_message(code, diagnostic)
}

/// A concise display message for a worktree failure. Paths, identifiers and
/// the full reason remain in the diagnostic, including diagnostics from older
/// runs. A `primary_checkout_drift` names a bounded list of changed primary
/// paths and whether the primary HEAD moved when the diagnostic carries those
/// fields.
pub fn worktree_failure_display_message(code: &str, diagnostic: &Value) -> String {
    if code == "primary_checkout_drift"
        && let Some(message) = primary_drift_message(diagnostic)
    {
        return message;
    }
    worktree_failure_message(code, diagnostic).to_string()
}

fn fixed_worktree_failure_message(code: &str, diagnostic: &Value) -> &'static str {
    let reason = diagnostic
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match code {
        "worktree_mismatch" if reason.contains("worktree is missing") => {
            "Run failed because its worktree is missing. Restore the worktree or start a new run."
        }
        // Only the no-pair refusal means the root was undeclared; other
        // mismatches with a null repo root name their own cause below.
        "worktree_mismatch" if reason.contains("distinct checkouts require a declared") => {
            "Run failed because no repo root was declared for its worktree. Declare matching workspace and repo roots, then retry."
        }
        "worktree_mismatch" if reason.contains("different Git checkouts") => {
            "Run failed because its workspace and repo roots resolve to different Git checkouts. Declare matching roots, then retry."
        }
        "worktree_mismatch" if reason.contains("exact Git checkout root") => {
            "Run failed because a declared root is not the exact Git checkout root. Declare the checkout root itself, then retry."
        }
        // Checked before the primary-checkout arm: a collapsed pair also names
        // the registered primary checkout, and its cause is the collapse.
        "worktree_mismatch" if reason.contains("collapses") => {
            "Run failed because its worktree collapses to the registered primary checkout. Assign a separate worktree, then retry."
        }
        "worktree_mismatch" if reason.contains("registered primary checkout") => {
            "Run failed because its primary checkout could not be verified. Check the workspace registration, then retry."
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

/// The drift message with its head state and bounded changed paths. `None`
/// when the diagnostic lacks those fields, so older stored diagnostics keep
/// the fixed text.
fn primary_drift_message(diagnostic: &Value) -> Option<String> {
    let changed = diagnostic.get("primary_changed_paths")?.as_array()?;
    let head = primary_head_statement(diagnostic)?;
    let paths = changed.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    let mut message =
        format!("Run failed because the primary checkout changed during execution. {head}.");
    if !paths.is_empty() {
        let named = paths
            .iter()
            .take(PRIMARY_PATH_LIMIT)
            .map(|path| quoted_bounded_path(path))
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str(&format!(" Changed primary paths: {named}"));
        let hidden = paths.len().saturating_sub(PRIMARY_PATH_LIMIT);
        if hidden > 0 {
            message.push_str(&format!(" and {hidden} more"));
        }
        message.push('.');
    }
    message.push_str(" Inspect and preserve its changes before retrying.");
    Some(message)
}

/// Whether the primary HEAD moved, only its branch changed, or neither, as the
/// before and after fingerprint summaries record it.
fn primary_head_statement(diagnostic: &Value) -> Option<&'static str> {
    let before = diagnostic.get("primary_before")?;
    let after = diagnostic.get("primary_after")?;
    if before.get("head")? != after.get("head")? {
        return Some("HEAD moved");
    }
    let branch_before = before.get("branch").unwrap_or(&Value::Null);
    let branch_after = after.get("branch").unwrap_or(&Value::Null);
    if branch_before != branch_after {
        Some("HEAD did not move, but the branch changed")
    } else {
        Some("HEAD did not move")
    }
}

/// A primary path, cut to [`PRIMARY_PATH_BYTES`] at a character boundary and
/// quoted, so a path such as `0` cannot read as a count.
fn quoted_bounded_path(path: &str) -> String {
    let bounded = if path.len() <= PRIMARY_PATH_BYTES {
        path.to_string()
    } else {
        let mut end = PRIMARY_PATH_BYTES;
        while !path.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &path[..end])
    };
    format!("{bounded:?}")
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
        worktree_failure_display_message(code, &diagnostic),
        diagnostic,
    ))
}
