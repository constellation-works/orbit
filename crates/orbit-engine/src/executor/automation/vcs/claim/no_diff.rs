//! Clean-base delivery for claimed leaves. The report is the existing verifier's
//! checkpoint, not an agent's skip flag; the owner rechecks it on its own base.
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, TaskHandoff,
};
use serde_json::{Value, json};

use crate::context::{ClaimExecutionContext, RuntimeHost};

use super::super::baseline::run_validation_command;
use super::super::git::{BaseSyncMode, resolve_worktree_start_point};
use super::super::review_gate::revision;
use super::delivery::repository;
use super::input::refused;
use super::observe::{observe_with, require_clean_candidate};
use super::validation::{attach_handoff_logs, claim_failure, passed_output};
use crate::executor::automation::vcs::commit::{
    claimed_clean_base_checkpoint, verify_clean_tree_handoff, verify_clean_tree_handoff_at_revision,
};

/// Whether the implementer returned clean-tree report scratch references.
pub(in crate::executor::automation::vcs) fn carries_no_diff_artifacts(input: &Value) -> bool {
    input
        .get("implementation")
        .and_then(|output| output.get("no_diff_artifacts"))
        .and_then(Value::as_array)
        .is_some_and(|artifacts| !artifacts.is_empty())
}

/// A sandboxed implementer cannot route artifacts to a remote owner. It
/// returns scratch paths; the deterministic commit step reads bounded bytes
/// through the existing artifact confinement helper and attaches them itself.
pub(in crate::executor::automation::vcs) fn import_implementation_evidence<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    task_id: &str,
    workspace: &Path,
    input: &Value,
) -> Result<(), OrbitError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ScratchArtifact {
        path: String,
        source_path: String,
    }
    let Some(artifacts) = input
        .get("implementation")
        .and_then(|output| output.get("no_diff_artifacts"))
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };
    let artifacts: Vec<ScratchArtifact> = serde_json::from_value(artifacts.clone())
        .map_err(|error| refused(format!("invalid no_diff_artifacts: {error}")))?;
    let scratch = workspace.join(".orbit/tmp");
    let mut prepared = Vec::new();
    let mut paths = std::collections::BTreeSet::new();
    for artifact in artifacts {
        if !paths.insert(artifact.path.clone()) {
            return Err(refused("no_diff_artifacts contains duplicate paths"));
        }
        let payload = orbit_tools::prepare_remote_task_artifact_put(
            json!({
                "id": task_id, "path": artifact.path, "source_path": artifact.source_path,
            }),
            Some(workspace),
            Some(&scratch),
        )?;
        let bytes: Vec<u8> = serde_json::from_value(payload["artifacts"][0]["content"].clone())
            .map_err(|error| refused(format!("invalid prepared artifact: {error}")))?;
        prepared.push((artifact.path, bytes));
    }
    let mut permitted = std::collections::BTreeSet::new();
    for (path, bytes) in &prepared {
        let logs = match path.as_str() {
            "no-diff.json" => {
                let report: orbit_types::workflow::handoff::NoDiffEvidence =
                    serde_json::from_slice(bytes)
                        .map_err(|error| refused(format!("invalid {path}: {error}")))?;
                report
                    .validation
                    .into_iter()
                    .map(|check| check.log_artifact)
                    .collect::<Vec<_>>()
            }
            "already-landed.json" => {
                let report: orbit_types::workflow::handoff::AlreadyLandedEvidence =
                    serde_json::from_slice(bytes)
                        .map_err(|error| refused(format!("invalid {path}: {error}")))?;
                report
                    .validation
                    .into_iter()
                    .map(|check| check.log_artifact)
                    .collect::<Vec<_>>()
            }
            _ => continue,
        };
        permitted.insert(path.clone());
        permitted.extend(logs);
    }
    if permitted.is_empty() || !paths.is_subset(&permitted) {
        return Err(refused(
            "no_diff_artifacts may contain only a clean-tree report and its declared validation logs",
        ));
    }
    for (path, bytes) in prepared {
        host.attach_claim_validation_log(&path, bytes)?;
    }
    Ok(())
}

fn checkpoint<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    reference: &HandoffArtifactRef,
) -> Result<Value, OrbitError> {
    orbit_types::task::validate_relative_artifact_path(&reference.path)?;
    let artifacts = host.get_task_artifacts(task_id)?;
    let artifact = artifacts
        .iter()
        .find(|artifact| artifact.path == reference.path)
        .ok_or_else(|| refused("NoDiff verifier report is missing"))?;
    if sha256_hex(&artifact.content) != reference.sha256 {
        return Err(refused("NoDiff verifier report changed"));
    }
    serde_json::from_slice(&artifact.content)
        .map_err(|error| refused(format!("invalid NoDiff verifier report: {error}")))
}

