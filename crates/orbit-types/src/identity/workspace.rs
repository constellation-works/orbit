use super::{IdentityError, validate_registry_identifier};

/// Namespace prefix for canonical workspace identifiers.
pub const WORKSPACE_ID_PREFIX: &str = "ws_";

/// Validate a canonical workspace identifier used by registries and selectors.
pub fn validate_workspace_id(workspace_id: &str) -> Result<(), IdentityError> {
    validate_registry_identifier("workspace_id", workspace_id)?;
    validate_workspace_partition_id(workspace_id)
}

/// Validate the canonical workspace form accepted by persisted task-store partitions.
///
/// Partition validation historically did not impose the registry identifier
/// byte limit, so this preserves acceptance of existing long partition IDs.
pub fn validate_workspace_partition_id(workspace_id: &str) -> Result<(), IdentityError> {
    if is_canonical_workspace_id(workspace_id) {
        return Ok(());
    }
    Err(IdentityError::Invalid(format!(
        "workspace_id must use the canonical '{WORKSPACE_ID_PREFIX}<name>' form"
    )))
}

fn is_canonical_workspace_id(workspace_id: &str) -> bool {
    let Some(name) = workspace_id.strip_prefix(WORKSPACE_ID_PREFIX) else {
        return false;
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
    {
        return false;
    }
    true
}

/// Convert a workspace display name to its stable identifier.
pub fn canonicalize_workspace_id(name: &str) -> String {
    let mut canonical = String::new();
    let mut separator = false;
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || character == '_' {
            canonical.push(character);
            separator = false;
        } else if !canonical.is_empty() {
            separator = true;
        }
        if separator && !canonical.ends_with('-') {
            canonical.push('-');
            separator = false;
        }
    }
    while canonical.ends_with('-') {
        canonical.pop();
    }
    if canonical.is_empty() {
        canonical.push_str("workspace");
    }
    format!("{WORKSPACE_ID_PREFIX}{canonical}")
}
