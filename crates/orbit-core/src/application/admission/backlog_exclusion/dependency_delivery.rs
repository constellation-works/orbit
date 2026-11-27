//! Backlog tasks whose done dependency has not reached the delivery base.
//!
//! A dependency reaches `done` before its pull request merges, or by an
//! operator's forced transition. Dispatching its dependent then only fails
//! `worktree_setup`, and the task stays in `backlog`, so the drain would
//! dispatch it again on every pass. The snapshot withholds it until the
//! dependency's commits reach the base, which is the engine's own question.

use orbit_engine::{BaseSyncMode, fetch_remote_base, undelivered_dependency_for_admission};
use orbit_types::task::Task;
use orbit_types::workflow::ShipMode;

use super::{BacklogTaskExclusion, BacklogTaskExclusionReason};
use crate::OrbitRuntime;
use crate::application::contracts::EngineHostPort;

/// Withhold each task of `backlog` whose done dependency has commits only on
/// other refs than the base a `mode` delivery cuts from.
///
/// A remote-tracking base is stale until something fetches it, and a dependency
/// that landed since the last fetch would hold its dependents forever when no
/// other run fetches. With `refresh`, the first withheld task triggers one
/// fetch for the pass and the check is asked again; a pass that only observes
/// never touches the network. A failed fetch leaves the task withheld, as
/// `worktree_setup` would hold it too.
pub(super) fn withhold_undelivered_dependencies(
    runtime: &OrbitRuntime,
    mode: ShipMode,
    refresh: bool,
    backlog: &mut Vec<&Task>,
    excluded: &mut Vec<BacklogTaskExclusion>,
) {
    let host = runtime.engine_host();
    let repo_root = runtime.paths().repo_root.as_path();
    let base = runtime.workspace_base_branch().to_string();
    let sync_mode = match mode {
        ShipMode::Local => BaseSyncMode::Local,
        ShipMode::Pr => BaseSyncMode::Remote,
    };
    let mut fetched = !refresh || sync_mode == BaseSyncMode::Local;
    backlog.retain(|task| {
        if task.dependencies().is_empty() {
            return true;
        }
        let check = || {
            undelivered_dependency_for_admission(&host, repo_root, &task.id, &base, sync_mode)
        };
        let mut undelivered = match check() {
            Ok(undelivered) => undelivered,
            Err(error) => {
                tracing::warn!(task_id = %task.id, "could not read dependency delivery: {error}");
                return true;
            }
        };
        if undelivered.is_some() && !fetched {
            fetched = true;
            let env = runtime.env();
            if let Err(error) = fetch_remote_base(env, repo_root, &base) {
                tracing::warn!(base = %base, "could not refresh the base for dependency delivery: {error}");
            } else if let Ok(rechecked) = check() {
                undelivered = rechecked;
            }
        }
        let Some(undelivered) = undelivered else {
            return true;
        };
        excluded.push(BacklogTaskExclusion {
            id: task.id.clone(),
            reason: BacklogTaskExclusionReason::DependencyNotDelivered,
            conflicts: Vec::new(),
            crew: None,
            detail: Some(undelivered.detail),
        });
        false
    });
}
