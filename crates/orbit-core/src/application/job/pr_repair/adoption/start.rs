use super::super::ledger::OutOfRunStart;
use super::pass_types::Repair;
use super::{PassReads, RepairAuthority, Verdict};
use crate::OrbitRuntime;
use orbit_types::workflow::{DeliveryRefusal, JobRun, PrAutoFixBlocker};
use serde_json::json;

impl OrbitRuntime {
    /// Whether checkpoint resume accepts `run` rewound for `repair`.
    pub(in crate::application::job::pr_repair) fn plan_repair(
        &self,
        run: &JobRun,
        repair: Repair,
        reads: &mut PassReads,
    ) -> Verdict {
        // Resume planning reports an unloadable activity catalog as a
        // validation refusal; load it first so that case reads as one.
        let catalog = reads.catalog.get_or_insert_with(|| {
            self.v2_activity_catalog()
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        if let Err(error) = catalog {
            return Verdict::Skip(
                "resume_unplannable",
                format!("load the activity catalog: {error}"),
            );
        }
        match self.plan_completion_repair_resume(&run.run_id, &repair.marker) {
            Ok(_) => Verdict::Repair(Box::new(repair)),
            Err(error) if error.job_validation_message().is_some() => {
                Verdict::Skip("not_resumable", error.to_string())
            }
            Err(error) => Verdict::Skip("resume_unplannable", error.to_string()),
        }
    }

    /// Resume `run` to repair `repair`, re-checking first that its task has
    /// not moved and nothing of it started since the verdict.
    pub(in crate::application::job::pr_repair) fn start_repair(
        &self,
        run: &JobRun,
        authority: &RepairAuthority<'_>,
        repair: Repair,
    ) -> Verdict {
        match self.get_task(&repair.task_id) {
            Ok(task) if task.status == repair.status => {
                if let Some(live) = self.live_task_delivery(&task) {
                    return Verdict::Skip("live_run", live);
                }
            }
            Ok(task) => {
                return Verdict::Skip(
                    "task_moved",
                    format!("the task moved to {} during the pass", task.status),
                );
            }
            Err(error) => {
                return Verdict::Skip("store_unreadable", format!("read the task: {error}"));
            }
        }
        let invoke =
            match self.submit_completion_repair_run(&run.run_id, &repair.marker, authority.actor) {
                Ok(invoke) => invoke,
                Err(error) => return Verdict::Skip("resume_failed", error.to_string()),
            };
        tracing::info!(
            target: "orbit.core.drain",
            source_run_id = %run.run_id,
            resumed_run_id = %invoke.run_id,
            task_id = %repair.task_id,
            pr = %repair.pr_number,
            blocker = repair.blocker.map(PrAutoFixBlocker::as_str),
            hold_code = repair.hold.map(DeliveryRefusal::error_code),
            drain_run_id = authority.drain_run_id,
            "handed a pull request back to its delivery for an auto-fix",
        );
        self.record_out_of_run_start(
            &repair.task_id,
            &OutOfRunStart {
                pr_number: repair.pr_number.clone(),
                head_sha: repair.head_sha.clone(),
                blocker: repair.blocker,
                hold_code: repair.hold.map(|hold| hold.error_code().to_string()),
                source_run_id: run.run_id.clone(),
                resumed_run_id: invoke.run_id.clone(),
                drain_run_id: authority.drain_run_id.map(str::to_string),
            },
        );
        Verdict::Started(json!({
            "task_id": repair.task_id,
            "source_run_id": run.run_id,
            "resumed_run_id": invoke.run_id,
            "pr_number": repair.pr_number,
            "head_sha": repair.head_sha,
            "blocker": repair.blocker.map(PrAutoFixBlocker::as_str),
            "hold_code": repair.hold.map(DeliveryRefusal::error_code),
        }))
    }
}
