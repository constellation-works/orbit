use orbit_common::OrbitError;

pub(in crate::application::task) fn invalid(message: &str) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}
pub(in crate::application::task) fn bounded_text(
    value: &str,
    required: bool,
) -> Result<(), OrbitError> {
    if value.len() > 32_768 || (required && value.trim().is_empty()) {
        return Err(invalid(
            "task text must be non-empty when required and at most 32768 bytes",
        ));
    }
    Ok(())
}
pub(in crate::application::task) fn criteria(values: &[String]) -> Result<(), OrbitError> {
    if values.is_empty() || values.len() > 100 {
        return Err(invalid("tasks require 1 to 100 acceptance criteria"));
    }
    for value in values {
        bounded_text(value, true)?;
    }
    Ok(())
}
