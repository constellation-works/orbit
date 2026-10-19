//! One owner-reserved base update for proven absent workflow emission.
use super::pr::{
    CheckState, DeliveryPin, PrMergeState, PullRequestStatus, classify_pr_state,
    read_pr_status_before,
};
use super::{completion_deadline::Deadline, operations};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::handoff::HandoffCandidate;
use orbit_types::workflow::{
    PrBaseUpdateAdmission, PrBaseUpdateReservation, pr_base_update_reservations,
};
use serde_json::{Value, json};
use std::path::Path;

pub(super) enum RefreshStep {
    None,
    Wait,
    Changed,
    Adopted { waiting: bool },
}

/// Called only with captured auto-fix or accepted landing authority. The
/// provider's status and adapter proof are re-read after reserving, before
/// the single write. No caller-authored evidence reaches this operation.
#[allow(clippy::too_many_arguments)]
pub(super) fn recover<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    execution: &str,
    candidate: &HandoffCandidate,
    status: &PullRequestStatus,
    deadline_ms: u64,
    diagnose: bool,
    handoff_id: Option<&str>,
) -> Result<RefreshStep, OrbitError> {
    let primary = tasks.iter().min().ok_or_else(|| refused("no owner task"))?;
    let history = host.get_task_history(primary)?;
    let prior = pr_base_update_reservations(&history)
        .map_err(|why| refused(&why))?
        .into_iter()
        .find(|row| {
            row.repository == candidate.repository
                && row.pr_number.parse::<u64>().ok() == status.number
        });
    if let Some(prior) = prior {
        let deadline = Deadline(prior.deadline_ms.min(deadline_ms));
        // An expired mutation/wait budget cannot hide a delivery that already
        // happened. Existing-merge readback still proves the exact parents,
        // tree and fetched landing before ordinary native settlement.
        if !matches!(classify_pr_state(status), PrMergeState::Merged) {
            deadline.remaining()?;
        }
        // The shared reservation also fences inherited-failure updates. Its
        // own recovery handles old-head failures; no cause grants a second write.
        if matches!(
            prior.evidence.get("cause").and_then(Value::as_str),
            Some("inherited" | "auto_fix_landed")
        ) {
            return Ok(RefreshStep::None);
        }
        if status.head_sha.as_deref() == Some(&prior.head_sha) {
            let observed = read_pr_status_before(host, workspace, &prior.pr_number, deadline.0)?;
            if observed.head_sha.as_deref() == Some(&prior.head_sha) {
                return Ok(RefreshStep::Wait);
            }
            return Ok(RefreshStep::Changed);
        }
        super::landing_checks::verify_published_base_update(workspace, candidate, status)?;
        let parents = super::git::git_output(
            workspace,
            &[
                "rev-list",
                "--parents",
                "-n",
                "1",
                status
                    .head_sha
                    .as_deref()
                    .ok_or_else(|| refused("head unreadable"))?,
            ],
        )?;
        if parents
            .split_whitespace()
            .skip(1)
            .find(|parent| *parent != candidate.candidate.commit)
            != Some(prior.base_sha.as_str())
        {
            return Err(refused(
                "provider base differs from reserved emission source",
            ));
        }
        let tree = super::git::git_output(
            workspace,
            &[
                "rev-parse",
                &format!(
                    "{}^{{tree}}",
                    status
                        .head_sha
                        .as_deref()
                        .ok_or_else(|| refused("head unreadable"))?
                ),
            ],
        )?;
        if tree.trim() != prior.evidence["merged_tree"].as_str().unwrap_or_default() {
            return Err(refused("provider merge differs from reserved clean tree"));
        }
        if !matches!(classify_pr_state(status), PrMergeState::Merged) {
            let observed = host.run_private_vcs_operation(operations::CI_EMISSION_OBSERVE, json!({"workspace_path":workspace,"head_sha":status.head_sha,"evidence":prior.evidence,"deadline_ms":deadline.0}))?;
            if observed["gates_passed"] != true {
                return Ok(RefreshStep::Adopted { waiting: true });
            }
        }
        return Ok(RefreshStep::Adopted { waiting: false });
    }
    if !diagnose
        || status.is_draft
        || status.merge_state_status.as_deref() != Some("BLOCKED")
        || !matches!(classify_pr_state(status), PrMergeState::Blocked(_))
        || !status.status_checks.as_ref().is_some_and(|checks| {
            checks
                .iter()
                .all(|check| check.state == Some(CheckState::Passed))
        })
    {
        return Ok(RefreshStep::None);
    }
    match &status.review_decision {
        super::pr::ReviewDecision::NotApplicable => {}
        super::pr::ReviewDecision::Reported(value) if value == "APPROVED" => {}
        _ => return Err(refused("required or unreadable review decision")),
    }
    let number = status
        .number
        .ok_or_else(|| refused("PR number unavailable"))?
        .to_string();
    let deadline = Deadline(deadline_ms);
    deadline.remaining()?;
    let input = json!({"workspace_path":workspace,"pr":number,"head_sha":status.head_sha,"head_branch":status.head_branch,"base_branch":status.base_branch,"accepted_base":candidate.base.commit,"deadline_ms":deadline_ms});
    let response =
        host.run_private_vcs_operation(operations::CI_EMISSION_EVIDENCE, input.clone())?;
    if response["evidence"].is_null() {
        return Ok(RefreshStep::None);
    }
    let evidence = response["evidence"].clone();
    let field = |key: &str| {
        evidence[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| refused("incomplete emission proof"))
    };
    let request = PrBaseUpdateReservation {
        execution: execution.into(),
        repository: field("repository")?,
        pr_number: field("pr_number")?,
        head_sha: field("head_sha")?,
        head_branch: field("head_branch")?,
        base_branch: field("base_branch")?,
        base_sha: field("base_sha")?,
        deadline_ms,
        evidence,
    };
    if request.repository != candidate.repository
        || request.head_sha != candidate.candidate.commit
        || request.head_branch != candidate.source_branch
        || request.base_branch != candidate.base_branch
    {
        return Err(refused("emission proof differs from accepted identity"));
    }
    let admission = match handoff_id {
        Some(id) => host.admit_handoff_base_update(id, &request)?,
        None => host.admit_pr_base_update(tasks, &request)?,
    };
    match admission {
        PrBaseUpdateAdmission::Refused(why) => Err(refused(&why)),
        PrBaseUpdateAdmission::Observe(previous) => {
            Deadline(previous.deadline_ms).remaining()?;
            Ok(RefreshStep::Wait)
        }
        PrBaseUpdateAdmission::Reserved => {
            let fresh = host.run_private_vcs_operation(operations::CI_EMISSION_EVIDENCE, input)?;
            if fresh["evidence"] != request.evidence {
                return Err(refused(
                    "emission policy, head, base or workflow changed after reservation",
                ));
            }
            deadline.remaining()?;
            if let Some(id) = handoff_id
                && !matches!(
                    host.admit_handoff_base_update(id, &request)?,
                    PrBaseUpdateAdmission::Observe(_)
                )
            {
                return Err(refused(
                    "handoff base-update reservation changed before mutation",
                ));
            }
            let _reply = host.run_private_vcs_operation(operations::PR_UPDATE_BRANCH, json!({"workspace_path":workspace,"pr":number,"expected_head_sha":request.head_sha}));
            // Even an error may be an applied write with a lost response.
            let observed = read_pr_status_before(host, workspace, &number, deadline_ms)?;
            if observed.head_sha.as_deref() != Some(&request.head_sha) {
                super::landing_checks::verify_published_base_update(
                    workspace, candidate, &observed,
                )?;
                return Ok(RefreshStep::Changed);
            }
            Ok(RefreshStep::Wait)
        }
    }
}
fn refused(reason: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "absent required-context recovery refused: {reason}"
    ))
}

