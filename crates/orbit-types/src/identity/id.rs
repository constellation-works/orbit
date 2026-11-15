/// A string identifier for a durable record, such as a task, a run or a notice.
///
/// This is a type alias rather than a newtype so it serializes and deserializes
/// as a plain string without needing custom serde impls. The format belongs to
/// the record kind and to the component that allocates it (for example the task
/// counter in `orbit-store` or the minute-stamped run IDs), so callers treat the
/// value as opaque and check a format with that kind's validator, such as
/// `validate_friction_id`, rather than parsing it inline.
pub type OrbitId = String;
