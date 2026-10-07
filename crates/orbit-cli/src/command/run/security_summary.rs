use orbit_core::JobRun;
use orbit_types::workflow::PipelineState;
use serde_json::Value;

/// Summarize the persisted filing result, including exclusions on success.
pub(super) fn security_alert_sweep_lines(run: &JobRun, state: Option<&PipelineState>) -> String {
    if run.job_id != "dependabot_alert_sweep_pipeline" {
        return String::new();
    }
    let Some(output) = state.and_then(|state| state.pipeline.get("file")) else {
        return String::new();
    };
    let Some(filed) = output.get("filed_count").and_then(Value::as_u64) else {
        return String::new();
    };
    let floor = output
        .get("min_severity")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    // Historical runs did not record a source; do not infer it from today's config.
    let source = output
        .get("min_severity_source")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let excluded = output
        .get("excluded_below_min_severity")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let alerts = excluded
        .iter()
        .map(|alert| {
            let family = alert
                .get("family")
                .and_then(Value::as_str)
                .unwrap_or("alert");
            let number = alert
                .get("number")
                .or_else(|| alert.get("alert_number"))
                .and_then(Value::as_u64)
                .map_or_else(|| "unavailable".to_string(), |number| format!("#{number}"));
            format!("{family} {number}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\n{} filed={filed} min_severity={floor} ({source})\n{} {}{}",
        crate::output::color::bold("Security alerts:"),
        crate::output::color::bold("Excluded below floor:"),
        excluded.len(),
        if alerts.is_empty() {
            String::new()
        } else {
            format!(" ({alerts})")
        },
    )
}
