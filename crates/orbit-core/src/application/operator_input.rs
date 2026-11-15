//! Input bounds shared by every surface that runs an operator handoff-console
//! mutation (dashboard, `orbit tool run`, MCP and remote host routing). Each
//! surface parses its input; these checks decide what the owner accepts.

use orbit_common::OrbitError;

/// Longest accepted replay key, in bytes. Long enough for a UUID or a digest,
/// short enough that a caller cannot use the mutation key as storage.
pub const MAX_REPLAY_KEY_BYTES: usize = 128;
/// Longest accepted operator reason, in bytes. It is stored as a status note.
pub const MAX_REASON_BYTES: usize = 2000;

/// Trim and check a replay key: 1..=128 bytes of ASCII letters, digits, `-`
/// or `_`. The key names one decision so a retry replays it instead of
/// recording a second one.
pub fn replay_key(raw: &str) -> Result<String, OrbitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(OrbitError::InvalidInput(
            "request key identifies this decision so a retry replays it instead of recording a \
             second one; send one"
                .to_string(),
        ));
    }
    if trimmed.len() > MAX_REPLAY_KEY_BYTES {
        return Err(OrbitError::InvalidInput(format!(
            "request key must be at most {MAX_REPLAY_KEY_BYTES} bytes"
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(OrbitError::InvalidInput(
            "request key must contain only ASCII letters, digits, '-' or '_'".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Trim and check an operator reason: 1..=2000 bytes. `action` names the
/// operation in the refusal, for example `claim recovery`.
pub fn reason(raw: &str, action: &str) -> Result<String, OrbitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "{action} requires a reason"
        )));
    }
    if trimmed.len() > MAX_REASON_BYTES {
        return Err(OrbitError::InvalidInput(format!(
            "reason must be at most {MAX_REASON_BYTES} bytes"
        )));
    }
    Ok(trimmed.to_string())
}

/// The durable receipt key for one operator decision: the surface's replay
/// namespace, the subject it acts on, then the caller's replay key. The
/// namespace is persisted, so each surface keeps its own.
pub(crate) fn scoped_replay_key(scope: &str, subject: &str, key: &str) -> String {
    format!("{scope}:{subject}:{key}")
}
