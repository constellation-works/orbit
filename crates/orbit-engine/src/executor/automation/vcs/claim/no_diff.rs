//! Clean-base delivery for claimed leaves. The report is the existing verifier's
//! checkpoint, not an agent's skip flag; the owner rechecks it on its own base.
//! An already-resolved outcome's named tests replay here on the base, outside
//! the agent sandbox, after the required commands [ORB-15747].
//!
//! A `no-diff-expected` skip checkpoint runs nothing: no required command,
//! deferred replay or already-resolved replay. A review, audit or sweep proves
//! no build, and the owner exempts exactly that checkpoint from required
//! validation, as its own pipelines skip `validate` for the tag [ORB-15764].
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::HostSandboxCommand;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, TaskHandoff,
};
use serde_json::{Value, json};

use crate::context::{ClaimExecutionContext, RuntimeHost};

use super::super::deferred_sandbox::{claimed_deferred_replays, with_replays};
use super::super::git::{
    BaseSyncMode, git_command_success, git_output, resolve_worktree_start_point,
};
use super::super::review_gate::revision;
use super::already_resolved;
use super::delivery::repository;
use super::input::refused;
use super::observe::{observe_with, require_clean_candidate};
use super::validation::{attach_handoff_logs, passed_output, run_claimed_commands};
use crate::executor::automation::vcs::commit::{
    NO_DIFF_EXPECTED_SKIP, claimed_clean_base_checkpoint, verify_clean_tree_handoff,
    verify_clean_tree_handoff_at_revision,
};

/// The decision `claim_validate` records for a `no-diff-expected` skip.
pub(super) const SKIPPED_NO_DIFF_EXPECTED: &str = NO_DIFF_EXPECTED_SKIP;

/// Why that decision ran nothing, in the step's own output.
const NO_DIFF_EXPECTED_NOTE: &str = "the task is tagged no-diff-expected and its clean base \
     changed nothing, so no required validation command or replay ran; the owner accepts this \
     checkpoint without validation logs";

/// Whether `checkpoint` is a `no-diff-expected` skip, which carries no
/// validation.
fn skips_validation(checkpoint: &Value) -> bool {
    checkpoint["decision"] == NO_DIFF_EXPECTED_SKIP
}

/// Whether the implementer returned clean-tree report scratch references.
pub(in crate::executor::automation::vcs) fn carries_no_diff_artifacts(input: &Value) -> bool {
    input
        .get("implementation")
        .and_then(|output| output.get("no_diff_artifacts"))
        .and_then(Value::as_array)
        .is_some_and(|artifacts| !artifacts.is_empty())
}

fn checkpoint<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    reference: &HandoffArtifactRef,
) -> Result<Value, OrbitError> {
    orbit_types::task::validate_relative_artifact_path(&reference.path)?;
    let artifact = host
        .get_task_artifact(task_id, &reference.path)?
        .ok_or_else(|| refused("NoDiff verifier report is missing"))?;
    if sha256_hex(&artifact.content) != reference.sha256 {
        return Err(refused("NoDiff verifier report changed"));
    }
    serde_json::from_slice(&artifact.content)
        .map_err(|error| refused(format!("invalid NoDiff verifier report: {error}")))
}

