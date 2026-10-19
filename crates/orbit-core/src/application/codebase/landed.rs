//! Local landed diffs, attributed only through host delivery evidence.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::LandingObservationStatus;
use serde_json::Value;

use crate::OrbitRuntime;

#[derive(Debug, Clone)]
pub(super) struct Landed {
    pub(super) at: DateTime<Utc>,
    pub(super) commit: String,
    pub(super) paths: BTreeSet<String>,
}

/// Reuse the repository and each commit diff during one workspace scan.
pub(super) struct Scanner {
    repo: Option<git2::Repository>,
    paths: BTreeMap<String, Option<BTreeSet<String>>>,
}

impl Scanner {
    pub(super) fn new(runtime: &OrbitRuntime) -> Self {
        Self {
            repo: git2::Repository::open(&runtime.paths().repo_root).ok(),
            paths: BTreeMap::new(),
        }
    }

    pub(super) fn read(
        &mut self,
        runtime: &OrbitRuntime,
        id: &str,
        history: &[TaskHistoryEntry],
    ) -> Option<Landed> {
        // Owner landings have no local delivery run; their completion event is
        // written only after the owner verifies the provider or target ref.
        let owner = history.iter().rev().find_map(|entry| {
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
        });
        let (commit, at) = owner.or_else(|| {
            let delivery = runtime.observe_task_delivery(id, None).ok()?;
            if delivery.landing.status != LandingObservationStatus::Merged {
                return None;
            }
            Some((
                delivery
                    .landing
                    .landed_commit
                    .or(delivery.commit.head_sha)?,
                delivery.landing.observed_at?,
            ))
        })?;
        // Never fetch from a read-only analytics request. Missing/pruned objects
        // remain unavailable and are counted separately from empty diffs.
        let paths = self
            .paths
            .entry(commit.clone())
            .or_insert_with(|| changed_paths(self.repo.as_ref()?, &commit))
            .clone()?;
        Some(Landed { at, commit, paths })
    }
}

fn changed_paths(repo: &git2::Repository, commit: &str) -> Option<BTreeSet<String>> {
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
