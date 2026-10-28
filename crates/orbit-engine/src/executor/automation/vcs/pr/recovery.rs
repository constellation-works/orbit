//! Fresh required-check evidence for job-level completion recovery.

use std::path::Path;

use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::landing::checks::{HeadCheckEvidence, head_check_evidence};
use super::answers::{CheckState, read_required_checks};

/// Whether current checks authorize a complete recovery diagnosis.
pub(crate) enum CompletionRecoveryChecks {
    Ready(Value),
    /// Re-observe in a later run without dispatching a repair agent.
    Wait(String),
}

/// Observe every required failure on the current head, independently of the
/// earlier step error. A partial or changing rollup cannot authorize a recovery
/// diagnosis. Completion already spent its bounded watch waiting for settlement;
/// recovery must not start a new watch budget after that deadline.
pub(crate) fn completion_recovery_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    input: &Value,
    task_id: &str,
) -> Result<CompletionRecoveryChecks, String> {
    let pr = super::super::super::input::input_string_field(input, "pr_number")
        .or_else(|| {
            host.get_task(task_id)
                .ok()
                .and_then(|task| task.github_pr_number().map(str::to_owned))
        })
        .ok_or("completion recovery has no pull request identity")?;
    let pr = pr.as_str();
    let required = read_required_checks(host, workspace, pr).map_err(|e| e.to_string())?;
    if required.reported_head_sha.as_deref() != Some(required.head_sha.as_str()) {
        return Err("completion recovery required checks do not identify the current head".into());
    }
    if required
        .checks
        .iter()
        .any(|check| matches!(check.state, None | Some(CheckState::Pending)))
    {
        return Ok(CompletionRecoveryChecks::Wait(
            "completion recovery required checks have not all concluded; wait for a complete failure diagnosis".into(),
        ));
    }
    let mut failures = Vec::new();
    for check in required
        .checks
        .iter()
        .filter(|check| check.state == Some(CheckState::Failed))
    {
        let name = check
            .name
            .as_deref()
            .ok_or("required failure has no name")?;
        let failed = (check.workflow_name.clone(), name.to_string());
        let detail = match head_check_evidence(
            host,
            workspace,
            &[task_id.to_string()],
            pr,
            &required.head_sha,
            input
                .get("base_branch")
                .or_else(|| input.get("base"))
                .and_then(Value::as_str),
            &[failed],
            &required,
        ) {
            HeadCheckEvidence::Repair { evidence, .. } => evidence,
            HeadCheckEvidence::Stop(reason) | HeadCheckEvidence::Inconclusive(reason) => reason,
            HeadCheckEvidence::WaitForBase(reason) => {
                return Ok(CompletionRecoveryChecks::Wait(reason));
            }
        };
        failures.push(json!({"name":name,"workflow":check.workflow_name,"evidence":detail}));
    }
    // Jobs/logs can take time to read. Refuse evidence for a replaced head or
    // a rerun that changed the required set while those reads were in flight.
    let after = read_required_checks(host, workspace, pr).map_err(|e| e.to_string())?;
    if after.head_sha != required.head_sha
        || after.reported_head_sha != required.reported_head_sha
        || after.checks != required.checks
    {
        return Err("completion recovery required checks changed while collecting evidence".into());
    }
    Ok(CompletionRecoveryChecks::Ready(
        json!({"head_sha":required.head_sha,"failed_checks":failures}),
    ))
}
