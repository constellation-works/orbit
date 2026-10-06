//! A pipeline worker's part in an Orbit upgrade.
//!
//! A detached pipeline worker (`orbit job run-pipeline-worker`) joins the
//! generation authority as a drain participant. It never holds a newer
//! generation back from the middle of a step:
//!
//! - **Yield.** At every top-level step boundary, and at every admission
//!   pass of a drain coordinator, the worker asks whether a newer,
//!   incompatible generation is pending on its authority. When one is, the
//!   run it owns is recorded `interrupted` under
//!   [`UPGRADE_QUIESCE_ERROR_CODE`] — resumable from its checkpoints with
//!   `orbit job resume`, except a claimed leaf, which generic resume refuses
//!   and whose owner recovers the claim for a later drain to re-admit — and
//!   the process exits, releasing its share of the generation. A drain
//!   coordinator also admits no new leaves while a switch is pending, since
//!   every one of them would be refused at startup.
//! - **Hand over.** A drain coordinator whose installed executable was
//!   replaced by a build that can adopt its run execs that executable in
//!   place with the same arguments and [`ADOPT_RUN_ENV`] naming the run. The
//!   new image keeps the pid (and so the run's recorded owner identity) and
//!   resumes the run from its checkpoints instead of claiming it afresh.

use std::ffi::OsStr;
use std::sync::OnceLock;

use chrono::Utc;
use orbit_common::fs::generation::{
    ParticipantRole, PendingSwitch, RESUME_DRAIN_ADOPT, handover_target,
    pending_switch_for_this_process, process_participation, reexec,
};
use orbit_store::contracts::TaskReservationReleaseReason;
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::JobRunState;

use crate::OrbitRuntime;

/// Names the run a handed-over worker image adopts. Only honoured by the
/// process already recorded as that run's owner.
pub(crate) const ADOPT_RUN_ENV: &str = "ORBIT_ADOPT_PIPELINE_RUN";

/// Error code of a run its worker interrupted to let an upgrade proceed.
pub(crate) const UPGRADE_QUIESCE_ERROR_CODE: &str = "upgrade_quiesce";

/// The run this worker process owns, once it started executing it.
static WORKER_RUN: OnceLock<String> = OnceLock::new();

/// Record the run this worker process executes. Only the first binding
/// counts: a worker executes exactly one run.
pub(crate) fn bind_worker_run(run_id: &str) {
    let _ = WORKER_RUN.set(run_id.to_string());
}

/// Whether this process was handed `run_id` by its previous image.
pub(crate) fn adopts_run(run_id: &str) -> bool {
    std::env::var_os(ADOPT_RUN_ENV).as_deref() == Some(OsStr::new(run_id))
}

/// This worker's run and the switch it must yield to, when both exist.
fn pending_switch_for_worker() -> Option<(&'static str, PendingSwitch)> {
    let run_id = WORKER_RUN.get()?;
    let (_, role) = process_participation()?;
    if role != ParticipantRole::Drain {
        return None;
    }
    pending_switch_for_this_process().map(|switch| (run_id.as_str(), switch))
}

impl OrbitRuntime {
    /// Step-boundary check: yield this worker's run to a pending switch.
    /// Returns only when there is nothing to yield to.
    pub(crate) fn yield_run_at_step_boundary(&self, run_id: &str) {
        if let Some((owned, switch)) = pending_switch_for_worker()
            && owned == run_id
        {
            self.yield_to_pending_switch(owned, &switch);
        }
    }

    /// Admission-pass check for a drain coordinator: yield to a pending
    /// switch, or hand over to a replaced installation that can adopt the
    /// run. Neither returns; a failed handover is logged and the drain keeps
    /// running the replaced image.
    pub(crate) fn drain_upgrade_boundary(&self) {
        let Some(run_id) = WORKER_RUN.get() else {
            return;
        };
        if !matches!(process_participation(), Some((_, ParticipantRole::Drain))) {
            return;
        }
        if let Some((owned, switch)) = pending_switch_for_worker() {
            self.yield_to_pending_switch(owned, &switch);
        }
        let Some(installed) = handover_target(Some(RESUME_DRAIN_ADOPT)) else {
            return;
        };
        tracing::info!(
            target: "orbit.generation",
            run_id,
            installed = %installed.display(),
            "the installed Orbit executable was replaced; handing this drain over to it",
        );
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        let error = reexec(&installed, &args, &[(ADOPT_RUN_ENV, OsStr::new(run_id))]);
        tracing::warn!(
            target: "orbit.generation",
            run_id,
            %error,
            "drain handover failed; this drain keeps running the replaced image",
        );
    }

