use orbit_core::app::run::PullSettlementOutcome;
use orbit_core::app::run::WorkflowDispatchResult;
use std::fmt::Write as _;

use clap::Args;
use orbit_common::OrbitError;
use orbit_core::app::run::{AutoDrainSubmissionReceipt, ShipSubmissionReceipt};
use serde_json::{Value, json};

use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub(super) struct WaitArgs {
    /// Wait for a terminal run state for up to 3600 seconds by default.
    /// Exit nonzero on wait expiry or an unsuccessful run; expiry reports
    /// wait_timeout=true with the actual run state and leaves the run active.
    #[arg(long)]
    wait: bool,
    /// Wait deadline in seconds (0..=86400); requires --wait.
    #[arg(long, requires = "wait")]
    timeout_seconds: Option<u64>,
}

impl WaitArgs {
    /// Validate before submission so an invalid deadline cannot start work.
    pub(super) fn timeout_seconds(&self) -> Result<Option<u64>, OrbitError> {
        let seconds = orbit_core::app::run::wait_timeout(self.timeout_seconds)?;
        Ok(self.wait.then_some(seconds))
    }
}

/// Render dispatched workflow runs, failing the command when any waited run
/// ended in a non-success terminal state or its wait expired. Submitted and queued runs report no
/// outcome yet, so they keep a zero exit.
pub(crate) fn workflow_dispatch_payload(
    workflow_alias: &'static str,
    runs: &[WorkflowDispatchResult],
) -> CommandOut {
    let doc = if runs.len() == 1 {
        workflow_dispatch_result_to_json(&runs[0])
    } else {
        json!({
            "workflow": workflow_alias,
            "runs": runs
                .iter()
                .map(workflow_dispatch_result_to_json)
                .collect::<Vec<_>>(),
        })
    };
    let lines = runs
        .iter()
        .flat_map(workflow_dispatch_result_lines)
        .collect::<Vec<_>>();
    let payload = Payload::detail(doc, lines.join("\n"));
    if runs.iter().any(WorkflowDispatchResult::failed) {
        return Ok(payload.with_exit_code(1).into());
    }
    Ok(payload.into())
}

pub(super) fn ship_submission_payload(receipt: &ShipSubmissionReceipt) -> CommandOut {
    let mut lines = submitted_workflow_lines(
        receipt.workflow(),
        &receipt.job_id,
        &receipt.run_id,
        receipt.state,
    );
    lines.extend(
        receipt
            .warnings()
            .iter()
            .map(|warning| format!("Warning: {warning}")),
    );
    lines.extend(receipt.notes().iter().map(|note| format!("Note: {note}")));
    Ok(Payload::detail(receipt.to_json(), lines.join("\n")).into())
}

pub(super) fn auto_drain_submission_payload(receipt: &AutoDrainSubmissionReceipt) -> CommandOut {
    let doc = receipt.to_json();
    let mut lines = submitted_workflow_lines(
        receipt.workflow(),
        &receipt.job_id,
        &receipt.run_id,
        receipt.state,
    );
    lines.extend(
        receipt
            .warnings()
            .iter()
            .map(|warning| format!("Warning: {warning}")),
    );
    lines.extend(receipt.notes().iter().map(|note| format!("Note: {note}")));
    if let Some((mode, source)) = receipt.auto_concurrency_summary() {
        lines.push(format!("Auto-concurrency: {mode} ({source})."));
    }
    Ok(Payload::detail(doc, lines.join("\n")).into())
}

pub(super) fn workflow_dispatch_result_to_json(run: &WorkflowDispatchResult) -> Value {
    let value = json!({
        "workflow": run.workflow_alias,
        "job_id": run.job_id,
        "run_id": run.run_id,
        "state": run.state,
        "wait_timeout": run.wait_timeout,
        "attempt": run.attempt,
        "error_code": run.error_code,
        "error_message": run.error_message,
    });
    value
}

fn submitted_workflow_lines(
    workflow: &str,
    job_id: &str,
    run_id: &str,
    state: &str,
) -> Vec<String> {
    vec![
        format!("Workflow: {workflow}"),
        format!("Job ID: {job_id}"),
        format!("Run ID: {run_id}"),
        format!("State: {state}"),
        format!("Inspect: orbit run history -j {job_id} | orbit run show {run_id}"),
    ]
}

fn workflow_dispatch_result_lines(run: &WorkflowDispatchResult) -> Vec<String> {
    if matches!(run.state.as_str(), "submitted" | "queued")
        && run.error_code.is_none()
        && run.error_message.is_none()
    {
        return submitted_workflow_lines(run.workflow_alias, &run.job_id, &run.run_id, &run.state);
    }

    let error_code = run.error_code.clone().unwrap_or_else(|| "-".to_string());
    let error_message = single_line(run.error_message.as_deref().unwrap_or("-"));
    let mut first = format!(
        "workflow={};job_id={};run_id={};state={};attempt={}",
        run.workflow_alias, run.job_id, run.run_id, run.state, run.attempt
    );
    let _ = write!(
        first,
        ";error_code={};error_message={}",
        single_line(&error_code),
        error_message
    );
    if run.wait_timeout {
        first.push_str(";wait_timeout=true");
    }

    vec![first]
}

fn single_line(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

/// Human lines for the pull settlements a cancel or stop carried
/// [ORB-13663]. Empty when the pass touched nothing.
pub(super) fn pull_settlement_lines(
    entries: &[orbit_core::app::run::PullSettlementEntry],
) -> Vec<String> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "Pull settlements ({} admission{}):",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" }
    )];
    lines.extend(
        entries
            .iter()
            .map(|entry| format!("  {}", entry.describe())),
    );
    if entries.iter().any(|entry| {
        matches!(
            entry.outcome,
            PullSettlementOutcome::PendingDelivery
                | PullSettlementOutcome::OwnerUnreachable
                | PullSettlementOutcome::Pending
        )
    }) {
        lines.push(
            "Undelivered settlements stay recorded; rerun `orbit run auto --stop` once the owner \
             is reachable."
                .to_string(),
        );
    }
    lines
}
