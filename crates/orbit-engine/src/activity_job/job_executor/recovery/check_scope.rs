//! Bounded CI-repair explanations supplement, never replace, host admission.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use orbit_types::identity::lower_hex;
use orbit_types::task::ContextWideningStep;
use orbit_types::workflow::{
    PR_AUTO_FIX_NO_EDIT_RERUN_EVENT, PrAutoFixBlocker, PrAutoFixNoEditRerun, pr_auto_fix_attempts,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::executor::automation::vcs::RecoveryCommitRefusal;
use crate::executor::automation::vcs::git::{git_output, git_output_paths};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairScope {
    schema_version: u32,
    run_id: String,
    failed_head: String,
    cause: String,
    paths: Vec<PathReason>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathReason {
    path: String,
    reason: String,
}

fn report_path(run: &str, step: &str, failure: &str) -> String {
    let key = format!("{run}\0{step}\0{failure}");
    let digest = lower_hex(Sha256::digest(key.as_bytes()));
    format!(".orbit/tmp/ci-repair-{digest}.json")
}

pub(super) fn instruction(run: &str, step: &str, failure: &str) -> String {
    let path = report_path(run, step, failure);
    format!(
        "CI repair scope record: before writing retry, write {path} in the assigned checkout. \
         JSON schema: {{\"schema_version\":1,\"run_id\":\"{run}\",\"failed_head\":\"<current HEAD>\",\"cause\":\"<cause tied to the failed job evidence>\",\"paths\":[{{\"path\":\"<relative changed file>\",\"reason\":\"<why this file is necessary for that failure>\"}}]}}. \
         List every delivery path, including both sides of renames, excluding .orbit/tmp scratch. Original task selectors do \
         not restrict this admitted CI repair, including shared-base defects. No unrelated \
         work, CI workflow or gate changes, metadata or secrets. Orbit checks this record \
         against the admitted head and actual edits before committing; missing, stale or \
         unexplained edits refuse the repair. The record does not grant authority. If the \
         failure is not caused by the candidate (a flake or infrastructure failure), make \
         no edit and write no record: Orbit reruns the failed checks once on the unchanged \
         head instead of committing a repair."
    )
}

fn refused(detail: impl std::fmt::Display) -> RecoveryCommitRefusal {
    RecoveryCommitRefusal::Uncommittable(format!("CI repair scope: {detail}"))
}

/// What binding an admitted check repair decided.
pub(super) enum CheckScopeBind {
    /// The repair explained its edits; commit it before the retry.
    Bound,
    /// The repair made no edit and wrote no scope record. Nothing is
    /// committed; the retry reruns completion on the unchanged head, where
    /// the watcher reruns the failed checks once.
    NoEdit,
}

/// Whether the worktree holds a delivery edit outside `.orbit/` scratch.
///
/// Uses the NUL-delimited porcelain form through [`git_output_paths`]:
/// trimmed line output would eat the first status column of a single-line
/// result (` M x` becomes `M x`), misaligning the path offset and hiding
/// a one-character path. A rename or copy record carries its source as
/// the following NUL field; both sides count as delivery paths.
fn worktree_has_delivery_edit(root: &Path) -> Result<bool, RecoveryCommitRefusal> {
    let fields = git_output_paths(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .map_err(refused)?;
    let is_delivery =
        |path: &str| !path.is_empty() && path != ".orbit" && !path.starts_with(".orbit/");
    let mut fields = fields.iter();
    while let Some(entry) = fields.next() {
        let mut codes = entry.chars();
        let (Some(index_state), Some(_worktree_state)) = (codes.next(), codes.next()) else {
            continue;
        };
        // `XY ` is always three ASCII bytes, so byte 3 is a char boundary.
        if is_delivery(entry.get(3..).unwrap_or_default()) {
            return Ok(true);
        }
        if matches!(index_state, 'R' | 'C')
            && let Some(source) = fields.next()
            && is_delivery(source)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Require a host admission for this exact run/head, then bind explanations.
/// The provider's report is evidence only; it cannot mint admission or a lease.
pub(super) fn bind(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    failure: &str,
    bound: &mut Value,
) -> Result<CheckScopeBind, RecoveryCommitRefusal> {
    if bound
        .pointer("/failed_step_input/auto_fix")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(refused("the failed step has no auto-fix authority"));
    }
    let root = Path::new(
        bound
            .get("workspace_path")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let head = git_output(root, &["rev-parse", "--verify", "HEAD^{commit}"]).map_err(refused)?;
    let ids = bound
        .get("task_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let task = bound
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| ids.iter().filter_map(Value::as_str).min())
        .ok_or_else(|| refused("no owning task"))?;
    let owner_run = bound
        .pointer("/failed_step_input/job_run_id")
        .and_then(Value::as_str)
        .ok_or_else(|| refused("no owning run checkpoint"))?
        .to_string();
    if ctx
        .host
        .get_task(task)
        .map_err(refused)?
        .job_run_id
        .as_deref()
        != Some(owner_run.as_str())
    {
        return Err(refused("task no longer belongs to the failed step's run"));
    }
    let history = ctx.host.get_task_history(task).map_err(refused)?;
    let pr = bound
        .pointer("/failed_step_input/pr_number")
        .and_then(Value::as_str)
        .ok_or_else(|| refused("no PR checkpoint"))?;
    let admitted = pr_auto_fix_attempts(&history, pr)
        .into_iter()
        .find(|attempt| {
            attempt.run_id == ctx.run_id
                && attempt.head_sha == head
                && attempt.blocker == PrAutoFixBlocker::FailedChecks
        })
        .ok_or_else(|| refused("no durable CI repair admission for this run, PR and head"))?;
    let relative = report_path(&ctx.run_id, &step.id, failure);
    // Refuse scratch symlinks as well as a symlink in place of the report.
    // An absent record is a repair outcome, not an OS error: with no edit
    // it hands the head to the one rerun; with edits it refuses in words.
    for part in [".orbit", ".orbit/tmp", relative.as_str()] {
        let metadata = match std::fs::symlink_metadata(root.join(part)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if worktree_has_delivery_edit(root)? {
                    return Err(refused(
                        "the CI repair changed files but wrote no scope record; \
                         no scope record was written, so no edit is explained",
                    ));
                }
                record_no_edit_rerun(
                    ctx,
                    step,
                    failure,
                    NoEditRerunTarget {
                        task,
                        owner_run: &owner_run,
                        pr,
                        head: &head,
                    },
                    &admitted,
                )?;
                return Ok(CheckScopeBind::NoEdit);
            }
            Err(error) => return Err(refused(error)),
        };
        if metadata.file_type().is_symlink() || (part == relative && !metadata.is_file()) {
            return Err(refused("scope record traverses a symlink"));
        }
    }
    let mut bytes = Vec::new();
    let file =
        orbit_common::fs::open_read_only_no_follow(&root.join(&relative)).map_err(refused)?;
    if !file.metadata().map_err(refused)?.is_file() {
        return Err(refused("scope record is not a regular file"));
    }
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(refused)?;
    if bytes.len() > 64 * 1024 {
        return Err(refused("scope record exceeds 64 KiB"));
    }
    let report: RepairScope = serde_json::from_slice(&bytes).map_err(refused)?;
    if report.schema_version != 1
        || report.run_id != ctx.run_id
        || report.failed_head != head
        || report.cause.trim().is_empty()
        || report.paths.is_empty()
    {
        return Err(refused("missing cause/paths or stale run/head"));
    }
    let mut paths = BTreeSet::new();
    for item in &report.paths {
        if item.reason.trim().is_empty()
            || !paths.insert(item.path.clone())
            || !orbit_common::fs::selector::claim_new_path_is_safe(&item.path)
            || item.path.to_ascii_lowercase().starts_with(".github/")
        {
            return Err(refused("invalid, duplicate, protected or unexplained path"));
        }
    }
    let evidence = json!({
        "schema_version": 1, "run_id": ctx.run_id, "owner_run_id": owner_run, "step": step.id,
        "admission": admitted, "failed_head": head,
        "failure": bounded_recovery_text("CI repair failure", &ctx.run_id, failure, MAX_RECOVERY_ERROR_MESSAGE_BYTES),
        "explanation": serde_json::from_slice::<Value>(&bytes).map_err(refused)?,
    });
    // Keep the cause before Git mutation, so an evidence-store failure cannot
    // leave a committed repair without its durable explanation. Publication
    // and validation keep their own subsequent exact-head checkpoints.
    let retained = orbit_common::security::redaction::redact_all_json(evidence.clone());
    ctx.host
        .attach_task_validation_log(
            task,
            &owner_run,
            &format!("auto-fix/{}/scope-{head}.json", ctx.run_id),
            serde_json::to_vec(&retained).map_err(refused)?,
        )
        .map_err(refused)?;
    bound["ci_repair_paths"] = json!(paths);
    bound["ci_repair_evidence"] = evidence;
    Ok(CheckScopeBind::Bound)
}

/// The task, owner run, pull request and failed head a no-edit repair reruns.
struct NoEditRerunTarget<'a> {
    task: &'a str,
    owner_run: &'a str,
    pr: &'a str,
    head: &'a str,
}

/// Record a no-edit repair durably before the retry, so the completion
/// watcher reruns the failed checks once on this head instead of admitting
/// another repair or holding as a duplicate. The admitted attempt already
/// spent one auto-fix attempt for the head.
fn record_no_edit_rerun(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    failure: &str,
    target: NoEditRerunTarget<'_>,
    admitted: &orbit_types::workflow::PrAutoFixAttempt,
) -> Result<(), RecoveryCommitRefusal> {
    let NoEditRerunTarget {
        task,
        owner_run,
        pr,
        head,
    } = target;
    let record = PrAutoFixNoEditRerun {
        pr_number: pr.to_string(),
        head_sha: head.to_string(),
        run_id: ctx.run_id.clone(),
        attempt: admitted.attempt,
    };
    let note = serde_json::to_string(&record).map_err(refused)?;
    let history = ctx.host.get_task_history(task).map_err(refused)?;
    if !history.iter().any(|entry| {
        entry.event == PR_AUTO_FIX_NO_EDIT_RERUN_EVENT && entry.note.as_deref() == Some(&note)
    }) {
        ctx.host
            .apply_task_automation_update(
                task,
                crate::context::TaskAutomationUpdate {
                    append_history: vec![
                        orbit_types::task::TaskHistoryEntry::new(
                            chrono::Utc::now(),
                            "system",
                            PR_AUTO_FIX_NO_EDIT_RERUN_EVENT,
                        )
                        .with_note(Some(note)),
                    ],
                    ..crate::context::TaskAutomationUpdate::default()
                },
            )
            .map_err(refused)?;
    }
    let evidence = json!({
        "schema_version": 1, "run_id": ctx.run_id, "owner_run_id": owner_run, "step": step.id,
        "admission": admitted, "failed_head": head, "outcome": "no_edit_no_scope_record",
        "reason": "the CI repair made no edit and wrote no scope record; the failed checks rerun once on the unchanged head",
        "failure": bounded_recovery_text("CI repair failure", &ctx.run_id, failure, MAX_RECOVERY_ERROR_MESSAGE_BYTES),
    });
    let retained = orbit_common::security::redaction::redact_all_json(evidence);
    ctx.host
        .attach_task_validation_log(
            task,
            owner_run,
            &format!("auto-fix/{}/no-edit-{head}.json", ctx.run_id),
            serde_json::to_vec(&retained).map_err(refused)?,
        )
        .map_err(refused)?;
    Ok(())
}

pub(super) fn record(
    ctx: &ExecCtx<'_>,
    task_ids: &[String],
    evidence: &Value,
    head: &str,
    paths: &[String],
) -> Result<(), RecoveryCommitRefusal> {
    let task = task_ids
        .iter()
        .min()
        .ok_or_else(|| refused("no owning task"))?;
    let owner_run = evidence["owner_run_id"]
        .as_str()
        .ok_or_else(|| refused("missing owning run"))?;
    let mut record = evidence.clone();
    record["repair_head"] = json!(head);
    record["changed_paths"] = json!(paths);
    let record = orbit_common::security::redaction::redact_all_json(record);
    let path = format!("auto-fix/{}/{head}.json", ctx.run_id);
    ctx.host
        .attach_task_validation_log(
            task,
            owner_run,
            &path,
            serde_json::to_vec(&record).map_err(refused)?,
        )
        .map_err(refused)?;
    ctx.host
        .widen_task_context_files(
            task,
            owner_run,
            ContextWideningStep::Recovery,
            STEP_FAILURE_RECOVERY_ACTIVITY,
            paths,
        )
        .map_err(refused)?;
    Ok(())
}