    /// Record `run_id` interrupted for `switch` and exit the process.
    fn yield_to_pending_switch(&self, run_id: &str, switch: &PendingSwitch) -> ! {
        self.record_upgrade_interruption(run_id, switch.pid, switch.role);
        std::process::exit(0)
    }

    /// Record `run_id` interrupted so the process `pid` can switch the store
    /// generation. A failure to record is logged and otherwise ignored:
    /// reconciliation records a run whose owner is gone as interrupted all the
    /// same.
    pub(crate) fn record_upgrade_interruption(
        &self,
        run_id: &str,
        pid: u32,
        role: ParticipantRole,
    ) {
        let message = self.upgrade_interruption_message(run_id, pid, role);
        tracing::warn!(target: "orbit.generation", run_id, "{message}");
        if let Err(error) = self.interrupt_worker_run(run_id, &message) {
            tracing::warn!(
                target: "orbit.generation",
                run_id,
                %error,
                "could not record the upgrade interruption",
            );
        }
    }

    /// The diagnostic recorded on an upgrade-interrupted run.
    ///
    /// A claimed leaf is refused by generic resume, so it must not be pointed
    /// at `orbit job resume`: its recovery is on the owner, which re-admits it
    /// through a later drain. When the store cannot say whether the run is a
    /// claimed leaf, the wording stays neutral instead of guessing a command
    /// that may be refused.
    fn upgrade_interruption_message(
        &self,
        run_id: &str,
        pid: u32,
        role: ParticipantRole,
    ) -> String {
        let claimed = self
            .stores()
            .jobs()
            .local_pull_for_run(run_id)
            .map(|admission| admission.is_some());
        let next = match claimed {
            Ok(false) => {
                format!("resume it with `orbit job resume {run_id}` once the upgrade completes")
            }
            Ok(true) => {
                "this claimed leaf cannot be resumed directly; once the upgrade completes, \
                 recover the claim on the owner and let a drain re-admit it"
                    .to_string()
            }
            Err(_) => {
                "once the upgrade completes, inspect the run before choosing how to recover it"
                    .to_string()
            }
        };
        format!(
            "interrupted at a step boundary by an Orbit upgrade so a newer Orbit (pid {pid}, {role}) \
             can switch the store generation; {next}"
        )
    }

    fn interrupt_worker_run(
        &self,
        run_id: &str,
        message: &str,
    ) -> Result<(), orbit_common::OrbitError> {
        let Some(run) = self.stores().jobs().get_job_run(run_id)? else {
            return Ok(());
        };
        let finished_at = Utc::now();
        let started_at = run.started_at.unwrap_or(run.scheduled_at);
        let duration_ms = u64::try_from(
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0),
        )
        .ok();
        let changed = self.finalize_job_run_with_reservation_cleanup_and_diagnostic(
            run_id,
            JobRunState::Interrupted,
            finished_at,
            duration_ms,
            TaskReservationReleaseReason::RunTerminal,
            Some((UPGRADE_QUIESCE_ERROR_CODE, message)),
        )?;
        if !changed {
            return Ok(());
        }
        self.record_pipeline_diagnostic_step(
            &run,
            started_at,
            finished_at,
            Some(UPGRADE_QUIESCE_ERROR_CODE),
            message,
            JobRunState::Interrupted,
        )?;
        self.record_event(OrbitEvent::JobRunCompleted {
            job_id: run.job_id.clone(),
            run_id: run.run_id.clone(),
            state: JobRunState::Interrupted.to_string(),
        })
    }
}
