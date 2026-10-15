//! Authenticated, bounded replacement of a stale delivery preparation.
//!
//! A preparation is immutable. Only this host boundary observes a new base,
//! issues a different certificate, and retries with materially different inputs.
use super::super::super::input::required_input_string;
use super::super::completion_deadline::now_ms;
use super::super::git::{GitDeadlineGuard, git_command_success, git_output};
use super::super::handoff::{
    FailedHandoffPhase, HandoffContext, load_handoff_context, rebase_in_progress,
    record_failed_handoff,
};
use super::checkpoint::{recorded_workspace_matches, recovery_run_id};
use super::divergence::{commit_sha, remote_branch_sha};
use super::rebase::prepare_pr_handoff_inner;
use super::rebase_recovery::{RecoveredRewrite, ensure_clean_for_rewrite, recovered_rewrite};
use crate::context::{RebaseRecoveryAttemptScope, RuntimeHost};
use orbit_common::OrbitError;
use serde_json::{Value, json};

const PREPARE: &str = "prepare_branch";
const REFRESH: &str = "sync_base_preparation";
const MAX_REFRESHES: usize = 2;
const REFRESH_BUDGET_MS: u64 = 120_000;
const FIELDS: &[&str] = &[
    "head",
    "head_sha",
    "base",
    "base_ref",
    "base_sha",
    "remote_sha",
    "commits_behind",
    "sync_required",
];

// The complete prepare output is carried so a pre-certificate persisted run
// still renders its sync input. Without a certificate it retains the ordinary
// pinned refusal; missing evidence never authorizes automatic refresh.
fn preparation(input: &Value) -> Option<&Value> {
    input
        .get("preparation")?
        .get("preparation")
        .filter(|value| !value.is_null())
}

fn refusal(reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("prepared_base_refresh_refused: {reason}"))
}

/// Sign the initial preparation separately from rewrite recovery. The whole
/// payload is bound by the existing host authority; no new trust store exists.
pub(super) fn certify_preparation<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    mut prepared: Value,
) -> Result<Value, OrbitError> {
    validate_owner(host, recovery_run_id(input, context), context)?;
    prepared["kind"] = json!("delivery_preparation");
    prepared["base_sync"] = input
        .get("base_sync")
        .cloned()
        .unwrap_or_else(|| json!("remote"));
    prepared["task_ids"] = json!(
        context
            .tasks
            .iter()
            .map(|task| &task.id)
            .collect::<Vec<_>>()
    );
    prepared["workspace_path"] = json!(context.workspace_path);
    prepared["run_id"] = json!(recovery_run_id(input, context));
    prepared["head_sha_before"] = prepared["head_sha"].clone();
    prepared["target_base_sha"] = prepared["base_sha"].clone();
    persist(host, context, PREPARE, prepared)
}

/// Project the authenticated refresh into conflict recovery's failed input as
/// well as its retry. Neither consumer may recover against the superseded pin.
pub(crate) fn refreshed_input<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    if preparation(input).is_none() {
        return Ok(input.clone());
    }
    let context = load_handoff_context(host, input, "git_rebase")?;
    effective_input(host, input, &context).map(|(effective, _)| effective)
}

fn effective_input<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
) -> Result<(Value, Option<Value>), OrbitError> {
    let source = preparation(input).ok_or_else(|| refusal("missing initial preparation"))?;
    let run_id = recovery_run_id(input, context);
    validate_owner(host, run_id, context)?;
    let task_ids = json!(
        context
            .tasks
            .iter()
            .map(|task| &task.id)
            .collect::<Vec<_>>()
    );
    if source["kind"] != "delivery_preparation"
        || source["run_id"] != run_id
        || source["step_id"] != PREPARE
        || source["task_ids"] != task_ids
        || !recorded_workspace_matches(source["workspace_path"].as_str(), &context.workspace_path)
        || !host.verify_rebase_recovery(run_id, PREPARE, source)?
        || FIELDS.iter().any(|field| {
            input[*field] != source[*field] || input["preparation"][*field] != source[*field]
        })
        || input["step_id"] != "sync_base"
    {
        return Err(refusal("missing, foreign or altered initial preparation"));
    }
    let attempts = host.rebase_recovery_attempts(run_id, REFRESH)?;
    let latest = host
        .read_run_state(run_id)?
        .and_then(|state| state.rebase_recovery_checkpoints.get(REFRESH).cloned());
    let mut effective = input.clone();
    if let Some(latest) = &latest {
        let previous = &latest["source_preparation"];
        let same_preparation = previous == source;
        // A legitimate preserved-failure resume may prepare anew in the same
        // run. It supersedes the old preparation, while retaining the run's
        // refresh count and deadline. Never inherit its candidate fields.
        let superseded = matches!(
            (previous["recovery_attempt"].as_u64(), source["recovery_attempt"].as_u64()),
            (Some(old), Some(new)) if new > old
        ) && previous["task_ids"] == source["task_ids"]
            && previous["workspace_path"] == source["workspace_path"];
        if (!same_preparation && !superseded)
            || latest["run_id"] != run_id
            || latest["step_id"] != REFRESH
            || !host.verify_rebase_recovery(run_id, REFRESH, latest)?
            || latest["recovery_attempt"].as_u64() != Some(attempts.len() as u64)
        {
            return Err(refusal("missing, foreign or altered refresh evidence"));
        }
        if same_preparation {
            for field in FIELDS {
                effective[*field] = latest[*field].clone();
            }
        }
    } else if !attempts.is_empty() {
        return Err(refusal("reserved refresh has no authenticated completion"));
    }
    Ok((effective, latest))
}