pub(super) fn verify_leaf<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace: &Path,
    evidence: &HandoffArtifactRef,
) -> Result<(), OrbitError> {
    let report = checkpoint(host, &context.task_id, evidence)?;
    let task = host.get_task(&context.task_id)?;
    require_checkpoint_identity(&report, &task.id, &context.run_id)?;
    verify_clean_tree_handoff(host, &[task], workspace, &context.run_id, &report)
}

fn require_checkpoint_identity(report: &Value, task: &str, run: &str) -> Result<(), OrbitError> {
    if report["task_id"] != task
        || report["job_run_id"] != run
        || !matches!(
            report["decision"].as_str(),
            // [ORB-14791] A claimed `no-diff-expected` skip pins its run and
            // base; its verifier rechecks the owner's tag.
            Some("verified_no_diff" | "verified_already_landed" | "skipped_no_diff_expected")
        )
    {
        return Err(refused(
            "NoDiff requires this task/run's verified clean-tree checkpoint",
        ));
    }
    Ok(())
}

pub(super) fn validate<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace: &Path,
    input: &Value,
) -> Result<Value, OrbitError> {
    let report = claimed_clean_base_checkpoint(input)
        .ok_or_else(|| refused("a skip flag or tag alone cannot authorize a NoDiff handoff"))?;
    require_checkpoint_identity(report, &context.task_id, &context.run_id)?;
    let task = host.get_task(&context.task_id)?;
    verify_clean_tree_handoff(host, &[task], workspace, &context.run_id, report)?;
    let content =
        serde_json::to_vec(report).map_err(|error| OrbitError::Execution(error.to_string()))?;
    let evidence = HandoffArtifactRef {
        path: format!("no-diff-handoff/{}.json", context.claim_id),
        sha256: sha256_hex(&content),
    };
    host.attach_claim_validation_log(&evidence.path, content)?;
    let candidate = observe_with(
        workspace,
        context,
        input,
        HandoffDelivery::NoDiff {
            evidence: evidence.clone(),
        },
    )?;
    require_clean_candidate(workspace, &candidate)?;
    let mut results = Vec::new();
    let mut validation_env = Value::Null;
    for (index, command) in context.required_commands.iter().enumerate() {
        let run = run_validation_command(host, workspace, command)?;
        validation_env = run.environment_record();
        if !run.passed {
            return Err(claim_failure(
                host, context, workspace, input, &candidate, index, &run,
            ));
        }
        require_clean_candidate(workspace, &candidate)?;
        results.push((run.command, run.output));
    }
    // Commands may have changed task artifacts too; recheck the checkpoint.
    verify_leaf(host, context, workspace, &evidence)?;
    let references = attach_handoff_logs(host, context, &candidate, &results)?;
    let mut output = passed_output(context, &candidate, &references, results, validation_env)?;
    output["no_diff_evidence"] =
        serde_json::to_value(evidence).map_err(|error| OrbitError::Execution(error.to_string()))?;
    Ok(output)
}

/// Observe and reverify a NoDiff claim on the owner's live base. No executor
/// branch or pull request needs to exist on the owner, and no checkout is
/// modified: the shared verifier reads immutable Git objects and task artifacts.
pub fn observe_no_diff_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    handoff: &TaskHandoff,
    base_sync: &str,
) -> Result<HandoffCandidate, OrbitError> {
    let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery else {
        return Err(refused("NoDiff delivery required"));
    };
    let report = checkpoint(host, &handoff.task_id, evidence)?;
    require_checkpoint_identity(&report, &handoff.task_id, &handoff.run_id)?;
    let submitted = &handoff.candidate;
    let sync = match base_sync {
        "remote" => BaseSyncMode::Remote,
        "local" => BaseSyncMode::Local,
        _ => {
            return Err(refused(
                "NoDiff observation requires a local or remote base",
            ));
        }
    };
    let base_ref = resolve_worktree_start_point(workspace, &submitted.base_branch, sync)?;
    let base = revision(workspace, &base_ref)?;
    if submitted.candidate != base
        || submitted.base != base
        || report["base_sha"] != base.commit
        || submitted.repository != repository(workspace, &handoff.workspace_id)
        || !handoff.footprint_widening.is_empty()
    {
        return Err(refused(
            "NoDiff candidate must equal the owner's current base, with no footprint widening",
        ));
    }
    let task = host.get_task(&handoff.task_id)?;
    verify_clean_tree_handoff_at_revision(host, &task, workspace, &handoff.run_id, &report)?;
    Ok(HandoffCandidate {
        candidate: base.clone(),
        base,
        ..submitted.clone()
    })
}
