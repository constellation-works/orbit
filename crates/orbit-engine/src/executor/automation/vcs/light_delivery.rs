//! Light delivery [ORB-16855]: whether a candidate keeps its tasks'
//! `skip-review` or `skip-validation` tag.
//!
//! The allow-list is the host's `workflow.light_delivery_paths`. A claimed
//! leaf reads the owner's captured list from its contract, so the leaf and
//! its owner judge one candidate alike; an owner too old to capture one, or a
//! replica host, has no list, so no candidate is light and both tags are
//! ignored. Each decision is recorded once per candidate in every delivered
//! task's history; a claimed leaf cannot write history, so its owner records
//! the decision when it accepts the handoff.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::path_filter::PathFilter;
use orbit_types::task::{
    GuardOutcome, LIGHT_DELIVERY_PATHS_KEY, LightDeliveryDecision, LightDeliveryJudgement,
    ReviewSkipProfile, TaskHistoryEntry,
};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::baseline::in_detached_worktree;
use super::git::{git_output_paths, git_run_bytes};
use super::required::command::run_required_command;
use super::scratch_target::{ScratchHostTarget, lease_scratch_host_target};
use super::worktree::scratch_checkout_path;

/// Ceiling for one review-skip guard run.
const GUARD_TIMEOUT_MS: u64 = 5 * 60 * 1000;
/// Name prefix of a guard's candidate checkout under the managed worktree root.
const GUARD_WORKTREE_DIR: &str = "orbit-review-skip-guard";

/// Judge the paths `head` changes against `base` in `workspace` (deletions
/// included, renames as a deletion and an addition) against `allow`.
///
/// # Errors
/// Returns an error when `allow` holds an invalid glob or Git cannot diff the
/// two commits.
pub fn judge_light_delivery(
    workspace: &Path,
    base: &str,
    head: &str,
    allow: &[String],
) -> Result<LightDeliveryJudgement, OrbitError> {
    judge_light_delivery_with_key(workspace, base, head, allow, LIGHT_DELIVERY_PATHS_KEY)
}

/// [`judge_light_delivery`] against `allow`, naming `key` in a refusal.
///
/// # Errors
/// Returns an error when `allow` holds an invalid glob or Git cannot diff the
/// two commits.
pub fn judge_light_delivery_with_key(
    workspace: &Path,
    base: &str,
    head: &str,
    allow: &[String],
    key: &str,
) -> Result<LightDeliveryJudgement, OrbitError> {
    let filter = PathFilter::new(key, allow)?;
    let changed = git_output_paths(
        workspace,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            "--end-of-options",
            base,
            head,
            "--",
        ],
    )?;
    Ok(LightDeliveryJudgement::new(
        changed.len(),
        &filter.outside(&changed),
    ))
}

/// The decision for `tag` over `task_ids` and the candidate `head` on
/// `base`, or `None` when no delivered task carries the tag.
pub(super) fn light_delivery_decision<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    base: &str,
    head: &str,
    task_ids: &[String],
    tag: &str,
) -> Result<Option<LightDeliveryDecision>, OrbitError> {
    let tasks = task_ids
        .iter()
        .map(|task_id| host.get_task(task_id).map(|task| (task_id, task.tags)))
        .collect::<Result<Vec<_>, _>>()?;
    let tagged = tasks
        .iter()
        .map(|(task_id, tags)| (task_id.as_str(), tags.as_slice()))
        .collect::<Vec<_>>();
    if !tagged
        .iter()
        .any(|(_, tags)| tags.iter().any(|carried| carried == tag))
    {
        return Ok(None);
    }
    let judgement = judge_light_delivery(
        workspace,
        base,
        head,
        host.validation_gates().light_delivery_paths(),
    )?;
    Ok(LightDeliveryDecision::decide(tag, &tagged, head, judgement))
}

/// The decision for a mapped review-skip `tag` with `profile` over
/// `task_ids` and the candidate `head` on `base`, or `None` when no
/// delivered task carries the tag. The guard, when set, runs only when
/// every task carries the tag and every changed path matches `profile.paths`.
///
/// # Errors
/// Returns an error when a task cannot be read or Git cannot diff.
pub fn review_skip_decision<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    base: &str,
    head: &str,
    task_ids: &[String],
    tag: &str,
    profile: &ReviewSkipProfile,
) -> Result<Option<LightDeliveryDecision>, OrbitError> {
    let tasks = task_ids
        .iter()
        .map(|task_id| host.get_task(task_id).map(|task| (task_id, task.tags)))
        .collect::<Result<Vec<_>, _>>()?;
    let tagged = tasks
        .iter()
        .map(|(task_id, tags)| (task_id.as_str(), tags.as_slice()))
        .collect::<Vec<_>>();
    if !tagged
        .iter()
        .any(|(_, tags)| tags.iter().any(|carried| carried == tag))
    {
        return Ok(None);
    }
    let profile_key = ReviewSkipProfile::paths_key(tag);
    let judgement =
        judge_light_delivery_with_key(workspace, base, head, &profile.paths, &profile_key)?;
    let all_tagged = tagged
        .iter()
        .all(|(_, tags)| tags.iter().any(|carried| carried == tag));
    let guard = match &profile.guard {
        None => GuardOutcome::NotConfigured,
        Some(_) if !all_tagged || !judgement.is_light() => GuardOutcome::NotRun,
        Some(guard) => run_review_skip_guard(host, workspace, base, head, tag, guard)?,
    };
    Ok(LightDeliveryDecision::decide_profile(
        tag,
        &tagged,
        head,
        judgement,
        &profile_key,
        guard,
    ))
}

