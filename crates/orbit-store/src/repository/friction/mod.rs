//! SQLite-backed friction records (ORB-10680).
//!
//! Friction reads used to discover every Markdown record, parse every YAML
//! envelope and body, build the whole corpus as a `Vec`, and only then filter,
//! sort, paginate, or aggregate — so peak memory grew with retained history
//! even for a 50-row page. This store pushes the filter, the ordering, the
//! page, and every aggregate into SQLite, so a bounded request costs bounded
//! work.
//!
//! Identity is composite `(workspace_id, friction_id)` (L-0072). Friction IDs
//! stay workspace-local and monthly: the same `F2026-05-001` may exist in two
//! workspaces as two unrelated records, and allocation of the next counter
//! happens inside the same write transaction as the insert.
//!
//! The tag taxonomy stays a small YAML file under `files_root` — moving record
//! persistence does not require moving configuration. `files_root` is also the
//! legacy tree [`import`] reads once per workspace; afterwards it is read-only
//! rollback evidence and no file edit can affect a live read.

use std::path::PathBuf;

use crate::Store;

mod backend;
mod metrics;
mod mutations;
pub(crate) mod queries;
mod stats;
mod store;

pub use crate::contracts::{
    FrictionAddParams, FrictionListFilter, FrictionRehomeOutcome, FrictionRehomeParams,
    FrictionReportedCount, FrictionUpdateParams, StoredFrictionRecord,
};
pub(crate) use store::read_page;

#[cfg(test)]
mod tests;

/// Live friction store for one logical workspace.
///
/// Construction performs no migration. Composition invokes the explicit
/// friction import workflow before returning this live repository.
pub struct FrictionStore {
    store: Store,
    workspace_id: String,
    files_root: PathBuf,
}
