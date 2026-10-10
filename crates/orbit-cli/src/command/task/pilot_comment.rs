//! Presentation of persisted pilot receipts, without changing their JSON projection.

use orbit_types::task::TaskComment;
use serde_json::Value;

pub(super) fn comment_presentation(comment: &TaskComment) -> String {
    assessment_summary(comment).unwrap_or_else(|| comment.message.clone())
}

fn assessment_summary(comment: &TaskComment) -> Option<String> {
    if comment.by != "task-pilot" {
        return None;
    }
    let (header, body) = comment.message.split_once('\n')?;
    let operation = header
        .trim_end_matches('\r')
        .strip_prefix("operation_id=")?;
    if operation.len() != 64 || !operation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let receipt: Value = serde_json::from_str(body).ok()?;
    let assessment = receipt.get("assessment")?;
    let text = |field: &str| {
        assessment
            .get(field)?
            .as_str()
            .filter(|value| !value.trim().is_empty())
    };
    let disposition = text("disposition")?;
    let confidence = text("confidence")?;
    let crew = text("recommended_crew")?;
    let complexity = text("recommended_complexity")?;
    let rationale = text("assessment_rationale")?;
    let change = |before: &str, after: &str| match receipt.get(before).and_then(Value::as_str) {
        Some(before) if !before.is_empty() && before != after => format!("{before} → {after}"),
        _ => after.to_owned(),
    };
    let summary = format!(
        "{disposition} ({confidence}); crew {}; complexity {}; {rationale}",
        change("crew_before", crew),
        change("complexity_before", complexity),
    );
    let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    // Reserve room for the timestamp and author on the full comment line.
    let mut chars = summary.chars();
    let bounded: String = chars.by_ref().take(150).collect();
    Some(if chars.next().is_some() {
        format!("{}…", bounded.chars().take(149).collect::<String>())
    } else {
        bounded
    })
}
