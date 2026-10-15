//! Bounded CI-repair explanations supplement, never replace, host admission.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use orbit_types::identity::lower_hex;
use orbit_types::task::ContextWideningStep;
use orbit_types::workflow::{PrAutoFixBlocker, pr_auto_fix_attempts};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::executor::automation::vcs::RecoveryCommitRefusal;
use crate::executor::automation::vcs::git::git_output;

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
         unexplained edits refuse the repair. The record does not grant authority."
    )
}

fn refused(detail: impl std::fmt::Display) -> RecoveryCommitRefusal {
    RecoveryCommitRefusal::Uncommittable(format!("CI repair scope: {detail}"))
}

/// Require a host admission for this exact run/head, then bind explanations.
/// The provider's report is evidence only; it cannot mint admission or a lease.
pub(super) fn bind(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    failure: &str,
    bound: &mut Value,
) -> Result<(), RecoveryCommitRefusal> {
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
    for part in [".orbit", ".orbit/tmp", relative.as_str()] {
        let metadata = std::fs::symlink_metadata(root.join(part)).map_err(refused)?;
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
            || !orbit_common::fs::selector::ci_repair_path_is_safe(&item.path)
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
