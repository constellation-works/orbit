//! Shared confirmation checks for irreversible command actions.

use orbit_core::OrbitError;

/// Require the standard non-interactive confirmation flag before an
/// irreversible CLI operation proceeds.
pub(crate) fn require_confirmation(confirm: bool, action: &str) -> Result<(), OrbitError> {
    if confirm {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "{action} is irreversible; pass --confirm to proceed"
    )))
}
