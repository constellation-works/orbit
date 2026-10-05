//! The observation a reconciliation binds to, read fresh from the owner's
//! claim journal, the provider and the owner checkout every time it matters:
//! at submission, before validation, before recording, and at completion.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_common::security::release::sha256_hex;
use orbit_engine::review_gate;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::handoff::AcceptedHandoff;
use orbit_types::workflow::{
    ReconciledExecution, ReconciledPullRequest, ReconciliationBinding, ReconciliationCommandSource,
    ReconciliationContract, ReviewReconciliation,
};

use crate::OrbitRuntime;
use crate::application::task::HandoffPullRequest;

/// A merged foreign delivery whose head differs from its handed-off candidate.
pub(super) struct Observation {
    pub(super) accepted: AcceptedHandoff,
    pub(super) binding: ReconciliationBinding,
    pub(super) binding_digest: String,
}

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

pub(super) fn task_digest(task: &Task) -> Result<String, OrbitError> {
    orbit_automation::review::task_meaning_digest(task)
        .map_err(|error| OrbitError::Execution(format!("digest task meaning: {error}")))
}

pub(super) fn binding_digest(binding: &ReconciliationBinding) -> Result<String, OrbitError> {
    let encoded = serde_json::to_vec(binding)
        .map_err(|error| OrbitError::Execution(format!("encode binding: {error}")))?;
    Ok(sha256_hex(&encoded))
}

/// Whether `record` reconciles exactly this execution, pull request, merged
/// head and task meaning, with an intact binding.
pub(super) fn binds(
    record: &ReviewReconciliation,
    task_digest: &str,
    accepted: &AcceptedHandoff,
    pull_request: &HandoffPullRequest,
) -> bool {
    let binding = &record.binding;
    let handoff = &accepted.handoff;
    binding_digest(binding).is_ok_and(|digest| digest == record.binding_digest)
        && binding.task_meaning_digest == task_digest
        && binding.execution
            == ReconciledExecution {
                run_id: handoff.run_id.clone(),
                machine_id: handoff.machine_id.clone(),
                claim_id: handoff.claim_id.clone(),
                handoff_id: accepted.handoff_id.clone(),
                candidate_commit: handoff.candidate.candidate.commit.clone(),
            }
        && binding.pull_request.number == pull_request.number
        && binding.pull_request.url == pull_request.url
        && binding.pull_request.landing_branch == handoff.candidate.landing_branch
        && binding.pull_request.merged_head.commit == pull_request.head
}

/// Observe `task_id`'s merged foreign delivery, refusing with the next step
/// whenever it is not one a reconciliation can bind.
pub(super) fn observe(runtime: &OrbitRuntime, task_id: &str) -> Result<Observation, OrbitError> {
    let task = runtime.get_task(task_id)?;
    if task.status != TaskStatus::Review {
        return Err(refused(format!(
            "task {task_id} is {}; only a task in review can reconcile its merged delivery",
            task.status
        )));
    }
    let foreign = runtime.desktop_foreign_execution(&task)?.ok_or_else(|| {
        refused(
            "the task's linked run was not executed on another machine; reconciliation applies \
             only to a recovered follower delivery, and a local delivery completes through its \
             own run",
        )
    })?;
    foreign.ensure_stopped()?;
    let (number, accepted) = foreign.pull_request_handoff().ok_or_else(|| {
        refused(
            "this owner holds no accepted pull-request handoff from the linked run, so there is \
             no merged delivery to reconcile",
        )
    })?;
    let accepted = accepted.clone();
    let pull_request = runtime.observe_handoff_pull_request(number, &accepted)?;
    let handoff = &accepted.handoff;
    if !pull_request.merged() {
        return Err(refused(format!(
            "pull request #{number} has not merged (state {}); reconcile its head only after it \
             merges",
            pull_request.state
        )));
    }
    if pull_request.head == handoff.candidate.candidate.commit {
        return Err(refused(format!(
            "pull request #{number} merged the handed-off candidate itself; it needs no \
             reconciliation, so complete the task from review"
        )));
    }
    let repo = &runtime.paths().repo_root;
    let readable = |commit: &str| {
        review_gate::fetch_landed_commit(repo, commit).map_err(|error| {
            refused(format!(
                "commit {commit} of pull request #{number} is not readable in the owner checkout \
                 ({error}); fetch origin there and retry"
            ))
        })
    };
    readable(&pull_request.head)?;
    let merged_head = review_gate::revision(repo, &pull_request.head)?;
    let merge_commit = pull_request.merge_commit.as_deref().ok_or_else(|| {
        refused(format!(
            "the provider reported pull request #{number} as merged without its merge commit; \
             the exact base for historical validation cannot be established, so refresh the \
             provider evidence and retry"
        ))
    })?;
    readable(merge_commit)?;
    let landed_on = format!("{merge_commit}^1");
    let base_commit = merge_base(repo, &merged_head.commit, &landed_on)?;
    let base = review_gate::revision(repo, &base_commit)?;
    let binding = ReconciliationBinding {
        workspace_id: runtime.workspace_id()?,
        task_id: task.id.to_string(),
        task_meaning_digest: task_digest(&task)?,
        execution: ReconciledExecution {
            run_id: handoff.run_id.clone(),
            machine_id: handoff.machine_id.clone(),
            claim_id: handoff.claim_id.clone(),
            handoff_id: accepted.handoff_id.clone(),
            candidate_commit: handoff.candidate.candidate.commit.clone(),
        },
        pull_request: ReconciledPullRequest {
            number,
            url: pull_request.url.clone(),
            repository: handoff.candidate.repository.clone(),
            landing_branch: handoff.candidate.landing_branch.clone(),
            merged_head,
            base,
        },
    };
    let binding_digest = binding_digest(&binding)?;
    Ok(Observation {
        accepted,
        binding,
        binding_digest,
    })
}

