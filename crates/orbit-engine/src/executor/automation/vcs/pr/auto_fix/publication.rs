use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{PrAutoFixBlocker, pr_auto_fix_attempts};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::input::input_string_field;
use super::super::super::candidate_validate::candidate_validate;
use super::super::super::freshness::{commit_sha, remote_branch_sha};
use super::super::super::git::{
    base_sync_mode_from_input, git_command_success, git_output, resolve_worktree_start_point,
};
use super::super::super::push::push_batch_changes;
use super::super::delivery::DeliveryPin;
use super::{admission, ledger, verdict};

/// What completion found on the branch before it read the pull request.
pub(in crate::executor::automation::vcs::pr) enum LocalRepair {
    /// Nothing to publish.
    None,
    /// The repair was validated and published as this head.
    Published(String),
    /// The repair sits on a reviewed head: validated, unpublished, and
    /// handed to the re-review round as this checkpoint.
    ForReview(Value),
}

/// Publish a check repair the executor committed on top of the pinned head.
///
/// Acts only when this execution or a retry ancestor holds an admitted
/// check repair of exactly the pinned head and the checkout is that branch,
/// strictly ahead of it.
/// Anything else (an interrupted rebase, a detached checkout, another
/// writer's commit) is left to the ordinary completion path, which refuses
/// what it does not pin. The repair passes the workspace's required
/// validation before it moves anything, and the push is a fast-forward of
/// the exact published head.
pub(in crate::executor::automation::vcs::pr) fn publish_local_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    auto_fix: &admission::AutoFix,
    workspace_path: &str,
    pr_number: &str,
    pin: &DeliveryPin,
    reviewed: bool,
) -> Result<LocalRepair, OrbitError> {
    let (Some(authorized), Some(head)) = (pin.candidate_sha(), input_string_field(input, "head"))
    else {
        return Ok(LocalRepair::None);
    };
    let workspace = Path::new(workspace_path);
    let history = host.get_task_history(auto_fix.primary_task()?)?;
    let holds_repair = pr_auto_fix_attempts(&history, pr_number)
        .iter()
        .any(|attempt| {
            auto_fix.ledger_runs.contains(&attempt.run_id)
                && attempt.head_sha == authorized
                && attempt.blocker == PrAutoFixBlocker::FailedChecks
        });
    if !holds_repair {
        return Ok(LocalRepair::None);
    }
    let local = commit_sha(workspace, "HEAD")?;
    let branch = git_output(workspace, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    if local == authorized
        || branch.as_deref() != Some(head.as_str())
        || !git_command_success(
            workspace,
            &["merge-base", "--is-ancestor", authorized, &local],
        )?
    {
        return Ok(LocalRepair::None);
    }
    let remote = remote_branch_sha(workspace, &head)?;
    if remote.as_deref() != Some(authorized) {
        return Err(verdict::stopped(
            pr_number,
            &format!(
                "published branch 'origin/{head}' moved away from {authorized} while its repair \
                 ran, so the repair is not published over it"
            ),
        ));
    }
    let base = input_string_field(input, "base").ok_or_else(|| {
        verdict::stopped(
            pr_number,
            "the completion step carries no base to validate against",
        )
    })?;
    let base_ref =
        resolve_worktree_start_point(workspace, &base, base_sync_mode_from_input(input)?)?;
    let base_sha = git_output(workspace, &["merge-base", authorized, &base_ref])?;
    candidate_validate(
        host,
        &json!({
            "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
            "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
            "workspace_path": workspace_path,
            "base_sha": base_sha,
            "base_ref": base_ref,
        }),
    )?;
    if reviewed {
        return Ok(LocalRepair::ForReview(json!({
            "head": head,
            "head_sha": local,
            "head_sha_before": authorized,
            "remote_sha_before": authorized,
            "base": base,
            "base_ref": base_ref,
            "base_sha": base_sha,
            "rewritten": false,
            "auto_fix": true,
        })));
    }
    push_batch_changes(
        host,
        &json!({
            "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
            "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
            "workspace_path": workspace_path,
            "branch": head,
            "expected_remote_sha": authorized,
        }),
    )?;
    ledger::record_published_repair(
        host,
        auto_fix,
        pr_number,
        authorized,
        &local,
        PrAutoFixBlocker::FailedChecks,
    )?;
    Ok(LocalRepair::Published(local))
}
