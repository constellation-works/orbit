//! The one-line summary of a task-pilot assessment comment.

use serde_json::Value;

use super::TaskComment;

/// Author label the task pilot writes its assessment comments under.
const PILOT_AUTHOR: &str = "task-pilot";

/// Longest summary, in characters, leaving room for the timestamp and author
/// on the full comment line.
const SUMMARY_MAX_CHARS: usize = 150;

impl TaskComment {
    /// Whether the task pilot wrote this comment.
    pub fn is_task_pilot_comment(&self) -> bool {
        self.by == PILOT_AUTHOR
    }

    /// The one-line summary of a task-pilot assessment receipt, or `None`
    /// when this is not a well-formed pilot assessment (another author, no
    /// receipt, or a body that is not the expected JSON). Callers fall back to
    /// the stored message.
    pub fn pilot_assessment_summary(&self) -> Option<String> {
        if !self.is_task_pilot_comment() {
            return None;
        }
        let receipt = self.receipt()?;
        let operation = receipt.operation_id;
        if operation.len() != 64 || !operation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let receipt: Value = serde_json::from_str(receipt.body).ok()?;
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
            Some(before) if !before.is_empty() && before != after => {
                format!("{before} → {after}")
            }
            _ => after.to_owned(),
        };
        let summary = format!(
            "{disposition} ({confidence}); crew {}; complexity {}; {rationale}",
            change("crew_before", crew),
            change("complexity_before", complexity),
        );
        let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut chars = summary.chars();
        let bounded: String = chars.by_ref().take(SUMMARY_MAX_CHARS).collect();
        Some(if chars.next().is_some() {
            format!(
                "{}…",
                bounded
                    .chars()
                    .take(SUMMARY_MAX_CHARS - 1)
                    .collect::<String>()
            )
        } else {
            bounded
        })
    }
}
