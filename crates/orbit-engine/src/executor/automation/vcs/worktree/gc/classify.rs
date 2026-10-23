use super::super::super::git::git_output;
use super::super::cleanup::remove_worktree;
use super::super::path_is_registered;
use super::leftovers::{is_failed_removal_leftover, reclaim_failed_removal_leftover};
use super::parked::{Parked, parked_commits_survive, parked_hold};
use super::paths::{attributed_task_ids, branch_name, directory_bytes, worktree_lock};
use super::reclaim::collect_build_output;
use super::rescue::{delete_run_branch, rescue_dirty_worktree};
use super::sweep::{
    NOT_REGISTERED_REMEDY, WorktreeGcOptions, WorktreeGcReport, task_status_permits_deletion,
};
use crate::context::{RuntimeHost, WorktreeGcTaskLookup};
use orbit_common::OrbitError;
use orbit_types::task::{TaskStatus, task_id_prefix};
use orbit_types::workflow::JobRun;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// One sweep's task lookups. Results and transport failures are memoized by
/// owner route and task, so a down owner is contacted once per prefix while
/// local and unroutable prefixes still get their own verdict. A missing route
/// is not an outage and is not carried over: another run's claim may name a route.
pub(super) struct SweepTaskLookups<'a, H: RuntimeHost + ?Sized> {
    host: &'a H,
    answers: RefCell<BTreeMap<(String, String), WorktreeGcTaskLookup>>,
    owner_unreachable: RefCell<BTreeMap<String, String>>,
}

impl<'a, H: RuntimeHost + ?Sized> SweepTaskLookups<'a, H> {
    pub(super) fn new(host: &'a H) -> Self {
        Self {
            host,
            answers: RefCell::new(BTreeMap::new()),
            owner_unreachable: RefCell::new(BTreeMap::new()),
        }
    }

    fn lookup(&self, run_id: &str, task_id: &str) -> WorktreeGcTaskLookup {
        let scope = self
            .host
            .worktree_gc_task_lookup_scope(run_id)
            .map(|scope| format!("{scope}/{}", task_id_prefix(task_id).unwrap_or_default()));
        if let Some(scope) = scope.as_ref() {
            let key = (scope.clone(), task_id.to_string());
            if let Some(answer) = self.answers.borrow().get(&key) {
                return answer.clone();
            }
            if let Some(reason) = self.owner_unreachable.borrow().get(scope) {
                return WorktreeGcTaskLookup::OwnerUnreachable(reason.clone());
            }
        }
        let answer = self.host.lookup_task_for_worktree_gc(run_id, task_id);
        if let Some(scope) = scope {
            match &answer {
                WorktreeGcTaskLookup::OwnerUnreachable(reason) => {
                    self.owner_unreachable
                        .borrow_mut()
                        .insert(scope, reason.clone());
                }
                WorktreeGcTaskLookup::NoOwnerRoute(_) => {}
                _ => {
                    self.answers
                        .borrow_mut()
                        .insert((scope, task_id.to_string()), answer.clone());
                }
            }
        }
        answer
    }
}

