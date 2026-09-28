//! Does a done dependency's work actually exist in the base this run will be
//! cut from?
//!
//! Lifecycle admission (`admit_task_for_workflow_as_system`, orbit-core) only
//! asks whether a dependency reached `done`. F2026-07-038 showed that is half
//! the question: ORB-10201 was admitted with dependency ORB-10203 done, but
//! its worktree was cut from `agent-main` at `0a7f7676`, which does not
//! contain ORB-10203's commit `76ded2c9` — the dependency branch existed
//! locally and remotely and had simply never merged. The run therefore started
//! from the exact stale content its dependency had already corrected, and
//! nothing in the pipeline noticed.
//!
//! Readiness means both halves: lifecycle completion *and* delivery into the
//! effective base. This module answers the second one from local Git history
//! alone — no GitHub API — so a PR-backed dependency and a locally committed
//! one are verified the same way, and the check works in a repo with no remote
//! at all.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::{DependencyNotDelivered, OrbitError};
use orbit_types::task::{TaskStatus, archived_task_completed_before_archive};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::delivery_marker::commits_matching;

/// How many delivery commits to name per undelivered dependency. Enough to
/// identify the branch to land; the message stays readable.
const MAX_EVIDENCE_COMMITS: usize = 3;

/// Whether `setup_worktree` enforces dependency delivery.
///
/// `Enforce` is the default. `Ignore` exists for the case the marker rule
/// cannot see: a dependency whose work was delivered under some *other*
/// commit message (reworked, folded into a sibling task) while its original
/// branch still exists. Such a dependency would otherwise block every
/// dependent run with no way forward.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::executor::automation) enum DependencyDeliveryMode {
    Enforce,
    Ignore,
}

pub(in crate::executor::automation) fn dependency_delivery_mode_from_input(
    input: &Value,
) -> Result<DependencyDeliveryMode, OrbitError> {
    match input
        .as_object()
        .and_then(|map| map.get("dependency_delivery"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("enforce") => Ok(DependencyDeliveryMode::Enforce),
        Some("ignore") => Ok(DependencyDeliveryMode::Ignore),
        Some(other) => Err(OrbitError::InvalidInput(format!(
            "input.dependency_delivery must be 'enforce' or 'ignore', got '{other}'"
        ))),
    }
}

/// One dependency that lifecycle-completed but whose delivery commits are not
/// reachable from the base.
#[derive(Debug)]
struct UndeliveredDependency {
    task_id: String,
    dependency_id: String,
    commits: Vec<String>,
}

/// Refuse the run when any done dependency of `task_ids` is undelivered into
/// `base_sha`.
///
/// Call this *before* the worktree exists and before admission flips the task
/// to `in-progress`: a refusal should leave no directory, no branch, and no
/// status change behind.
///
/// `base_sha` is the commit pinned by the caller (ADR-0251, L-0113), never a
/// ref name — re-resolving `origin/<base>` here would ask about a base
/// different from the one the worktree is created at. `base_ref` is carried
/// only to name the base in the diagnostic.
pub(in crate::executor::automation) fn ensure_dependencies_delivered_into_base<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    repo_root: &Path,
    task_ids: &[String],
    base_ref: &str,
    base_sha: &str,
) -> Result<(), OrbitError> {
    let in_scope = task_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<&str>>();
    let mut inspected = BTreeSet::new();
    let mut undelivered = Vec::new();

    for task_id in task_ids {
        let task = host.get_task(task_id)?;
        for dependency_id in task.dependencies() {
            // A dependency shipping in this same run delivers itself.
            if in_scope.contains(dependency_id.as_str()) || !inspected.insert(dependency_id.clone())
            {
                continue;
            }
            let dependency = match host.get_task(&dependency_id) {
                Ok(dependency) => dependency,
                // A dangling dependency belongs to the lifecycle gate, not to
                // us: there is no completed work to look for.
                Err(OrbitError::NotFound { .. }) => continue,
                Err(error) => return Err(error),
            };
            // An archived dependency counts as completed when it reached
            // `done` before the archive, exactly as admission counts it.
            let completed = dependency.status.satisfies_dependency()
                || (dependency.status == TaskStatus::Archived
                    && archived_task_completed_before_archive(
                        &host.get_task_history(&dependency_id)?,
                    ));
            if !completed {
                continue;
            }
            if let Some(commits) = undelivered_commits(repo_root, &dependency_id, base_sha)? {
                undelivered.push(UndeliveredDependency {
                    task_id: task_id.clone(),
                    dependency_id,
                    commits,
                });
            }
        }
    }

    match undelivered.first() {
        None => Ok(()),
        // The first offender names the refusal; `detail` carries every one of
        // them, so a bundle blocked by three dependencies reports all three.
        Some(first) => Err(OrbitError::DependencyNotDelivered(Box::new(
            DependencyNotDelivered {
                task_id: first.task_id.clone(),
                dependency_id: first.dependency_id.clone(),
                base_ref: base_ref.to_string(),
                base_sha: base_sha.to_string(),
                detail: undelivered_detail(base_ref, &undelivered),
            },
        ))),
    }
}

/// `Some(commits)` when the repository holds delivery commits for
/// `dependency_id` and none of them reached `base_sha`; `None` when the
/// dependency is delivered, or when the repository holds no commit for it at
/// all.
///
/// The delivery marker is the task id in the commit subject — every Orbit
/// commit message carries `[ORB-…]` (see `vcs::commit::message`), and the
/// marker survives merge, squash, and rebase, so the check is "is *a message
/// match* reachable from the base", not "is a particular sha reachable". The
/// matching rule itself lives in `vcs::delivery_marker`, shared with the
/// base-obsolescence gate.
///
/// No evidence anywhere means no refusal: a dependency completed without a
/// commit in this repository (docs handled elsewhere, a side-effect-only task,
/// work delivered from another workspace) is indistinguishable from one that
/// never needed a commit, and blocking on that would be a guess.
fn undelivered_commits(
    repo_root: &Path,
    dependency_id: &str,
    base_sha: &str,
) -> Result<Option<Vec<String>>, OrbitError> {
    let marker = format!("[{dependency_id}]");
    if !commits_matching(repo_root, &marker, &["--max-count=1", base_sha])?.is_empty() {
        return Ok(None);
    }

    let elsewhere = commits_matching(
        repo_root,
        &marker,
        &[&format!("--max-count={MAX_EVIDENCE_COMMITS}"), "--all"],
    )?;
    Ok((!elsewhere.is_empty()).then_some(elsewhere))
}

fn undelivered_detail(base_ref: &str, undelivered: &[UndeliveredDependency]) -> String {
    let findings = undelivered
        .iter()
        .map(|entry| {
            format!(
                "'{}' (required by '{}') has commit(s) {} on other refs",
                entry.dependency_id,
                entry.task_id,
                entry.commits.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{findings}; merge the dependency work into '{base_ref}' (or wait for its PR to land) and re-dispatch, \
         or pass input.dependency_delivery='ignore' if it was delivered under a different commit"
    )
}
