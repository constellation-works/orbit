use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffValidationLog,
};
use orbit_types::workflow::{BaselineRedHold, TRANSIENT_FAILURE_MARKER};
use serde_json::{Value, json};

use crate::context::{ClaimExecutionContext, RuntimeHost};
use crate::executor::automation::input::input_string_field;

use super::super::baseline::{
    baseline_red_failure, candidate_failure, compare_with_base, run_validation_command,
};
use super::super::git::normalize_base_branch;
use super::super::required_command::RequiredCommandRun;
use super::input::{refused, required_workspace};
use super::no_diff;
use super::observe::{claimed_base_sync, observe, observe_with, require_clean_candidate};

/// Run the owner's required commands on the exact candidate and attach one
/// captured log per command to the owner's copy of the task.
///
/// The pull-request route validates before anything is published
/// [ORB-14258]. With no `pull_request` yet, the step runs the commands and
/// returns their results as `publication: pending`, attaching nothing: every
/// log the owner accepts names the delivery, which does not exist yet. After
/// `pr_open`, the same activity given that output as `prevalidated` runs
/// nothing. It re-observes the candidate with its pull request, refuses one
/// that is not the commit and base the commands passed on, and attaches the
/// logs. A candidate whose validation failed is therefore never published.
///
/// A failing command is compared with the base (see [`super::super::baseline`]):
/// its log and the base's are attached to the owner's task, and a failure
/// the base shares fails as a typed `baseline_red`, which the leaf's
/// settlement turns into a held release rather than a failed claim.
///
/// An empty requirement list runs no command: the candidate is still observed
/// and pinned, and the output records `skipped_no_required_commands` with no
/// validation references.
///
/// [ORB-14849] A before-landing review runs after `pr_open`. Its
/// revalidation step passes the pre-publication output as `carry`: unless
/// `revalidate` is true — the reviewer committed a fix — that output is
/// returned unchanged, so the pin step always reads one step. With a fix the
/// commands run on the new head, still unpublished, before it is pushed.
pub(in crate::executor::automation) fn claim_validate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = host.claim_execution_context()?;
    let workspace_path = required_workspace(input)?;
    if input
        .get("skipped_no_diff_expected")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return no_diff::validate(host, &context, &workspace_path, input);
    }
    if let Some(carried) = input.get("carry").filter(|value| !value.is_null())
        && input.get("revalidate").and_then(Value::as_bool) != Some(true)
    {
        return Ok(carried.clone());
    }
    if let Some(prevalidated) = input.get("prevalidated").filter(|value| !value.is_null()) {
        return pin_prevalidated(host, &context, &workspace_path, input, prevalidated);
    }
    let pending = context.ship_mode == "pr"
        && input
            .get("pull_request")
            .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty));
    let candidate = if pending {
        // Observed for its commit and base only; the delivery is pinned by
        // the `prevalidated` step once the pull request exists.
        observe_with(
            &workspace_path,
            &context,
            input,
            HandoffDelivery::LocalCandidate,
        )?
    } else {
        observe(&workspace_path, &context, input)?
    };
    require_clean_candidate(&workspace_path, &candidate)?;

    let mut results = Vec::new();
    let mut validation_env = Value::Null;
    for (index, command) in context.required_commands.iter().enumerate() {
        let run = run_validation_command(host, &workspace_path, command, None)?;
        validation_env = run.environment_record();
        if !run.passed {
            return Err(claim_failure(
                host,
                &context,
                &workspace_path,
                input,
                &candidate,
                index,
                &run,
            ));
        }
        if let Some(failure) = run.deferral_failure(&candidate.candidate.commit) {
            return Err(failure);
        }
        require_clean_candidate(&workspace_path, &candidate)?;
        results.push((run.command, run.output));
    }

    // A later required command must not invalidate logs from an earlier one.
    // Publish them only after the whole suite has kept the candidate intact.
    require_clean_candidate(&workspace_path, &candidate)?;
    if pending {
        let mut output = json!({
            "decision": "passed",
            "publication": "pending",
            "candidate": null,
            "validation": [],
            "commands": results.iter().map(|(command, _)| command).collect::<Vec<_>>(),
            "results": results
                .iter()
                .map(|(command, output)| json!({ "command": command, "output": output }))
                .collect::<Vec<_>>(),
            "tested_head": candidate.candidate.commit,
            "validated_base": candidate.base.commit,
            "validation_env": validation_env,
        });
        if context.required_commands.is_empty() {
            output["decision"] = json!(SKIPPED_NO_REQUIRED_COMMANDS);
            output["note"] = json!(NO_REQUIRED_COMMANDS_NOTE);
        }
        return Ok(output);
    }
    let references = attach_handoff_logs(host, &context, &candidate, &results)?;
    passed_output(&context, &candidate, &references, results, validation_env)
}