pub(super) fn classify_known<H: RuntimeHost + ?Sized>(
    repo_root: &Path,
    path: &Path,
    run: &JobRun,
    lookups: &SweepTaskLookups<'_, H>,
    options: &WorktreeGcOptions,
    registered: &BTreeSet<PathBuf>,
) -> Result<WorktreeGcReport, OrbitError> {
    let task_ids = attributed_task_ids(run);
    // A claimed leaf whose claim is settled with its owner needs no task
    // answer: the owner already holds what the leaf delivered.
    let settled_claim = if options.target_only {
        None
    } else {
        lookups.host.settled_claim_for_worktree_gc(&run.run_id)
    };
    // Target-only collection never consults task state, so it never pays a
    // store or owner round trip per task; neither does a settled claim.
    let resolved = if options.target_only || settled_claim.is_some() {
        Vec::new()
    } else {
        task_ids
            .iter()
            .map(|task_id| (task_id.clone(), lookups.lookup(&run.run_id, task_id)))
            .collect::<Vec<(String, WorktreeGcTaskLookup)>>()
    };
    let first_task = resolved.first().and_then(|(_, lookup)| match lookup {
        WorktreeGcTaskLookup::Found {
            status, pr_status, ..
        } => Some((*status, pr_status.clone())),
        _ => None,
    });

    let mut report = WorktreeGcReport {
        // A bundle worktree serves several tasks; name all of them until a
        // single one is identified as the reason it is retained.
        task_id: (!task_ids.is_empty()).then(|| task_ids.join(",")),
        task_status: first_task.as_ref().map(|(status, _)| *status),
        pr_status: first_task.and_then(|(_, pr_status)| pr_status),
        ..WorktreeGcReport::for_run(path, run)
    };

    // Secondary gate: never disturb a worktree that may still back a live
    // process, regardless of what the associated task's status says.
    if !run.state.is_terminal() {
        report.action = "skipped:run_not_terminal".to_string();
        return Ok(report);
    }
    if options
        .older_than
        .is_some_and(|cutoff| run.finished_at.unwrap_or(run.created_at) > cutoff)
    {
        report.action = "skipped:too_recent".to_string();
        return Ok(report);
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to inspect worktree '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        report.action = "skipped:not_a_real_directory".to_string();
        return Ok(report);
    }
    // Git unregisters a worktree before it finishes deleting the directory,
    // so a removal that failed partway leaves a mapped, unregistered
    // directory. That leftover is reclaimed below through the same task
    // gates; any other unregistered directory may be a moved worktree.
    let registered_here = path_is_registered(registered, path);
    if !registered_here && (options.target_only || !is_failed_removal_leftover(path)) {
        report.action = "skipped:not_registered_worktree".to_string();
        report.detail = Some(NOT_REGISTERED_REMEDY.to_string());
        return Ok(report);
    }
    if options.target_only {
        return collect_build_output(path, run, options, report);
    }

    // Primary gate: only a task settled to rejected, archived, or done
    // licenses deletion. A run's process finishing says nothing about
    // whether the work it produced is settled — a run with no associated
    // task, or one whose task can't be resolved, is retained rather than
    // treated as eligible.
    //
    // Bundle rule (ORB-10427): a worktree that serves several tasks is
    // eligible only when *every* task it serves is settled, so a bundle is
    // never easier to discard than its least-settled member. The first
    // member that blocks deletion becomes the reported task.
    //
    // A claimed leaf's settled claim stands in for its task's status: the
    // follower holds no task records, and the owner already has the leaf's
    // delivery whatever the task's status says now.
    //
    // A task parked in `someday` permits deletion only once it has been
    // parked for the configured age, measured from the status change; the
    // parked gates below then apply to the whole worktree.
    if let Some(settlement) = settled_claim {
        report.detail = Some(settlement);
    } else if resolved.is_empty() {
        report.action = "skipped:unattributed".to_string();
        return Ok(report);
    }
    let mut parked: Option<Parked> = None;
    for (task_id, lookup) in resolved {
        let (task_status, pr_status, action, detail) = match lookup {
            WorktreeGcTaskLookup::Found {
                status,
                pr_status,
                status_since,
            } => {
                if task_status_permits_deletion(status) {
                    continue;
                }
                match (status, options.parked_older_than, status_since) {
                    (TaskStatus::Someday, Some(cutoff), Some(since)) if since <= cutoff => {
                        // The most recently parked task names the removal.
                        if parked.as_ref().is_none_or(|held| since > held.since) {
                            parked = Some(Parked { since });
                        }
                        continue;
                    }
                    (TaskStatus::Someday, Some(cutoff), Some(since)) => (
                        Some(status),
                        pr_status,
                        "skipped:parked_too_recent",
                        Some(format!(
                            "parked in someday since {}; reclaimable once parked since {} or earlier",
                            since.to_rfc3339(),
                            cutoff.to_rfc3339()
                        )),
                    ),
                    (TaskStatus::Someday, Some(_), None) => (
                        Some(status),
                        pr_status,
                        "skipped:parked_age_unknown",
                        Some("the task store did not say when the task was parked".to_string()),
                    ),
                    _ => (
                        Some(status),
                        pr_status,
                        "skipped:task_status_ineligible",
                        None,
                    ),
                }
            }
            WorktreeGcTaskLookup::Unresolved => (None, None, "skipped:task_unresolved", None),
            WorktreeGcTaskLookup::TaskPrefixUnroutable => {
                (None, None, "skipped:task_prefix_unroutable", None)
            }
            WorktreeGcTaskLookup::NoOwnerRoute(reason) => {
                (None, None, "skipped:no_owner_route", Some(reason))
            }
            WorktreeGcTaskLookup::OwnerLookupFailed(reason) => {
                (None, None, "skipped:owner_lookup_failed", Some(reason))
            }
            // A replica's task state lives on its owner. Not reaching the
            // owner is not evidence the task is unknown, so say which it was.
            WorktreeGcTaskLookup::OwnerUnreachable(reason) => {
                tracing::warn!(
                    path = %path.display(),
                    run_id = %run.run_id,
                    %task_id,
                    %reason,
                    "worktree GC could not reach the workspace owner to resolve a task; retaining the worktree"
                );
                (None, None, "skipped:owner_unreachable", Some(reason))
            }
        };
        report.task_id = Some(task_id);
        report.task_status = task_status;
        report.pr_status = pr_status;
        report.action = action.to_string();
        report.detail = detail;
        return Ok(report);
    }

    if let Some(parked) = &parked {
        if let Some((action, detail)) = parked_hold(lookups.host, run) {
            report.action = action.to_string();
            report.detail = detail;
            return Ok(report);
        }
        report.detail = Some(parked.detail());
    }

    if !registered_here {
        return reclaim_failed_removal_leftover(path, run, options, report);
    }

    if let Some(reason) = worktree_lock(path)? {
        report.action = "skipped:worktree_locked".to_string();
        report.detail = Some(reason);
        return Ok(report);
    }

    // The safety net: a task can be settled with uncommitted content still
    // sitting in the worktree. Only a sweep that rescues dirty candidates
    // removes one, and only once that content is recoverable elsewhere.
    let dirty = !git_output(path, &["status", "--porcelain", "--untracked-files=all"])?
        .trim()
        .is_empty();
    if dirty && !options.rescue_dirty {
        report.action = "skipped:dirty_rescue_candidate".to_string();
        return Ok(report);
    }

    let branch = match branch_name(path) {
        Ok(branch) => branch,
        Err(_) => {
            report.action = "skipped:branch_unknown".to_string();
            return Ok(report);
        }
    };
    if dirty {
        // A rescue patch pins its base commit, and a published ref holds
        // the whole candidate, so the branch's commits survive either way.
        return rescue_dirty_worktree(repo_root, path, run, &branch, options, report);
    }
    // A clean parked worktree is often a failed run's only copy of the
    // commits it made before publication failed. Its branch goes only when
    // those commits survive elsewhere.
    let mut checked_tip = None;
    if parked.is_some() {
        match parked_commits_survive(path, run, &branch) {
            Ok((tip, recovery)) => {
                checked_tip = Some(tip);
                report.recovery = recovery;
            }
            Err(reason) => {
                report.action = "skipped:unpublished_commits".to_string();
                report.detail = Some(reason);
                return Ok(report);
            }
        }
    }

    let estimated_bytes = if options.delete || options.estimate_bytes {
        directory_bytes(path)?
    } else {
        0
    };
    if !options.delete {
        report.action = "would_remove".to_string();
        // Dry-run skips the recursive walk unless `estimate_bytes` is set;
        // the result's `dry_run` flag says nothing was actually freed.
        report.bytes_reclaimed = estimated_bytes;
        return Ok(report);
    }

    // Deliberately no `--force`: a last-moment dirtying of the worktree makes
    // Git fail closed. Never replace this with raw recursive deletion.
    remove_worktree(repo_root, path, None, false)?;
    report.bytes_reclaimed = estimated_bytes;
    report.action = delete_run_branch(repo_root, &branch, checked_tip.as_deref());
    Ok(report)
}