pub(super) fn synchronize<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    mut rebase: impl FnMut(&Value, &mut bool) -> Result<Value, OrbitError>,
) -> Result<Value, OrbitError> {
    if preparation(input).is_none() {
        return rebase(input, &mut false);
    }
    let (mut effective, mut latest) = effective_input(host, input, context)?;
    loop {
        validate_owner(host, recovery_run_id(input, context), context)?;
        if !rebase_in_progress(&context.workspace_path)? {
            let current = commit_sha(&context.workspace_path, "HEAD")?;
            if let Some(latest) = &latest
                && latest["source_preparation"]
                    == *preparation(input).ok_or_else(|| refusal("missing preparation"))?
                && current == required_input_string(&effective, "head_sha")?
                && now_ms()?
                    >= latest["deadline_ms"]
                        .as_u64()
                        .ok_or_else(|| refusal("refresh lost its deadline"))?
            {
                return Err(refusal(format!(
                    "fresh preparation deadline exhausted; original stale preparation error: {}; attempt trace: {}",
                    latest["original_error"], latest["attempt_trace"]
                )));
            }
            if current != required_input_string(&effective, "head_sha")?
                && !matches!(
                    recovered_rewrite(host, &effective, context, &current)?,
                    RecoveredRewrite::Current(_) | RecoveredRewrite::Overtaken(_)
                )
            {
                // The legacy pinned path can redo an uncertified rewrite.
                // Refresh admission must never reset a foreign candidate.
                return Err(refusal(
                    "candidate changed without an authenticated rewrite",
                ));
            }
        }
        if remote_branch_sha(
            &context.workspace_path,
            required_input_string(&effective, "head")?,
        )? != effective["remote_sha"].as_str().map(ToOwned::to_owned)
        {
            return Err(refusal("remote candidate lease changed"));
        }
        let mut stale = false;
        match rebase(&effective, &mut stale) {
            Ok(mut output) => {
                if let Some(latest) = latest {
                    output["preparation_refresh"] = latest;
                }
                return Ok(output);
            }
            Err(error) if stale => {
                // Keep the exact original refusal before attempting normal recovery.
                record_failed_handoff(
                    host,
                    context,
                    &effective,
                    FailedHandoffPhase::Rebase,
                    &error,
                )?;
                let refreshed = refresh(host, input, context, &effective, latest.as_ref(), &error);
                match refreshed {
                    Ok(next) => {
                        for field in FIELDS {
                            effective[*field] = next[*field].clone();
                        }
                        latest = Some(next);
                    }
                    Err(reason) => {
                        return Err(refusal(format!(
                            "{reason}; original stale preparation error: {}; attempt trace: {}",
                            latest
                                .as_ref()
                                .map(|value| &value["original_error"])
                                .unwrap_or(&json!(error.to_string())),
                            latest
                                .as_ref()
                                .map(|value| &value["attempt_trace"])
                                .unwrap_or(&Value::Null),
                        )));
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn refresh<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    effective: &Value,
    latest: Option<&Value>,
    original_error: &OrbitError,
) -> Result<Value, OrbitError> {
    let run_id = recovery_run_id(input, context);
    let attempts = host.rebase_recovery_attempts(run_id, REFRESH)?;
    if attempts.len() >= MAX_REFRESHES {
        return Err(refusal(format!(
            "exhausted {MAX_REFRESHES} fresh preparation attempts"
        )));
    }
    let deadline = match latest {
        Some(latest) => latest["deadline_ms"]
            .as_u64()
            .ok_or_else(|| refusal("refresh lost its deadline"))?,
        None => now_ms()?.saturating_add(REFRESH_BUDGET_MS),
    };
    let remaining = deadline
        .checked_sub(now_ms()?)
        .filter(|ms| *ms > 0)
        .ok_or_else(|| refusal("fresh preparation deadline exhausted"))?;
    // Fetches and reads share one remaining deadline. No Git rewrite occurs
    // under this guard; normal rebase timeout cleanup keeps its own budget.
    let _deadline = GitDeadlineGuard::install(std::time::Duration::from_millis(remaining));
    let workspace = &context.workspace_path;
    let head = required_input_string(effective, "head")?;
    let head_sha = required_input_string(effective, "head_sha")?;
    let base_ref = required_input_string(effective, "base_ref")?;
    let old_base = required_input_string(effective, "base_sha")?;
    let unchanged = || -> Result<(), OrbitError> {
        validate_owner(host, run_id, context)?;
        if rebase_in_progress(workspace)?
            || git_output(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim() != head
            || commit_sha(workspace, "HEAD")? != head_sha
            || remote_branch_sha(workspace, head)?
                != effective["remote_sha"].as_str().map(ToOwned::to_owned)
        {
            return Err(refusal(
                "candidate, branch, remote lease or rebase state changed",
            ));
        }
        ensure_clean_for_rewrite(workspace, head_sha, "a stale prepared base")
    };
    unchanged()?;
    let mut prepare_input = input.clone();
    prepare_input["base_sync"] = input["preparation"]["preparation"]["base_sync"].clone();
    let mut next =
        prepare_pr_handoff_inner(&prepare_input, context).map_err(|failure| failure.1)?;
    let new_base = required_input_string(&next, "base_sha")?;
    if next["base_ref"] != base_ref
        || new_base == old_base
        || !git_command_success(
            workspace,
            &["merge-base", "--is-ancestor", old_base, new_base],
        )?
    {
        return Err(refusal(
            "base did not strictly advance along its prepared ancestry",
        ));
    }
    unchanged()?;
    if next["head"] != head
        || next["head_sha"] != head_sha
        || next["remote_sha"] != effective["remote_sha"]
        || now_ms()? >= deadline
    {
        return Err(refusal(
            "preparation changed candidate/lease or exhausted its deadline",
        ));
    }
    let mut trace = latest
        .and_then(|value| value["attempt_trace"].as_array())
        .cloned()
        .unwrap_or_default();
    trace.push(json!({"previous_base_sha": old_base, "base_sha": new_base, "error": original_error.to_string()}));
    next["kind"] = json!("delivery_preparation_refresh");
    next["run_id"] = json!(run_id);
    next["task_ids"] = input["preparation"]["preparation"]["task_ids"].clone();
    next["workspace_path"] = json!(workspace);
    next["head_sha_before"] = json!(head_sha);
    next["target_base_sha"] = next["base_sha"].clone();
    next["source_preparation"] = input["preparation"]["preparation"].clone();
    next["previous_base_sha"] = json!(old_base);
    next["deadline_ms"] = json!(deadline);
    next["original_error"] = latest
        .map(|value| value["original_error"].clone())
        .unwrap_or_else(|| json!(original_error.to_string()));
    next["attempt_trace"] = json!(trace);
    persist(host, context, REFRESH, next)
}

pub(super) fn validate_owner<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    context: &HandoffContext,
) -> Result<(), OrbitError> {
    host.validate_step_recovery_mutation(
        run_id,
        "sync_base",
        &context
            .tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>(),
        &context.workspace_path,
    )
}

fn persist<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    step: &str,
    mut payload: Value,
) -> Result<Value, OrbitError> {
    let run = required_input_string(&payload, "run_id")?.to_string();
    let dispatch = |error: crate::DispatchError| OrbitError::Execution(error.to_string());
    let attempt = host
        .begin_rebase_recovery_attempt(
            &run,
            step,
            &RebaseRecoveryAttemptScope {
                workspace_path: context.workspace_path.to_string_lossy().into_owned(),
                head_sha_before: required_input_string(&payload, "head_sha_before")?.to_string(),
                target_base_sha: required_input_string(&payload, "target_base_sha")?.to_string(),
            },
        )
        .map_err(dispatch)?;
    payload["step_id"] = json!(step);
    payload["recovery_attempt"] = json!(attempt);
    host.checkpoint_rebase_recovery(&run, step, &payload)
        .map_err(dispatch)?;
    let path = format!("validation/{run}/preparation/{step}-{attempt}.json");
    let content = serde_json::to_vec(&payload).map_err(|error| refusal(error.to_string()))?;
    for task in &context.tasks {
        if let Some(previous) = host.get_task_artifact(&task.id, &path)?
            && previous.content != content
        {
            return Err(refusal(
                "immutable preparation artifact already has different bytes",
            ));
        }
        if host.worker_invocation().is_some() {
            host.attach_claim_validation_log(&path, content.clone())?;
        } else {
            host.attach_task_validation_log(&task.id, &context.batch_id, &path, content.clone())?;
        }
    }
    Ok(payload)
}
