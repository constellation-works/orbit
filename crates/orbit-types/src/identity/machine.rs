use super::IdentityError;

/// Maximum encoded length for a stable registry identifier.
pub const REGISTRY_IDENTIFIER_MAX_BYTES: usize = 128;
/// Namespace prefix for generated machine identifiers.
pub const MACHINE_ID_PREFIX: &str = "hm_";

/// Validate a path-free, normalized identifier stored in public registry
/// records. Transport targets and filesystem paths are never identities.
pub fn validate_registry_identifier(field: &str, value: &str) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::Invalid(format!("{field} must not be empty")));
    }
    if value.trim() != value {
        return Err(IdentityError::Invalid(format!(
            "{field} must not contain leading or trailing whitespace"
        )));
    }
    if value.len() > REGISTRY_IDENTIFIER_MAX_BYTES {
        return Err(IdentityError::Invalid(format!(
            "{field} must not exceed {REGISTRY_IDENTIFIER_MAX_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) || value.contains(['/', '\\']) {
        return Err(IdentityError::Invalid(format!(
            "{field} must be a logical registry identifier, not a path"
        )));
    }
    Ok(())
}

/// Validate the stable machine key used in machine identity and workspace
/// role records.
pub fn validate_machine_id(machine_id: &str) -> Result<(), IdentityError> {
    validate_registry_identifier("machine_id", machine_id)?;
    let Some(suffix) = machine_id.strip_prefix(MACHINE_ID_PREFIX) else {
        return Err(IdentityError::Invalid(
            "machine_id must use the canonical 'hm_' namespace".to_string(),
        ));
    };
    if suffix.is_empty()
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(IdentityError::Invalid(
            "machine_id must contain 'hm_' followed by ASCII letters, digits, '_' or '-'"
                .to_string(),
        ));
    }
    Ok(())
}

/// Validate the operator-chosen display name for a machine (`machine.name`).
pub fn validate_machine_name(machine_name: &str) -> Result<(), IdentityError> {
    validate_registry_identifier("machine_name", machine_name)
}

/// Task-id namespaces Orbit reserves for its own artifact kinds, plus the
/// historical default. None may be chosen for a fresh machine.
const RESERVED_TASK_PREFIXES: [&str; 4] = ["ORB", "ADR", "L", "F"];
/// The namespace every pre-`machine.task_prefix` installation minted under.
/// Still valid when already persisted; never selectable for a new machine.
pub const LEGACY_TASK_PREFIX: &str = "ORB";

/// Validate an operator's fresh task-prefix choice.
///
/// The persisted migration prefix `ORB` remains valid for existing machines,
/// but cannot be selected for a new identity.
pub fn validate_new_task_prefix(value: &str) -> Result<String, IdentityError> {
    if RESERVED_TASK_PREFIXES.contains(&value) {
        return Err(IdentityError::Invalid(format!(
            "task prefix '{value}' is reserved; choose 2-5 uppercase ASCII letters other than ORB, ADR, L, or F"
        )));
    }
    if !(2..=5).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(IdentityError::Invalid(
            "task prefix must be 2-5 uppercase ASCII letters".to_string(),
        ));
    }
    Ok(value.to_string())
}

/// Validate a task prefix already persisted for this machine. Identical to
/// [`validate_new_task_prefix`] except that the historical `ORB` namespace is
/// accepted, because machines predating the prefix choice mint under it.
pub fn validate_stored_task_prefix(value: &str) -> Result<String, IdentityError> {
    if value == LEGACY_TASK_PREFIX {
        return Ok(value.to_string());
    }
    validate_new_task_prefix(value)
}