/// Owner-local completion has no accepted-handoff row. Its existing exact
/// published/review pins still supply the candidate; Git supplies its base.
/// Missing checkpoints leave recovery unavailable, never weaken those pins.
pub(super) fn owner_candidate(
    workspace: &Path,
    input: &Value,
    pin: &DeliveryPin,
    status: &PullRequestStatus,
) -> Result<Option<HandoffCandidate>, OrbitError> {
    let (Some(head), Some(branch), Some(base), Some(number)) = (
        pin.candidate_sha(),
        pin.head(),
        input["base"].as_str(),
        status.number,
    ) else {
        return Ok(None);
    };
    let base = base.strip_prefix("origin/").unwrap_or(base);
    let origin = super::git::git_output(workspace, &["remote", "get-url", "origin"])?;
    let Some(repository) = super::claim::slug(&origin) else {
        return Ok(None);
    };
    let base_ref = super::git::resolve_worktree_start_point(
        workspace,
        base,
        super::git::base_sync_mode_from_input(input)?,
    )?;
    let base = super::git::git_output(workspace, &["merge-base", head, &base_ref])?;
    Ok(Some(HandoffCandidate {
        repository,
        source_branch: branch.into(),
        base_branch: status
            .base_branch
            .clone()
            .ok_or_else(|| refused("base branch unavailable"))?,
        landing_branch: status
            .base_branch
            .clone()
            .ok_or_else(|| refused("landing branch unavailable"))?,
        candidate: super::review_gate::revision(workspace, head)?,
        base: super::review_gate::revision(workspace, base.trim())?,
        delivery: orbit_types::workflow::handoff::HandoffDelivery::PullRequest { number },
    }))
}
