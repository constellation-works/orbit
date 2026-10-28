use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    DeliveryRefusal, PrAutoFixAttempt, PrAutoFixBlocker, PrAutoFixDecision,
};
use serde_json::Value;

use crate::context::{PrAutoFixAdmission, PrAutoFixRequest, RuntimeHost};

use super::super::super::super::input::input_string_field;
use super::{ledger, verdict};

/// What an auto-fix run may do, read once per completion step.
pub(in crate::executor::automation::vcs::pr) struct AutoFix {
    pub(in crate::executor::automation::vcs::pr) run_id: String,
    /// Current execution and authenticated retry ancestors, never siblings.
    pub(super) ledger_runs: BTreeSet<String>,
    pub(in crate::executor::automation::vcs::pr) task_ids: Vec<String>,
    allowed_crews: Value,
    /// `--complete` was granted too: completion may merge once mergeable.
    merge_authorized: bool,
}

impl AutoFix {
    /// `Some` when the step's input carries `auto_fix: true`.
    pub(in crate::executor::automation::vcs::pr) fn from_input<H: RuntimeHost + ?Sized>(
        host: &H,
        input: &Value,
        task_ids: &[String],
        batch_id: &str,
    ) -> Result<Option<Self>, OrbitError> {
        if input.get("auto_fix").and_then(Value::as_bool) != Some(true) {
            return Ok(None);
        }
        let run_id = input_string_field(input, "run_id").unwrap_or_else(|| batch_id.to_string());
        let ledger_runs = ledger::repair_ledger_runs(host, &run_id)?;
        Ok(Some(Self {
            run_id,
            ledger_runs,
            task_ids: task_ids.to_vec(),
            allowed_crews: input.get("allowed_crews").cloned().unwrap_or(Value::Null),
            merge_authorized: input.get("completion").and_then(Value::as_str) == Some("done"),
        }))
    }

    /// Repair only: never request a merge or complete a task.
    pub(in crate::executor::automation::vcs::pr) fn watch_only(&self) -> bool {
        !self.merge_authorized
    }

    pub(in crate::executor::automation::vcs::pr) fn primary_task(
        &self,
    ) -> Result<&str, OrbitError> {
        self.task_ids
            .iter()
            .min()
            .map(String::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "auto_fix: the pull request names no task to keep its repair ledger on"
                        .to_string(),
                )
            })
    }
}

/// Ask the host to admit one repair of `head_sha`, returning the attempt
/// the repair runs as. Every refusal is a stop with its diagnostics.
pub(in crate::executor::automation::vcs::pr) fn admit<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    head_sha: &str,
    blocker: PrAutoFixBlocker,
) -> Result<PrAutoFixAttempt, OrbitError> {
    let admission = host.admit_pr_auto_fix(&PrAutoFixRequest {
        run_id: auto_fix.run_id.clone(),
        task_ids: auto_fix.task_ids.clone(),
        pr_number: pr_number.to_string(),
        head_sha: head_sha.to_string(),
        blocker,
        allowed_crews: auto_fix.allowed_crews.clone(),
    })?;
    let decision = match admission {
        PrAutoFixAdmission::Refused(reason) => return Err(verdict::stopped(pr_number, &reason)),
        PrAutoFixAdmission::Decided(decision) => decision,
    };
    match decision {
        PrAutoFixDecision::Admit(attempt) | PrAutoFixDecision::Continue(attempt) => {
            tracing::info!(
                pr_number,
                head_sha,
                blocker = blocker.as_str(),
                attempt = attempt.attempt,
                budget = attempt.budget,
                crew = %attempt.crew,
                crew_source = %attempt.crew_source,
                "auto_fix: repair admitted"
            );
            Ok(attempt)
        }
        PrAutoFixDecision::Duplicate(previous) => Err(verdict::hold_auto_fix_stop(
            verdict::stopped(
                pr_number,
                &format!(
                    "a {} repair was already dispatched for head {head_sha} as attempt {} of {} by \
                 run {}, and that head is still the one blocked",
                    blocker.as_str(),
                    previous.attempt,
                    previous.budget,
                    previous.run_id
                ),
            ),
            DeliveryRefusal::RepairDispatched,
        )),
        PrAutoFixDecision::Exhausted(attempts) => {
            let spent = attempts
                .iter()
                .map(|attempt| {
                    format!(
                        "{} on {} by run {} with crew '{}'",
                        attempt.blocker.as_str(),
                        attempt.head_sha,
                        attempt.run_id,
                        attempt.crew
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            Err(verdict::stopped(
                pr_number,
                &format!(
                    "auto_fix_budget_exhausted: its {} repairs are spent ({spent})",
                    attempts.len()
                ),
            ))
        }
    }
}
