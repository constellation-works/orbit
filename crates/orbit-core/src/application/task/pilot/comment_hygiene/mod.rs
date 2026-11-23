//! Deterministic pre-admission batch preparation for `comment-hygiene` tasks
//! [ORB-26787].
//!
//! Reconciles the repository-wide tracked-file inventory, standard exclusions,
//! carried attempt ledgers, unchanged-file drops, open-task footprints,
//! repeat-skip holds, git commit dates and line/file budgets at the pinned
//! `source_revision` inside `prepare_task_pilot`. Missing, unreadable or
//! malformed required ledger or footprint evidence produces an honest `hold`
//! with empty selectors and empty ranked candidates — never provisional
//! selectors — and `apply_task_pilot_results` enforces that contract.

mod apply;
mod exclusions;
mod git;
mod ledger;
mod prepare;

use serde_json::{Value, json};

pub(super) use apply::{attach_comment_hygiene_hold, validate_comment_hygiene_selectors};
pub(super) use prepare::{
    comment_hygiene_batch_holds, is_comment_hygiene_batch_task, prepare_comment_hygiene_batch,
};

/// Tag marking `comment-hygiene` auto-task instances.
const COMMENT_HYGIENE_TAG: &str = "comment-hygiene";
/// Tag marking dedicated single-file escalation tasks (not rolling batch runs).
const COMMENT_HYGIENE_ESCALATION_TAG: &str = "comment-hygiene-escalation";
/// Maximum files selected per comment-hygiene batch.
const MAX_BATCH_FILES: usize = 6;
/// Target line budget per comment-hygiene batch.
const LINE_BUDGET: usize = 1500;
/// Files strictly larger than this line count are taken alone (or by ranges).
const OVERSIZED_LINE_THRESHOLD: usize = 800;
/// Maximum ranked eligible candidates emitted for bounded repo-guide fallthrough.
const MAX_RANKED_CANDIDATES: usize = 32;
/// Cap on itemized exclusion detail rows in the prepared evidence payload.
const MAX_REPORTED_EXCLUSIONS: usize = 50;
/// Number of blobs inspected per `git cat-file --batch` invocation.
const BLOB_BATCH_SIZE: usize = 32;

#[derive(Debug, Clone)]
struct HoldReason {
    code: &'static str,
    source: String,
    detail: String,
}

impl HoldReason {
    fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "source": self.source,
            "detail": self.detail,
        })
    }
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}
