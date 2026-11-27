//! Task-level adapters over the existing authoritative predicates.

mod item;
mod read;
mod subject;

use std::{cell::OnceCell, collections::BTreeMap};

use chrono::{DateTime, Utc};
use orbit_common::governance::attention::filter::AttentionFilter;
use orbit_types::{
    task::Task,
    tool::{AttentionDegraded, AttentionHost, AttentionItem, AttentionWorkspace},
};

use crate::OrbitRuntime;

pub(super) use item::{
    attention_reason, attention_task, finalize_attention_item, workspace_degraded,
};
pub(super) use read::{pending_commit_message, read_attention_sources};
pub(super) use subject::Subject;

/// Shared inputs for sources that inspect one workspace in an attention query.
pub(super) struct AttentionSourceContext<'a> {
    pub(super) runtime: &'a OrbitRuntime,
    pub(super) workspace: &'a AttentionWorkspace,
    pub(super) host: &'a AttentionHost,
    pub(super) filter: &'a AttentionFilter,
    pub(super) now: DateTime<Utc>,
}

pub(super) struct AttentionCandidates {
    pub items: Vec<AttentionItem>,
    pub degraded: Vec<AttentionDegraded>,
    pub timings: BTreeMap<String, u128>,
}

/// The one open-task metadata listing shared by every attention source in
/// a single `attention_query` for one workspace. Loaded lazily on first
/// use, so a query that selects no task-backed source pays no listing, and
/// every later source reuses the first source's result (or its error,
/// rendered by each source in its own degraded shape).
#[derive(Default)]
pub(super) struct SharedOpenTasks {
    cell: OnceCell<Result<Vec<Task>, String>>,
}
