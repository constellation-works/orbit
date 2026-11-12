//! Release only recovery-added selectors whose stopped candidate no longer needs them.
//!
//! Widening history already distinguishes original scope from repair additions.
//! Admission reconciles that history against the retained candidate and every
//! recorded/open publication; failure to observe any of them retains the lock.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::git::{GIT_REMOTE_TIMEOUT, run_git_within};
use orbit_common::fs::selector::overlaps;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunQuery;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening,
    ContextWideningStep, GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskHistoryEntry,
    names_task_delivery_pull_request,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams, TaskRecords};

const RECONCILED: &str = "recovery_context_reconciled";
const RUN_WINDOW: usize = 128;

/// Reconcile before taking an admitting snapshot. Observational readiness does
/// not call this: it remains a read of the last durable declaration.
pub(super) fn reconcile_recovery_context(
    runtime: &OrbitRuntime,
    tasks: &mut BTreeMap<String, Task>,
) {
    for task in tasks.values_mut().filter(|task| {
        !task.status.is_terminal() && task.job_run_id.is_some() && !task.context_files.is_empty()
    }) {
        match reconcile_task(runtime, task) {
            Ok(Some(updated)) => task.context_files = updated.context_files,
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(task_id = %task.id, %error, "retaining recovery context: observation incomplete")
            }
        }
    }
}

