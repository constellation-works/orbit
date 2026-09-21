use orbit_common::OrbitError;

use super::TASK_HYBRID_FALLBACK_NOTE;

pub(super) fn warn_task_hybrid_fallback(notes: &mut Vec<String>, reason: &str) {
    orbit_common::tracing::warn!(
        target: "orbit.search.tasks",
        reason,
        "falling back to lexical task search"
    );
    push_skip_note(
        notes,
        "task hybrid vector",
        &format!("{TASK_HYBRID_FALLBACK_NOTE}: {reason}"),
    );
}

/// The companion's install remediation is appropriate for an explicit
/// semantic command, but hybrid search is intentionally best-effort.
pub(super) fn fallback_reason(error: &OrbitError) -> String {
    match error {
        OrbitError::CompanionNotInstalled(_) => {
            "optional inference companion unavailable".to_string()
        }
        OrbitError::Store(message)
            if message
                .contains("semantic index layout is incompatible with this Orbit runtime") =>
        {
            message.clone()
        }
        _ => error.to_string(),
    }
}

pub(super) fn push_skip_note(notes: &mut Vec<String>, branch: &str, reason: &str) {
    notes.push(format!("{branch} branch skipped: {reason}"));
}
