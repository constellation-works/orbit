use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{DeliveryRefusal, PrAutoFixBlocker};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::super::super::input::input_string_field;
use super::super::super::freshness::commit_sha;
use super::super::super::git::{
    base_sync_mode_from_input, git_output, resolve_worktree_start_point,
};
use super::super::super::landing::checks::{
    HeadCheckEvidence, head_check_evidence, unmoved_red_base,
};
use super::super::answers::{CheckState, PullRequestStatus, RequiredChecks, read_required_checks};
use super::{admission, ledger, verdict};

/// A required failure the base's own job already fails the same way, while
/// the base branch has not moved past that commit, or has moved to a commit
/// that cannot prove yet whether the base fixed it: its same job has not
/// concluded, did not run to a result, or cannot be read.
///
/// `Some(reason)` waits for the base and spends no repair attempt; the next
/// poll evaluates it again. `None` keeps the ordinary repair path, including
/// a candidate-caused failure and a moved tip whose same jobs ran to success
/// or failure. Only required failures take part in the hold.
///
/// Git state or evidence this classification could not read (a base that
/// cannot be resolved, a head object that is not local, an unreadable job or
/// log) is not itself a red base. It holds only when an earlier poll already
/// recorded this head's failure as inherited (`pr_auto_fix_base_red`): a
/// read gap must not turn that recorded inheritance into a repair.
pub(in crate::executor::automation::vcs::pr) fn red_base_wait<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
) -> Option<String> {
    let head = status.head_sha.as_deref()?;
    match evaluate_red_base(
        host,
        auto_fix,
        input,
        workspace_path,
        pr_number,
        head,
        failed,
    ) {
        Ok(hold) => hold,
        Err(gap) => recorded_base_red(host, auto_fix, head).then(|| {
            tracing::warn!(pr_number, head, %gap, "auto-fix kept the recorded base_red hold");
            format!(
                "base_red: head {head} was recorded failing as its base does, and this poll \
                 could not read the base evidence again, so no repair agent runs on this poll"
            )
        }),
    }
}

/// [`red_base_wait`]'s reading: `Ok(Some)` holds, `Ok(None)` does not, and
/// `Err` names what could not be read.
fn evaluate_red_base<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    head: &str,
    failed: &[(Option<String>, String)],
) -> Result<Option<String>, String> {
    // [ORB-16416] An optional failure that no job answers would otherwise
    // switch the hold off for a required inherited failure beside it.
    let (failed, _) = required_failures(host, workspace_path, pr_number, head, failed)?;
    let base_name =
        input_string_field(input, "base").ok_or_else(|| "the input names no base".to_string())?;
    let workspace = Path::new(workspace_path);
    let base_ref = resolve_worktree_start_point(
        workspace,
        &base_name,
        base_sync_mode_from_input(input).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("base '{base_name}' could not be resolved: {error}"))?;
    let tip = commit_sha(workspace, &base_ref).map_err(|error| error.to_string())?;
    let base = git_output(workspace, &["merge-base", head, &tip])
        .map_err(|error| format!("the fork point of {head} could not be read: {error}"))?;
    unmoved_red_base(
        host,
        workspace,
        head,
        base.trim(),
        &tip,
        base_name.strip_prefix("origin/").unwrap_or(&base_name),
        &auto_fix.task_ids,
        &failed,
        pr_number.parse::<u64>().ok(),
    )
}

/// Whether an earlier poll recorded `head`'s failure as inherited from the
/// base on a task this run watches. An unreadable history is no record.
fn recorded_base_red<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    head: &str,
) -> bool {
    auto_fix.task_ids.iter().any(|task| {
        host.get_task_history(task).is_ok_and(|history| {
            history.iter().any(|entry| {
                entry.event == "pr_auto_fix_base_red"
                    && entry
                        .note
                        .as_deref()
                        .and_then(|note| serde_json::from_str::<Value>(note).ok())
                        .is_some_and(|note| note["failed_head"] == head)
            })
        })
    })
}