/// Recheck the pinned checkpoint on the leaf, and return whether it is a
/// `no-diff-expected` skip, whose handoff carries no validation.
pub(super) fn verify_leaf<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace: &Path,
    evidence: &HandoffArtifactRef,
) -> Result<bool, OrbitError> {
    let report = checkpoint(host, &context.task_id, evidence)?;
    let task = host.get_task(&context.task_id)?;
    require_checkpoint_identity(&report, &task.id, &context.run_id)?;
    verify_clean_tree_handoff(host, &[task], workspace, &context.run_id, &report)?;
    Ok(skips_validation(&report))
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

/// Refuse a NoDiff leaf whose worktree no longer is its pinned base, naming
/// the commits beyond that base and the uncommitted paths. Only the leaf's own
/// checkout is read: `origin/<base>` advancing since does not matter here.
fn require_unchanged_base(workspace: &Path, base_sha: &str) -> Result<(), OrbitError> {
    const SHOWN: usize = 10;
    let listed = |lines: &str| {
        let lines = lines.lines().collect::<Vec<_>>();
        let more = lines.len().saturating_sub(SHOWN);
        let mut text = lines.into_iter().take(SHOWN).collect::<Vec<_>>().join("; ");
        if more > 0 {
            text.push_str(&format!("; and {more} more"));
        }
        text
    };
    if base_sha.is_empty() {
        return Err(refused("the NoDiff checkpoint names no synchronized base"));
    }
    let head = git_output(workspace, &["rev-parse", "HEAD"])?;
    let mut changes = Vec::new();
    if head != base_sha {
        let commits = git_output(
            workspace,
            &[
                "log",
                "--format=%h %s",
                "--end-of-options",
                &format!("{base_sha}..{head}"),
            ],
        )?;
        changes.push(if commits.is_empty() {
            format!("HEAD {head} does not sit on it")
        } else {
            format!("commit(s) beyond it: {}", listed(&commits))
        });
    }
    let pending = git_output(
        workspace,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !pending.is_empty() {
        changes.push(format!("uncommitted change(s): {}", listed(&pending)));
    }
    if changes.is_empty() {
        return Ok(());
    }
    Err(refused(format!(
        "a NoDiff handoff must be its synchronized base {base_sha} unchanged; the worktree has {}",
        changes.join(", and ")
    )))
}

pub(super) fn validate<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace: &Path,
    input: &Value,
) -> Result<Value, OrbitError> {
    // Refused before anything runs: a declaration naming nothing, or a
    // selector outside the grammar, never reaches the host.
    let resolved = already_resolved::declared(input)?;
    let report = claimed_clean_base_checkpoint(input)
        .ok_or_else(|| refused("a skip flag or tag alone cannot authorize a NoDiff handoff"))?;
    require_checkpoint_identity(report, &context.task_id, &context.run_id)?;
    require_unchanged_base(workspace, report["base_sha"].as_str().unwrap_or_default())?;
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
    if skips_validation(report) {
        // An implementer's deferral or already-resolved selectors name tests
        // for a gate this route does not run, so neither replays either.
        let mut output = passed_output(context, &candidate, &[], Vec::new(), Value::Null)?;
        output["decision"] = json!(SKIPPED_NO_DIFF_EXPECTED);
        output["note"] = json!(NO_DIFF_EXPECTED_NOTE);
        output["no_diff_evidence"] = serde_json::to_value(evidence)
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        output["already_resolved"] = json!([]);
        return Ok(output);
    }
    let replays = claimed_deferred_replays(host, input)?;
    let commands = with_replays(context.required_commands.clone(), &replays);
    let (mut results, validation_env) = run_claimed_commands(
        host, context, workspace, input, &candidate, &commands, &replays,
    )?;
    let resolved = resolved.unwrap_or_default();
    results.extend(already_resolved::replay(
        host, context, workspace, &candidate, &resolved,
    )?);
    require_clean_candidate(workspace, &candidate)?;
    // Commands may have changed task artifacts too; recheck the checkpoint.
    verify_leaf(host, context, workspace, &evidence)?;
    let references = attach_handoff_logs(host, context, &candidate, &results)?;
    let mut output = passed_output(context, &candidate, &references, results, validation_env)?;
    output["no_diff_evidence"] =
        serde_json::to_value(evidence).map_err(|error| OrbitError::Execution(error.to_string()))?;
    output["already_resolved"] = json!(
        resolved
            .iter()
            .map(HostSandboxCommand::host_command)
            .collect::<Vec<_>>()
    );
    Ok(output)
}

/// Observe and reverify a NoDiff claim against the base its run synchronized
/// onto, which the owner's live base must still contain. The live base may
/// have advanced since [ORB-15074]. No executor branch or pull request needs
/// to exist on the owner, and no checkout is modified: the shared verifier
/// reads immutable Git objects and task artifacts.
pub fn observe_no_diff_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    handoff: &TaskHandoff,
    base_sync: BaseSyncMode,
) -> Result<HandoffCandidate, OrbitError> {
    let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery else {
        return Err(refused("NoDiff delivery required"));
    };
    let report = checkpoint(host, &handoff.task_id, evidence)?;
    require_checkpoint_identity(&report, &handoff.task_id, &handoff.run_id)?;
    let submitted = &handoff.candidate;
    let base_ref = resolve_worktree_start_point(workspace, &submitted.base_branch, base_sync)?;
    let tip = revision(workspace, &base_ref)?;
    let contained = git_command_success(
        workspace,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            &submitted.base.commit,
            &tip.commit,
        ],
    )?;
    if !contained {
        return Err(refused(format!(
            "NoDiff base '{}' is not contained in the owner's base '{base_ref}' ({}); the \
             run's base was rewritten, so revalidate the current base",
            submitted.base.commit, tip.commit
        )));
    }
    let base = revision(workspace, &submitted.base.commit)?;
    if submitted.candidate != base
        || submitted.base != base
        || report["base_sha"] != base.commit
        || submitted.repository != repository(workspace, &handoff.workspace_id)
        || !handoff.footprint_widening.is_empty()
    {
        return Err(refused(
            "NoDiff candidate must equal its synchronized base and the checkpoint's base, with \
             no footprint widening",
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