/// Pin a pre-publication validation onto the published candidate: the
/// commands already passed on this exact commit and base, so the logs are
/// built from those results with the pull request as their delivery.
fn pin_prevalidated<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace_path: &Path,
    input: &Value,
    prevalidated: &Value,
) -> Result<Value, OrbitError> {
    let candidate = observe(workspace_path, context, input)?;
    require_clean_candidate(workspace_path, &candidate)?;
    if input_string_field(prevalidated, "publication").as_deref() != Some("pending") {
        return Err(OrbitError::InvalidInput(
            "prevalidated must be the output of a pre-publication claim_validate step".to_string(),
        ));
    }
    let tested_head = input_string_field(prevalidated, "tested_head");
    let validated_base = input_string_field(prevalidated, "validated_base");
    if tested_head.as_deref() != Some(candidate.candidate.commit.as_str())
        || validated_base.as_deref() != Some(candidate.base.commit.as_str())
    {
        return Err(refused(format!(
            "required validation passed on candidate {} over base {}, but the published \
             candidate is {} over base {}; rerun validation",
            tested_head.as_deref().unwrap_or("<none>"),
            validated_base.as_deref().unwrap_or("<none>"),
            candidate.candidate.commit,
            candidate.base.commit
        )));
    }
    let results = prevalidated
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .map(|result| {
                    Some((
                        result.get("command")?.as_str()?.to_string(),
                        result.get("output")?.as_str()?.to_string(),
                    ))
                })
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default()
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "prevalidated results must each carry a command and its output".to_string(),
            )
        })?;
    let required = context
        .required_commands
        .iter()
        .map(|command| command.trim())
        .collect::<Vec<_>>();
    if results
        .iter()
        .map(|(command, _)| command.as_str())
        .ne(required)
    {
        return Err(refused(
            "the prevalidated commands are not the owner's required commands; rerun validation",
        ));
    }
    let references = attach_handoff_logs(host, context, &candidate, &results)?;
    let validation_env = prevalidated
        .get("validation_env")
        .cloned()
        .unwrap_or(Value::Null);
    passed_output(context, &candidate, &references, results, validation_env)
}

/// Attach one typed log per passed command to the owner's task.
pub(super) fn attach_handoff_logs<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    candidate: &HandoffCandidate,
    results: &[(String, String)],
) -> Result<Vec<HandoffArtifactRef>, OrbitError> {
    let mut references = Vec::new();
    for (index, (command, output)) in results.iter().enumerate() {
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: context.workspace_id.clone(),
            task_id: context.task_id.clone(),
            claim_id: context.claim_id.clone(),
            machine_id: context.machine_id.clone(),
            run_id: context.run_id.clone(),
            candidate: candidate.clone(),
            tested_head: candidate.candidate.commit.clone(),
            command: command.clone(),
            exit_code: 0,
            output: output.clone(),
        };
        let content = serde_json::to_vec(&log)
            .map_err(|error| OrbitError::Execution(format!("encode validation log: {error}")))?;
        let path = format!("validation/{}/{index}.json", context.claim_id);
        host.attach_claim_validation_log(&path, content.clone())?;
        references.push(HandoffArtifactRef {
            path,
            sha256: sha256_hex(&content),
        });
    }
    Ok(references)
}

pub(super) fn passed_output(
    context: &ClaimExecutionContext,
    candidate: &HandoffCandidate,
    references: &[HandoffArtifactRef],
    results: Vec<(String, String)>,
    validation_env: Value,
) -> Result<Value, OrbitError> {
    let mut output = json!({
        "decision": "passed",
        "candidate": serde_json::to_value(candidate)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "validation": serde_json::to_value(references)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "commands": results.into_iter().map(|(command, _)| command).collect::<Vec<_>>(),
        "tested_head": candidate.candidate.commit,
        "validated_base": candidate.base.commit,
        "validation_env": validation_env,
        "no_diff_evidence": null,
    });
    if context.required_commands.is_empty() {
        output["decision"] = json!(SKIPPED_NO_REQUIRED_COMMANDS);
        output["note"] = json!(NO_REQUIRED_COMMANDS_NOTE);
    }
    Ok(output)
}

