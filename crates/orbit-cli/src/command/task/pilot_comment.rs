//! Presentation of persisted pilot receipts, without changing their JSON projection.

use orbit_types::task::TaskComment;

pub(super) fn comment_presentation(comment: &TaskComment) -> String {
    comment
        .pilot_assessment_summary()
        .unwrap_or_else(|| comment.message.clone())
}