fn reconcile_task(runtime: &OrbitRuntime, metadata: &Task) -> Result<Option<Task>, OrbitError> {
    let history = runtime.get_task_history(&metadata.id)?;
    let mut repair = BTreeMap::new();
    for entry in &history {
        if entry.by == SYSTEM_ACTOR_LABEL && entry.event == CONTEXT_FILES_WIDENED_EVENT {
            let Some(widening) = ContextFilesWidening::from_entry(entry) else {
                return Ok(None);
            };
            for selector in widening.selectors {
                if widening.step == ContextWideningStep::Recovery
                    && widening.activity == "step_failure_recovery"
                {
                    repair.insert(selector, (widening.run_id.clone(), entry.at));
                } else {
                    repair.remove(&selector);
                }
            }
        } else if entry.by == SYSTEM_ACTOR_LABEL && entry.event == RECONCILED {
            let note: Value = serde_json::from_str(entry.note.as_deref().unwrap_or_default())
                .map_err(observation_error)?;
            for selector in note["selectors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                repair.remove(selector);
            }
        }
    }
    repair.retain(|selector, _| metadata.context_files.contains(selector));
    if repair.is_empty() {
        return Ok(None);
    }
    let task = runtime.get_task(&metadata.id)?;
    if task.updated_at != metadata.updated_at
        || task.status.is_terminal()
        || !runtime.task_run_is_local(&task)
        || runtime.live_task_delivery(&task).is_some()
    {
        return Ok(None);
    }
    // A terminal row alone does not prove its worker has stopped. Include
    // retries whose binding still points to the original run, and fail closed
    // when the bounded scan cannot establish the complete lineage.
    let runs = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            // A drain/gate/pilot may mention the task without owning its
            // candidate. In particular the admitting drain is still live.
            // CI recovery here belongs to the owner-local PR delivery job;
            // live/kept execution claims are fenced separately above.
            job_id: Some(orbit_types::workflow::TASK_PR_PIPELINE_JOB.into()),
            task_id: Some(task.id.clone()),
            limit: Some(RUN_WINDOW),
            include_steps: false,
            ..Default::default()
        })?;
    if runs.len() == RUN_WINDOW
        || runs.iter().any(|run| {
            !run.state.is_terminal()
                || run.finished_at.is_none()
                || run_owner_liveness(run) != RunOwnerLiveness::Stopped
        })
    {
        return Ok(None);
    }
    let Some(current) = task
        .job_run_id
        .as_ref()
        .and_then(|id| runs.iter().find(|run| &run.run_id == id))
    else {
        return Ok(None);
    };
    if repair
        .values()
        .any(|(id, _)| !runs.iter().any(|run| &run.run_id == id))
    {
        return Ok(None);
    }
    let Some(state) = runtime.read_run_state(&current.run_id)? else {
        return Ok(None);
    };
    if state.run_id != current.run_id || state.job_id != current.job_id {
        return Ok(None);
    }
    let Some(base) = state
        .pipeline
        .get("worktree")
        .and_then(|value| value.get("base_sha"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let discarded = history
        .iter()
        .filter(|entry| entry.event == CANDIDATE_DISCARDED_EVENT)
        .map(|entry| entry.at)
        .max();
    let all_discarded = repair
        .values()
        .all(|(_, at)| discarded.is_some_and(|discard| discard > *at));
    if !all_discarded && runtime.kept_claim_candidate(&task.id)?.is_some() {
        return Ok(None);
    }
    let mut protected = BTreeSet::new();
    let mut observations = Vec::new();
    let repo = git2::Repository::open(&runtime.paths().repo_root).map_err(observation_error)?;
    let mut numbers = task
        .external_refs
        .iter()
        .filter(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
        .map(|reference| reference.id.clone())
        .collect::<BTreeSet<_>>();
    let mut publication_recorded = !numbers.is_empty();
    let mut pushed_branches = BTreeSet::new();
    for run in &runs {
        if let Some(state) = runtime.read_run_state(&run.run_id)? {
            if state.run_id != run.run_id || state.job_id != run.job_id {
                return Ok(None);
            }
            publication_recorded |= ["push", "pr_open"].iter().any(|key| {
                state
                    .pipeline
                    .get(*key)
                    .is_some_and(|value| !value.is_null())
            });
            for output in state
                .pipeline
                .as_object()
                .ok_or_else(|| observation_error("unreadable pipeline"))?
                .values()
                .filter(|output| output["phase"] == "push")
            {
                let Some(branch) = output["branch"].as_str() else {
                    return Ok(None);
                };
                pushed_branches.insert(branch.to_string());
            }
            if let Some(receipt) = &state.failure_activity_checkpoint {
                if let Some(number) = receipt.output.get("pr_number").and_then(Value::as_str) {
                    numbers.insert(number.to_string());
                    publication_recorded = true;
                    if let Some(branch) = receipt.output.get("branch").and_then(Value::as_str) {
                        pushed_branches.insert(branch.to_string());
                    }
                } else if receipt.output["decision"] != "no_candidate" {
                    return Ok(None);
                }
            }
        } else if run.started_at.is_some() {
            return Ok(None);
        }
    }
    // A lost push/PR reply may have left no checkpoint or task reference.
    // The complete forge listing is therefore required even before publication.
    for pr in runtime
        .task_pr_forge()
        .open_pull_requests(&runtime.paths().repo_root)?
    {
        if names_task_delivery_pull_request(
            &runtime
                .context
                .settings()
                .pr_settings()
                .conventions
                .branch_name,
            &pr.head_branch,
            &pr.body,
            &task.id,
        ) {
            numbers.insert(pr.number.to_string());
        }
    }
    if publication_recorded && numbers.is_empty() {
        return Ok(None);
    }
    // Use the configured forge identity; libgit2's remote URL may already
    // apply an insteadOf transport rewrite (as in the isolated Git fixtures).
    let remote_url = repo
        .config()
        .and_then(|config| config.get_string("remote.origin.url"))
        .map_err(observation_error)?;
    let remote_identity =
        orbit_types::workspace::git_remote_identity(&remote_url).map_err(observation_error)?;
    if !remote_identity.starts_with("github.com/") {
        return Ok(None);
    }
    let mut observed_branches = BTreeSet::new();
    let mut open_publication = false;
    for number in numbers {
        let status = runtime.read_pull_request_status(&number)?;
        if status.number.map(|value| value.to_string()).as_deref() != Some(number.as_str())
            || status.url.as_deref()
                != Some(format!("https://{remote_identity}/pull/{number}").as_str())
        {
            return Ok(None);
        }
        let Some(branch) = status.head_branch.as_deref() else {
            return Ok(None);
        };
        if !git2::Reference::is_valid_name(&format!("refs/heads/{branch}")) {
            return Ok(None);
        }
        observed_branches.insert(branch.to_string());
        match status.state.as_deref() {
            Some("CLOSED" | "MERGED") => {
                observations.push(json!({"pr": number, "state": status.state}));
                continue;
            }
            Some("OPEN")
                if status.merge_absence_confirmed() && status.auto_merge_enabled == Some(false) => {
            }
            _ => return Ok(None),
        }
        let (Some(branch), Some(head)) =
            (status.head_branch.as_deref(), status.head_sha.as_deref())
        else {
            return Ok(None);
        };
        let remote = run_git_within(
            &runtime.paths().repo_root,
            &[
                "ls-remote",
                "--exit-code",
                "origin",
                &format!("refs/heads/{branch}"),
            ],
            GIT_REMOTE_TIMEOUT,
        )?;
        if !remote.success || remote.stdout.split_whitespace().next() != Some(head) {
            return Ok(None);
        }
        protected.extend(candidate_paths(&repo, base, head).map_err(observation_error)?);
        observations.push(json!({"pr": number, "head": head, "base": base}));
        open_publication = true;
    }
    if !pushed_branches.is_subset(&observed_branches) {
        return Ok(None);
    }
    // A verified replacement publication supersedes the old committed tip.
    // Uncommitted work still protects paths unless explicitly discarded.
    let mut found_local = false;
    if !all_discarded {
        let mut candidates = orbit_engine::run_worktree_paths(&runtime.paths().repo_root, current)?;
        // Source-repair adoption can continue in a checkout that predates
        // this run; prefer neither spelling over the host's actual checkpoint.
        if let Some(path) = state.pipeline["worktree"]["workspace_path"].as_str() {
            candidates.push(path.into());
        }
        candidates.sort();
        candidates.dedup();
        for path in candidates {
            if !path.try_exists().map_err(observation_error)? {
                continue;
            }
            let candidate = git2::Repository::open(&path).map_err(observation_error)?;
            if candidate
                .workdir()
                .and_then(|root| root.canonicalize().ok())
                != path.canonicalize().ok()
                || candidate
                    .commondir()
                    .canonicalize()
                    .map_err(observation_error)?
                    != repo.commondir().canonicalize().map_err(observation_error)?
            {
                return Ok(None);
            }
            protected.extend(uncommitted_paths(&candidate).map_err(observation_error)?);
            if !open_publication {
                let head = candidate
                    .head()
                    .and_then(|head| head.peel_to_commit())
                    .map_err(observation_error)?
                    .id()
                    .to_string();
                protected
                    .extend(candidate_paths(&candidate, base, &head).map_err(observation_error)?);
                observations.push(json!({"worktree": path, "head": head, "base": base}));
            }
            found_local = true;
        }
        if !open_publication && !found_local {
            return Ok(None);
        }
    }
    let released = repair
        .keys()
        .filter(|selector| {
            !protected
                .iter()
                .any(|path| overlaps(selector, &format!("file:{path}")))
        })
        .cloned()
        .collect::<Vec<_>>();
    if released.is_empty() {
        return Ok(None);
    }
    let note =
        json!({"selectors": released, "repair_runs": repair.values().map(|(run, _)| run).collect::<BTreeSet<_>>(), "observations": observations, "discarded": all_discarded})
            .to_string();
    let mut updated = None;
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(&task.id, &mut || {
            let fresh = runtime.get_task(&task.id)?;
            if fresh.updated_at != task.updated_at
                || fresh.status.is_terminal()
                || runtime.live_task_delivery(&fresh).is_some()
            {
                return Ok(());
            }
            runtime.ensure_coordination_task_write_permitted()?;
            let context_files = fresh
                .context_files
                .into_iter()
                .filter(|selector| !released.contains(selector))
                .collect();
            updated = Some(runtime.with_mutation(|| {
                let updated = runtime.stores().task_records().update(
                    &task.id,
                    TaskRecordUpdateParams {
                        actor: SYSTEM_ACTOR_LABEL.into(),
                        context_files: Some(context_files),
                        append_history: vec![TaskHistoryEntry {
                            at: Utc::now(),
                            by: SYSTEM_ACTOR_LABEL.into(),
                            event: RECONCILED.into(),
                            note: Some(note.clone()),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )?;
                Ok((
                    updated,
                    OrbitEvent::TaskUpdated {
                        id: task.id.clone(),
                    },
                ))
            })?);
            Ok(())
        })?;
    Ok(updated)
}

fn candidate_paths(
    repo: &git2::Repository,
    base: &str,
    head: &str,
) -> Result<BTreeSet<String>, git2::Error> {
    if base.len() != 40 || head.len() != 40 {
        return Err(git2::Error::from_str(
            "candidate/base identity is not an exact commit",
        ));
    }
    let base = repo.find_commit(git2::Oid::from_str(base)?)?;
    let head = repo.find_commit(git2::Oid::from_str(head)?)?;
    let ancestor = repo.merge_base(base.id(), head.id())?;
    let base_tree = repo.find_commit(ancestor)?.tree()?;
    let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head.tree()?), None)?;
    Ok(diff
        .deltas()
        .flat_map(|delta| {
            [
                delta.old_file().path().map(Path::to_path_buf),
                delta.new_file().path().map(Path::to_path_buf),
            ]
        })
        .flatten()
        .map(|path| path.to_string_lossy().into_owned())
        .collect())
}

fn uncommitted_paths(repo: &git2::Repository) -> Result<BTreeSet<String>, git2::Error> {
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .update_index(false);
    let statuses = repo.statuses(Some(&mut options))?;
    Ok(statuses
        .iter()
        .flat_map(|entry| {
            [entry.head_to_index(), entry.index_to_workdir()]
                .into_iter()
                .flatten()
                .flat_map(|delta| {
                    [
                        delta.old_file().path().map(Path::to_path_buf),
                        delta.new_file().path().map(Path::to_path_buf),
                    ]
                })
                .flatten()
                .collect::<Vec<_>>()
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect())
}

fn observation_error(error: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("recovery context observation: {error}"))
}
