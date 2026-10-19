//! Local landed diffs, attributed only through host delivery evidence.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::LandingObservationStatus;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::CodebaseDeliveryScan;

#[derive(Debug, Clone)]
pub(super) struct Landed {
    pub(super) at: DateTime<Utc>,
    pub(super) commit: String,
    pub(super) paths: BTreeSet<String>,
}

/// Reuse the repository, each commit diff and the delivery lookups during one
/// workspace scan.
pub(super) struct Scanner {
    repo: Option<git2::Repository>,
    paths: BTreeMap<String, Option<BTreeSet<String>>>,
    delivery: CodebaseDeliveryScan,
}

impl Scanner {
    pub(super) fn new(runtime: &OrbitRuntime) -> Self {
        Self {
            repo: git2::Repository::open(&runtime.paths().repo_root).ok(),
            paths: BTreeMap::new(),
            delivery: CodebaseDeliveryScan::default(),
        }
    }

    /// The files `commit` changed, diffed at most once per scan. `None`
    /// when the repository, the commit or its objects are missing: never
    /// fetched by a read-only analytics request, and counted separately from
    /// an empty diff. A later fetch can supply them, so the caller keeps no
    /// `None` as final.
    pub(super) fn changed_paths(&mut self, commit: &str) -> Option<BTreeSet<String>> {
        self.paths
            .entry(commit.to_string())
            .or_insert_with(|| changed_paths(self.repo.as_ref()?, commit))
            .clone()
    }

    /// For a done task without a landing receipt, the landed diff its
    /// delivery run observed as merged. Only a window-scoped related scan
    /// asks, so nothing it returns is stored with the task.
    pub(super) fn observe(&mut self, runtime: &OrbitRuntime, id: &str) -> Option<Landed> {
        let delivery = runtime
            .observe_codebase_task_delivery(id, &self.delivery)
            .ok()?;
        if delivery.landing.status != LandingObservationStatus::Merged {
            return None;
        }
        let commit = delivery
            .landing
            .landed_commit
            .or(delivery.commit.head_sha)?;
        let at = delivery.landing.observed_at?;
        let paths = self.changed_paths(&commit)?;
        Some(Landed { at, commit, paths })
    }
}

/// The verified landing receipt in task history, without touching delivery runs
/// or Git. Its timestamp also excludes old receipts from unavailable counts.
pub(super) fn receipt(history: &[TaskHistoryEntry]) -> Option<(String, DateTime<Utc>)> {
    // Owner landings have no local delivery run; their completion event is
    // written only after the owner verifies the provider or target ref.
    history.iter().rev().find_map(|entry| {
        if entry.event != "landing_completed" {
            return None;
        }
        let note: Value = serde_json::from_str(entry.note.as_deref()?).ok()?;
        let commit = note
            .pointer("/delivery/merge_commit")
            .and_then(Value::as_str)
            .or_else(|| {
                (note["delivery"] == "local_candidate"
                    || note["reconciled"] == "local_target_contains_candidate")
                    .then(|| note["candidate"].as_str())
                    .flatten()
            })?;
        Some((commit.to_string(), entry.at))
    })
}

fn changed_paths(repo: &git2::Repository, commit: &str) -> Option<BTreeSet<String>> {
    let _span = tracing::trace_span!(
        target: "orbit.core.codebase",
        "codebase_landed_diff",
        commit = %commit,
    )
    .entered();
    let landed = repo.find_commit(git2::Oid::from_str(commit).ok()?).ok()?;
    let tree = landed.tree().ok()?;
    let parent = if landed.parent_count() == 0 {
        None
    } else {
        Some(landed.parent(0).ok()?.tree().ok()?)
    };
    let diff = repo
        .diff_tree_to_tree(parent.as_ref(), Some(&tree), None)
        .ok()?;
    let paths = diff
        .deltas()
        .flat_map(|delta| {
            [
                delta.old_file().path().map(ToOwned::to_owned),
                delta.new_file().path().map(ToOwned::to_owned),
            ]
        })
        .flatten()
        .map(|path| path.to_str().map(str::to_owned))
        .collect::<Option<BTreeSet<_>>>()?;
    Some(paths)
}
