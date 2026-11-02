//! Resolve `regression_from` targets' landed commits for commit-message
//! trailers, from recorded delivery or landing evidence only.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_types::task::{Task, TaskHistoryEntry, TaskRelationType, TaskStatus};
use orbit_types::workflow::handoff::EXTERNAL_MERGE_SETTLED_EVENT;

use crate::context::{PrConfig, RuntimeHost};

use super::super::delivery_marker::commits_matching;
use super::super::git::git_output;
use super::message::{RegressionLandedCommit, RegressionLandedCommits};

/// Best-effort: a target that cannot be resolved is simply absent, which
/// omits its `Fixes:` line and never fails the commit.
pub(super) fn resolve_regression_landed_commits<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[Task],
    pr_config: &PrConfig,
) -> RegressionLandedCommits {
    let mut landed = RegressionLandedCommits::new();
    if !pr_config.conventions.regression_trailers {
        return landed;
    }
    let mut targets = BTreeSet::new();
    for task in tasks {
        for relation in &task.relations {
            if relation.relation_type == TaskRelationType::RegressionFrom {
                targets.insert(relation.target.clone());
            }
        }
    }
    for target in targets {
        if let Some(commit) = resolve_one(host, workspace, &target) {
            landed.insert(target, commit);
        }
    }
    landed
}

fn resolve_one<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    target: &str,
) -> Option<RegressionLandedCommit> {
    if let Some(sha) = host
        .get_task_history(target)
        .ok()
        .and_then(|history| landed_commit_from_history(&history))
        && let Some(subject) = commit_subject(workspace, &sha)
    {
        return Some(RegressionLandedCommit { sha, subject });
    }
    let marker = format!("[{target}]");
    let sha = commits_matching(workspace, &marker, &["HEAD"])
        .ok()?
        .into_iter()
        .next()?;
    let subject = commit_subject(workspace, &sha)?;
    Some(RegressionLandedCommit { sha, subject })
}

fn commit_subject(workspace: &Path, sha: &str) -> Option<String> {
    let subject = git_output(workspace, &["show", "-s", "--format=%s", sha]).ok()?;
    let subject = subject.trim().to_string();
    (!subject.is_empty()).then_some(subject)
}

/// The landed commit a target's history records: an owner
/// `landing_completed` note, else the latest transition into `done` whose
/// note records a pull-request merge commit.
fn landed_commit_from_history(history: &[TaskHistoryEntry]) -> Option<String> {
    history.iter().rev().find_map(|entry| {
        if entry.event == "landing_completed" {
            let note: serde_json::Value = serde_json::from_str(entry.note.as_deref()?).ok()?;
            return note
                .pointer("/delivery/merge_commit")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    (note["delivery"] == "local_candidate"
                        || note["reconciled"] == "local_target_contains_candidate")
                        .then(|| note["candidate"].as_str())
                        .flatten()
                })
                .map(ToOwned::to_owned);
        }
        if entry.to_status != Some(TaskStatus::Done) {
            return None;
        }
        let note = entry.note.as_deref()?;
        if entry.event == EXTERNAL_MERGE_SETTLED_EVENT {
            settled_merge_commit(note).map(ToOwned::to_owned)
        } else {
            pull_request_delivery(note).map(ToOwned::to_owned)
        }
    })
}

fn pull_request_delivery(note: &str) -> Option<&str> {
    let (_, delivery) = note.rsplit_once("; delivered by pull request #")?;
    let (_, commit) = delivery.split_once(" merged as ")?;
    let commit = commit.trim();
    (commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(commit)
}

fn settled_merge_commit(note: &str) -> Option<&str> {
    let landed = note.strip_prefix("settled from pull request #")?;
    let (_, merged) = landed.split_once(", merged into ")?;
    let (_, commit) = merged.strip_suffix(" outside Orbit")?.rsplit_once(" as ")?;
    let commit = commit.trim();
    (commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(commit)
}
