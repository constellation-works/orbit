//! Task index: `rows` replaces indexed task rows from bundle envelopes and projects
//! their status, complexity and version reads; `snapshot` retains the freshness row
//! snapshots that scans reuse until a registry commit invalidates them.

mod rows;
mod snapshot;

pub(super) use snapshot::TaskIndexSnapshots;
pub(crate) use snapshot::{TaskIndexProofRow, TaskIndexSnapshot};