fn merge_base(repo: &std::path::Path, head: &str, landed_on: &str) -> Result<String, OrbitError> {
    let output = run_git(repo, &["merge-base", "--end-of-options", head, landed_on])?;
    let commit = output.stdout.trim();
    if !output.success || commit.is_empty() {
        return Err(refused(format!(
            "the merged head {head} has no merge base with {landed_on} in the owner checkout; \
             fetch the landing branch there and retry"
        )));
    }
    Ok(commit.to_string())
}

/// The validation and reviewer contract a reconciliation submitted now
/// would freeze, or why the owner cannot run one at all.
///
/// The accepted handoff's captured commands are the delivery's own
/// obligations and are kept whenever it captured any. An accepted handoff
/// always carries an explicit list; when that list is empty the acceptance
/// required no check, so the operator's reconciliation adopts the owner's
/// current configuration as its own contract and labels it so, rather than
/// claiming the delivery was ever held to it.
pub(super) fn contract(
    runtime: &OrbitRuntime,
    accepted: &AcceptedHandoff,
) -> Result<ReconciliationContract, String> {
    let trimmed = |commands: &[String]| -> Vec<String> {
        commands
            .iter()
            .map(|command| command.trim().to_string())
            .filter(|command| !command.is_empty())
            .collect()
    };
    let historical = trimmed(&accepted.required_commands);
    let (required_commands, commands_source) = if historical.is_empty() {
        (
            trimmed(runtime.workflow_required_validation_commands()),
            ReconciliationCommandSource::OwnerConfigurationAtSubmission,
        )
    } else {
        (historical, ReconciliationCommandSource::AcceptedHandoff)
    };
    if required_commands.is_empty() {
        return Err(
            "the accepted handoff required no validation command and the owner has none \
             configured, so the merged head cannot be validated; set `[workflow] \
             required_validation_commands` in the owner's configuration and retry"
                .into(),
        );
    }
    let crew = &runtime.operation_policy().review_crew;
    let review_crew = crew
        .value
        .clone()
        .filter(|crew| !crew.trim().is_empty())
        .ok_or_else(|| {
            "no independent review crew is configured; set `[operation] review_crew` in the \
             owner's configuration and retry"
                .to_string()
        })?;
    runtime
        .resolve_crew_for_task(Some(&review_crew), None)
        .map_err(|error| {
            format!(
                "the configured review crew '{review_crew}' cannot be resolved on this host \
                 ({error}); define it or set another `[operation] review_crew` and retry"
            )
        })?;
    Ok(ReconciliationContract {
        accepted_commands: accepted.required_commands.clone(),
        required_commands,
        commands_source,
        review_crew,
        review_crew_source: crew.source.label().to_string(),
        frozen_at: Utc::now(),
    })
}
