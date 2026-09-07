use orbit_core::AutoTaskDefinition;
use serde_json::Value;

/// Project a definition to its JSON form. The record is `Serialize`, so this
/// is the canonical on-disk shape (schemaVersion + fields).
pub(crate) fn definition_to_json(definition: &AutoTaskDefinition) -> Value {
    serde_json::to_value(definition).unwrap_or(Value::Null)
}

/// A one-line summary for the plain-text `list` output.
pub(crate) fn schedule_summary(definition: &AutoTaskDefinition) -> String {
    match &definition.schedule {
        orbit_core::AutoTaskSchedule::Deliveries {
            deliveries_landed: t,
        } => format!(
            "deliveries={} branch={} coverage={}",
            t.threshold, t.branch, t.coverage
        ),
        orbit_core::AutoTaskSchedule::Cron { cron } => format!("cron={cron}"),
        orbit_core::AutoTaskSchedule::Interval { every_minutes } => {
            format!("every={every_minutes}m")
        }
    }
}