/// Run `guard` as read from `base` (never the worktree) the way a required
/// validation command runs: same validation environment and sandbox rules,
/// in a fresh detached checkout of `head` taken from `workspace` (so the
/// guard sees the judged candidate's `HEAD` and files, whatever `workspace`
/// has checked out, and leaves its branch and edits alone), with `base` and
/// `head` as its first two arguments and as `ORBIT_REVIEW_SKIP_BASE` /
/// `ORBIT_REVIEW_SKIP_HEAD` (the tag as `ORBIT_REVIEW_SKIP_TAG`). Exit 0
/// permits the skip.
///
/// # Errors
/// Returns an error only when the guard cannot be materialized outside
/// the checkout or `head` cannot be checked out; a missing or failing
/// guard is a [`GuardOutcome`].
pub fn run_review_skip_guard<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    base: &str,
    head: &str,
    tag: &str,
    guard: &str,
) -> Result<GuardOutcome, OrbitError> {
    let spec = format!("{base}:{guard}");
    let outcome = git_run_bytes(
        workspace,
        &["cat-file", "-p", "--end-of-options", &spec],
        None,
    )?;
    if !outcome.success {
        return Ok(GuardOutcome::MissingAtBase {
            guard: guard.to_string(),
        });
    }
    let dir = tempfile::Builder::new()
        .prefix("orbit-review-skip-guard-")
        .tempdir()
        .map_err(|error| {
            OrbitError::Execution(format!("create the review-skip guard directory: {error}"))
        })?;
    let file_name = Path::new(guard)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "guard".to_string());
    let path = dir.path().join(file_name);
    orbit_common::fs::io::atomic_write_bytes(&path, &outcome.stdout).map_err(|error| {
        OrbitError::Execution(format!("write the review-skip guard from base: {error}"))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                OrbitError::Execution(format!("make the review-skip guard executable: {error}"))
            },
        )?;
    }
    let command = format!(
        "ORBIT_REVIEW_SKIP_BASE={} ORBIT_REVIEW_SKIP_HEAD={} ORBIT_REVIEW_SKIP_TAG={} {} {} {}",
        shell_quote(base),
        shell_quote(head),
        shell_quote(tag),
        shell_quote(&path.to_string_lossy()),
        shell_quote(base),
        shell_quote(head),
    );
    let key = orbit_common::security::release::sha256_hex(
        format!("{}\0{head}\0{tag}", std::process::id()).as_bytes(),
    );
    let worktree =
        scratch_checkout_path(workspace, &format!("{GUARD_WORKTREE_DIR}-{}", &key[..16]))?;
    let run = in_detached_worktree(workspace, &worktree, head, |checkout| {
        // Held until the run ends, so worktree GC cannot reclaim the host
        // target underneath it and can once the checkout is gone.
        let _lease = if host
            .validation_subprocess_environment()
            .build_output_env
            .is_empty()
        {
            None
        } else {
            Some(lease_scratch_host_target(
                checkout,
                ScratchHostTarget::HostEvidence,
            )?)
        };
        run_required_command(host, checkout, &command, None, GUARD_TIMEOUT_MS)
    })??;
    if run.passed {
        Ok(GuardOutcome::Passed)
    } else if run.timed_out {
        Ok(GuardOutcome::Failed {
            detail: format!("guard `{guard}` timed out"),
        })
    } else {
        Ok(GuardOutcome::Failed {
            detail: format!("guard `{guard}` exited {}", run.exit_code),
        })
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Record `decision` in each delivered task's history unless the task's
/// latest entry already records it, so a retried step records it once.
pub(super) fn record_light_delivery<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    decision: &LightDeliveryDecision,
) -> Result<(), OrbitError> {
    if host.worker_invocation().is_some() {
        return Ok(());
    }
    let event = decision.event();
    let note = decision.note();
    for task_id in task_ids {
        let recorded = host
            .get_task_history(task_id)?
            .iter()
            .rev()
            .any(|entry| entry.event == event && entry.note.as_deref() == Some(note.as_str()));
        if recorded {
            continue;
        }
        host.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: event.to_string(),
                    note: Some(note.clone()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}
