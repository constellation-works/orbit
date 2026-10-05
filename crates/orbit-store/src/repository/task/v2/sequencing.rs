use super::*;

pub(crate) fn next_event_id(events: &[TaskEventRowV2]) -> String {
    format!("EV-{:04}", next_sequence(events, "EV-"))
}

pub(crate) trait SequencedRow {
    fn row_id(&self) -> &str;
}

impl SequencedRow for TaskEventRowV2 {
    fn row_id(&self) -> &str {
        &self.event_id
    }
}

impl SequencedRow for TaskCommentRowV2 {
    fn row_id(&self) -> &str {
        &self.comment_id
    }
}

pub(crate) fn next_sequence<T: SequencedRow>(rows: &[T], prefix: &str) -> usize {
    rows.iter()
        .filter_map(|row| row.row_id().strip_prefix(prefix))
        .filter_map(|suffix| suffix.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1
}
