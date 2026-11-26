//! A pipeline worker's part in an Orbit upgrade: the run it bound, the switch
//! it must yield to and the handover variable. The yield and the handover
//! themselves finish through job-run finalization, so they are `orbit-core`'s
//! (`application::job::upgrade_resume`).
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
//!   place with the same arguments and `ADOPT_RUN_ENV` naming the run. The
//!   new image keeps the pid (and so the run's recorded owner identity) and
//!   resumes the run from its checkpoints instead of claiming it afresh.

use std::ffi::OsStr;
use std::sync::OnceLock;

use orbit_common::fs::generation::{
    ParticipantRole, PendingSwitch, pending_switch_for_this_process, process_participation,
};

/// Names the run a handed-over worker image adopts. Only honoured by the
/// process already recorded as that run's owner.
pub use orbit_common::env_vars::ORBIT_ADOPT_PIPELINE_RUN as ADOPT_RUN_ENV;

/// Error code of a run its worker interrupted to let an upgrade proceed.
pub const UPGRADE_QUIESCE_ERROR_CODE: &str = "upgrade_quiesce";

/// The run this worker process owns, once it started executing it.
static WORKER_RUN: OnceLock<String> = OnceLock::new();

/// Record the run this worker process executes. Only the first binding
/// counts: a worker executes exactly one run.
pub fn bind_worker_run(run_id: &str) {
    let _ = WORKER_RUN.set(run_id.to_string());
}

/// Whether this process was handed `run_id` by its previous image.
pub fn adopts_run(env: &orbit_common::Env, run_id: &str) -> bool {
    env.orbit_adopt_pipeline_run() == Some(OsStr::new(run_id))
}

/// The run this worker process bound with [`bind_worker_run`], if any.
pub fn bound_worker_run() -> Option<&'static str> {
    WORKER_RUN.get().map(String::as_str)
}

/// This worker's run and the switch it must yield to, when both exist.
pub fn pending_switch_for_worker() -> Option<(&'static str, PendingSwitch)> {
    let run_id = WORKER_RUN.get()?;
    let (_, role) = process_participation()?;
    if role != ParticipantRole::Drain {
        return None;
    }
    pending_switch_for_this_process().map(|switch| (run_id.as_str(), switch))
}
