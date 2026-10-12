//! The idempotency receipt an atomic task mutation leaves on the history
//! event and the comment it writes.
//!
//! A receipt is the typed `operation_id` field. Records written before that
//! field existed carry it in text instead; [`legacy_text_receipt`] is the only
//! reader of that form.

use super::{TaskComment, TaskEventRowV2, TaskHistoryEntry};

/// A comment's receipt: the mutation's operation id and the audit body it
/// sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentReceipt<'a> {
    pub operation_id: &'a str,
    pub body: &'a str,
}

impl TaskHistoryEntry {
    /// Operation id of the task mutation that wrote this event.
    pub fn receipt(&self) -> Option<&str> {
        event_receipt(self.operation_id.as_deref(), self.note.as_deref())
    }
}

impl TaskEventRowV2 {
    /// Operation id of the task mutation that wrote this event.
    pub fn receipt(&self) -> Option<&str> {
        event_receipt(self.operation_id.as_deref(), self.note.as_deref())
    }
}

impl TaskComment {
    /// Operation id and audit body of the task mutation that wrote this
    /// comment. The caller decides which actor it trusts.
    pub fn receipt(&self) -> Option<CommentReceipt<'_>> {
        match typed(self.operation_id.as_deref()) {
            Some(operation_id) => Some(CommentReceipt {
                operation_id,
                body: &self.message,
            }),
            None => legacy_text_receipt(&self.message, false)
                .map(|(operation_id, body)| CommentReceipt { operation_id, body }),
        }
    }
}

fn event_receipt<'a>(operation_id: Option<&'a str>, note: Option<&'a str>) -> Option<&'a str> {
    typed(operation_id).or_else(|| Some(legacy_text_receipt(note?, true)?.0))
}

fn typed(operation_id: Option<&str>) -> Option<&str> {
    operation_id.filter(|id| !id.trim().is_empty())
}

/// Compatibility reader for receipts written before the typed field: an
/// `operation_id=<id>` first line followed by the body, or (event notes
/// only) a ` (operation_id=<id>)` suffix after the summary. Returns the id
/// and the remaining text. Retire once no live task holds a pre-typed
/// receipt (30 days after the typed field shipped, from 2026-10-12).
fn legacy_text_receipt(text: &str, note: bool) -> Option<(&str, &str)> {
    const PREFIX: &str = "operation_id=";
    let (first, body) = text.split_once('\n').unwrap_or((text, ""));
    if let Some(id) = first.trim().strip_prefix(PREFIX) {
        return (!id.is_empty()).then_some((id, body));
    }
    if !note {
        return None;
    }
    let (summary, tail) = text.rsplit_once(&format!(" ({PREFIX}"))?;
    let id = tail.strip_suffix(')')?;
    (!id.is_empty() && !id.contains(char::is_whitespace)).then_some((id, summary))
}
