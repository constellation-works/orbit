//! The `updated` history entry a task field write appends, and the task
//! documents its note names.
//!
//! The note is `task fields updated` when only envelope fields changed, and
//! `task fields updated: description, acceptance_criteria` when the write
//! changed body documents, so a reader can tell an edit to what the task asks
//! from run bookkeeping such as the execution summary. Entries written before
//! the note named documents read as naming none.

/// History event of a task field write.
pub const TASK_FIELDS_UPDATED_EVENT: &str = "updated";

const NOTE: &str = "task fields updated";

/// The task description document.
pub const DESCRIPTION_DOCUMENT: &str = "description";
/// The acceptance criteria document.
pub const ACCEPTANCE_CRITERIA_DOCUMENT: &str = "acceptance_criteria";
/// The plan document.
pub const PLAN_DOCUMENT: &str = "plan";
/// The execution summary document.
pub const EXECUTION_SUMMARY_DOCUMENT: &str = "execution_summary";

/// The note of an `updated` entry whose write changed `documents`, in the
/// order given.
pub fn task_fields_updated_note(documents: &[&str]) -> String {
    if documents.is_empty() {
        NOTE.to_string()
    } else {
        format!("{NOTE}: {}", documents.join(", "))
    }
}

/// The documents an `updated` entry's note names; empty for an envelope-only
/// write and for a note written before notes named documents.
pub fn task_fields_updated_documents(note: &str) -> Vec<&str> {
    note.strip_prefix(NOTE)
        .and_then(|rest| rest.strip_prefix(':'))
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|document| !document.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
