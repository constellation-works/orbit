//! What a worktree GC run kept and rescued, read from its step output
//! [ORB-15613].

use orbit_types::workflow::PipelineState;
use serde_json::Value;

/// A summary line per worktree GC step, a `Kept worktree:` line for each
/// worktree it retained (reason and the bytes it still occupies), and a
/// `Rescued worktree:` line for each dirty worktree it removed (where the
/// candidate went), so a leak shows in `orbit run show` without reading code.
pub(super) fn worktree_gc_lines(state: Option<&PipelineState>) -> Vec<String> {
    let bold = crate::output::color::bold;
    let mut lines = Vec::new();
    for (_, output) in state
        .into_iter()
        .flat_map(PipelineState::step_output_entries)
    {
        let (Some(_), Some(reports)) = (
            output.get("dry_run").and_then(Value::as_bool),
            output.get("reports").and_then(Value::as_array),
        ) else {
            continue;
        };
        let field = |report: &Value, key: &str| {
            report
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string()
        };
        let kept = reports
            .iter()
            .filter(|report| field(report, "action").starts_with("skipped:"))
            .collect::<Vec<_>>();
        lines.push(format!(
            "{} reports={} kept={} bytes_reclaimed={}",
            bold("Worktree GC:"),
            reports.len(),
            kept.len(),
            output
                .get("bytes_reclaimed")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ));
        for report in kept {
            let mut line = format!(
                "{} {} action={} retained_bytes={} run_id={} task_id={}",
                bold("Kept worktree:"),
                field(report, "path"),
                field(report, "action"),
                report
                    .get("retained_bytes")
                    .and_then(Value::as_u64)
                    .map_or_else(|| "-".to_string(), |bytes| bytes.to_string()),
                field(report, "run_id"),
                field(report, "task_id"),
            );
            if let Some(detail) = report.get("detail").and_then(Value::as_str) {
                line.push_str(&format!(" detail={detail}"));
            }
            lines.push(line);
        }
        for report in reports {
            let Some(recovery) = report.get("recovery") else {
                continue;
            };
            lines.push(format!(
                "{} {} action={} recovery={} location={}",
                bold("Rescued worktree:"),
                field(report, "path"),
                field(report, "action"),
                field(recovery, "source"),
                field(recovery, "location"),
            ));
        }
    }
    lines
}