/// Read the failed checks on the pinned head and admit their repair. Returns
/// the hand-off failure once admitted, or the stop.
pub(in crate::executor::automation::vcs::pr) fn repair_failed_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    workspace_path: &str,
    pr_number: &str,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
) -> OrbitError {
    let Some(head_sha) = status.head_sha.as_deref() else {
        return verdict::stopped(pr_number, "the pull request reported no head commit");
    };
    let (failed, required) =
        match required_failures(host, workspace_path, pr_number, head_sha, failed) {
            Ok(found) => found,
            Err(reason) => return verdict::stopped(pr_number, &reason),
        };
    let (evidence, annotations) = match head_check_evidence(
        host,
        Path::new(workspace_path),
        &auto_fix.task_ids,
        pr_number,
        head_sha,
        status.base_branch.as_deref(),
        &failed,
        &required,
    ) {
        HeadCheckEvidence::Repair {
            evidence,
            annotations,
        } => (evidence, annotations),
        HeadCheckEvidence::Stop(reason) => return verdict::stopped(pr_number, &reason),
        HeadCheckEvidence::Inconclusive(reason) => {
            return verdict::hold_auto_fix_stop(
                verdict::stopped(pr_number, &reason),
                DeliveryRefusal::CheckEvidenceInconclusive,
            );
        }
        HeadCheckEvidence::WaitForBase(reason) => {
            return verdict::hold_auto_fix_stop(
                verdict::stopped(pr_number, &reason),
                DeliveryRefusal::BaseChecksPending,
            );
        }
    };
    if !annotations.is_empty()
        && let Err(error) =
            ledger::record_check_annotations(host, auto_fix, pr_number, head_sha, annotations)
    {
        return verdict::stopped(
            pr_number,
            &format!("the check annotations a repair would rely on were not retained: {error}"),
        );
    }
    match admission::admit(
        host,
        auto_fix,
        pr_number,
        head_sha,
        PrAutoFixBlocker::FailedChecks,
    ) {
        Ok(attempt) => verdict::check_repair_handoff(pr_number, head_sha, &attempt, &evidence),
        Err(stop) => stop,
    }
}

/// Compare required failures with the current base tip before repair admission.
pub(in crate::executor::automation::vcs::pr) fn base_tip_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
) -> Option<Value> {
    let workspace = Path::new(workspace_path);
    let head = status.head_sha.as_deref()?;
    let (failed, _) = required_failures(host, workspace_path, pr_number, head, failed).ok()?;
    let base = input_string_field(input, "base")?;
    let base_ref =
        resolve_worktree_start_point(workspace, &base, base_sync_mode_from_input(input).ok()?)
            .ok()?;
    let tip = commit_sha(workspace, &base_ref).ok()?;
    super::super::super::landing::checks::base_tip_check_evidence(
        host,
        workspace,
        head,
        base.strip_prefix("origin/").unwrap_or(&base),
        &tip,
        &failed,
    )
}

/// The required failures on a head, with the head-bound read naming them.
type RequiredFailures = (Vec<(Option<String>, String)>, RequiredChecks);

/// The failed checks branch protection requires on `head_sha`. The rollup
/// does not say which checks are required, and a `BLOCKED` state can come
/// from a rule the rollup does not show, so an optional failure beside it
/// would otherwise read as the blocker. Nothing required failing, or an
/// answer that cannot show it, is a stop. The head-bound read is returned
/// with the failures, for the identities of the required checks.
fn required_failures<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
    head_sha: &str,
    failed: &[(Option<String>, String)],
) -> Result<RequiredFailures, String> {
    let required = read_required_checks(host, Path::new(workspace_path), pr_number)
        .map_err(|error| format!("its required checks could not be read: {error}"))?;
    if required.head_sha != head_sha || required.reported_head_sha.as_deref() != Some(head_sha) {
        return Err(format!(
            "its required checks were read on head {} rather than {head_sha}",
            required.head_sha
        ));
    }
    if required.checks.iter().any(|check| {
        check.state != Some(CheckState::Passed) && check.state != Some(CheckState::Failed)
    }) {
        return Err("its required checks have not all concluded; no repair is admitted".into());
    }
    let mut failed = failed
        .iter()
        .filter(|(workflow, name)| required.requires(workflow.as_deref(), name))
        .cloned()
        .collect::<Vec<_>>();
    // The policy read is newer than the status rollup. Include failures that
    // concluded between those reads, so a settled sibling cannot be omitted.
    for check in required
        .checks
        .iter()
        .filter(|check| check.state == Some(CheckState::Failed))
    {
        if let Some(name) = &check.name {
            let identity = (check.workflow_name.clone(), name.clone());
            if !failed.iter().any(|(workflow, failed_name)| {
                failed_name == name && (workflow.is_none() || workflow == &check.workflow_name)
            }) {
                failed.push(identity);
            }
        }
    }
    if failed.is_empty() {
        return Err("only optional checks failed; the merge is blocked by something else".into());
    }
    Ok((failed, required))
}