/// The refusal for a claimed command that did not pass. A failure that is not
/// a missing tool is rerun on the candidate's base, and both logs are
/// attached to the owner's task before the step fails; a failure the base
/// shares is typed `baseline_red`, and one that still could not reach the
/// network after its retries is typed `transient`.
pub(super) fn claim_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace_path: &Path,
    input: &Value,
    candidate: &HandoffCandidate,
    index: usize,
    run: &RequiredCommandRun,
) -> OrbitError {
    let commit = &candidate.candidate.commit;
    if run.missing_tool.is_some() {
        return run.failure(commit);
    }
    let check = compare_with_base(
        host,
        workspace_path,
        &candidate.base.commit,
        &run.command,
        run.selection(),
    );
    let red = check.reproduces(run);
    let log_path = format!("validation/{}/{index}.failed.json", context.claim_id);
    let base_path = format!("validation/{}/{index}.baseline.json", context.claim_id);
    let log = json!({
        "schema_version": 1,
        "role": "candidate",
        "workspace_id": context.workspace_id,
        "task_id": context.task_id,
        "claim_id": context.claim_id,
        "machine_id": context.machine_id,
        "run_id": context.run_id,
        "tested_head": commit,
        "base_sha": candidate.base.commit,
        "command": run.command,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "output": run.output,
        "validation_env": run.environment_record(),
        "failure_kind": if red {
            json!("baseline_red")
        } else if run.network_evidence.is_some() {
            json!("transient")
        } else {
            run.failure_kind()
        },
        "network_retries": run.network_retries,
        "summary": run.summary,
        "baseline": check.record(Some(&base_path)),
    });
    // The logs are evidence for whoever reads the owner's task; failing to
    // deliver them must not hide the typed failure itself.
    for (path, content) in [(&log_path, log), (&base_path, check.log(&context.run_id))] {
        if let Err(error) = serde_json::to_vec(&content)
            .map_err(|error| OrbitError::Execution(error.to_string()))
            .and_then(|bytes| host.attach_claim_validation_log(path, bytes))
        {
            tracing::warn!(
                path,
                "could not attach a claimed validation failure log: {error}"
            );
        }
    }
    let evidence = format!("Candidate log: `{log_path}`; base log: `{base_path}`.");
    if red {
        let hold = BaselineRedHold {
            base_ref: claimed_base_ref(context, input),
            base_sha: check.base_sha.clone(),
            command: run.command.clone(),
            run_id: context.run_id.clone(),
            selection: run.selection().cloned(),
        };
        return baseline_red_failure(&hold, run, commit, &evidence);
    }
    // Still unable to reach the network after its retries, the command said
    // nothing about the candidate [ORB-14257].
    if let Some(network) = &run.network_evidence {
        return OrbitError::Execution(format!(
            "{TRANSIENT_FAILURE_MARKER} required validation '{}' could not reach the network on \
             candidate {commit} after {} retries ({network}); the candidate was not judged. \
             {evidence}",
            run.command, run.network_retries
        ));
    }
    candidate_failure(run, commit, Some(&check), &evidence)
}

/// The base ref this claim synchronized onto, as the owner names it.
fn claimed_base_ref(context: &ClaimExecutionContext, input: &Value) -> String {
    let branch = normalize_base_branch(&context.base_branch)
        .unwrap_or_else(|_| context.base_branch.trim().to_string());
    match claimed_base_sync(context, input).as_deref() {
        Ok("remote") => format!("origin/{branch}"),
        _ => branch,
    }
}

/// The decision a validation step records when the workspace requires no
/// command, on the owner's delivery path and a claimed leaf alike.
pub(in crate::executor::automation::vcs) const SKIPPED_NO_REQUIRED_COMMANDS: &str =
    "skipped_no_required_commands";

/// Why that decision ran nothing, in the step's own output.
pub(in crate::executor::automation::vcs) const NO_REQUIRED_COMMANDS_NOTE: &str = "no required validation commands configured \
     (`workflow.required_validation_commands` is empty); no check ran";
