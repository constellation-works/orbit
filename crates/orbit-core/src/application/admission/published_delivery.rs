//! Locks for a resumed published delivery.
//!
//! A final-recovery `requeue` that preserved its run's publication sends the
//! task back to `backlog` with a branch and pull request the next run only
//! has to adopt and complete (`OrbitRuntime::published_resume`). That run
//! does not edit the working tree before it adopts, so admission does not
//! hold it behind other tasks' context locks on files its own published diff
//! already changed:
//!
//! - the backlog snapshot, the admission wave and the admission section admit
//!   it past held locks;
//! - its gate, refused an ordinary reservation, takes one that holds no files
//!   ([`OrbitRuntime::deferred_footprint_reservation`]) and tells the leaf it
//!   deferred the footprint;
//! - the leaf takes that footprint like any reservation before its first
//!   agent that may edit — an implementer repairing a conflicting
//!   re-application, a reviewer fix, a step, conflict or check repair —
//!   and waits while other tasks hold it
//!   ([`OrbitRuntime::acquire_deferred_footprint`]).
//!
//! Two resumed runs that deferred behind the same holder both hold their
//! context by status while neither holds a file, so neither counts the other as
//! a holder: whichever reserves first takes the files, and the store refuses
//! the other for as long as that reservation lives.
//!
//! Re-applying the published patch unchanged onto the current base is not an
//! edit: it changes no file the patch did not already change. Once its run
//! starts, the task is `in-progress` and holds its own surface by status, so
//! no new overlapping work is admitted beside it.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_tools::ReservationOwnerContext;
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::JobRunListParams;
use crate::runtime::task::locks::{
    EmptyTaskSurfacePolicy, TaskLockIndex, parse_task_ids, reserve_deferred_footprint,
    reserve_with_index, runs_holding_file_reservations,
};

/// The run input key the gate sets when it deferred the leaf's footprint.
const FOOTPRINT_DEFERRED: &str = "footprint_deferred";

/// How long a leaf waits for its deferred footprint by default: the gate's
/// own default reservation wait.
const DEFAULT_FOOTPRINT_WAIT_SECONDS: u64 = 3600;

/// How often a waiting leaf asks for its footprint again by default: the
/// gate's own default poll interval.
const DEFAULT_FOOTPRINT_POLL_SECONDS: u64 = 30;

/// The leaf's footprint reservation lasts as long as a reservation may; the
/// run's terminal cleanup releases it sooner.
const FOOTPRINT_TTL_SECONDS: u32 = 14400;

/// The decision-only activity that edits nothing, so never waits on locks.
const FINAL_RECOVERY_ACTIVITY: &str = "final_recovery";

impl OrbitRuntime {
    /// The reservation a gate takes for a resumed published delivery the
    /// ordinary reservation in `denied` refused, or `None` when `input` is
    /// not such a delivery: one `backlog` task whose `delivery_job` is the
    /// PR pipeline and whose preserved publication is on record.
    ///
    /// The grant holds no files and names the holders it did not wait on;
    /// `footprint_deferred` tells the leaf to take the footprint itself
    /// before it edits.
    pub(crate) fn deferred_footprint_reservation(
        &self,
        input: &Value,
        denied: &Value,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<ReservationOwnerContext>,
    ) -> Result<Option<Value>, OrbitError> {
        if denied["reserved"] == true
            || input.get("delivery_job").and_then(Value::as_str) != Some("task_pr_pipeline")
        {
            return Ok(None);
        }
        let task_ids = parse_task_ids(input)?;
        let [task_id] = task_ids.as_slice() else {
            return Ok(None);
        };
        let Some(resume) = self.published_resume(&self.get_task(task_id)?)? else {
            return Ok(None);
        };
        let mut output = reserve_deferred_footprint(
            self,
            input,
            agent,
            model,
            owner,
            denied["conflicts"].clone(),
        )?;
        output["published_delivery"] = json!({
            "source_run_id": resume.source_run_id,
            "pull_request": resume.pull_request,
        });
        Ok(Some(output))
    }

    /// Before `activity_name` of `run_id` may edit, take the footprint its
    /// gate deferred, waiting while other tasks hold it.
    ///
    /// A run whose input does not defer its footprint, or that already holds
    /// a reservation with files, has nothing to take. The wait polls every
    /// `footprint_poll_seconds` for at most `footprint_wait_seconds` of the
    /// run input, and ends early when the run stops; running out refuses the
    /// dispatch naming the holders, so the activity never edits beside them.
    /// Another resumed run still waiting for its own footprint is no holder:
    /// it edits nothing and holds no file, and counting it would leave two
    /// such runs refusing each other until both time out. Once it takes its
    /// files, its reservation holds them against this run.
    pub(crate) fn acquire_deferred_footprint(
        &self,
        run_id: &str,
        activity_name: &str,
    ) -> Result<(), OrbitError> {
        if activity_name == FINAL_RECOVERY_ACTIVITY {
            return Ok(());
        }
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(());
        };
        let Some(input) = run
            .input
            .as_ref()
            .filter(|input| input[FOOTPRINT_DEFERRED] == true)
        else {
            return Ok(());
        };
        if runs_holding_file_reservations(self)?.contains(run_id) {
            return Ok(());
        }
        let task_ids = parse_task_ids(input)?;
        let seconds =
            |key: &str, default: u64| input.get(key).and_then(Value::as_u64).unwrap_or(default);
        let wait = Duration::from_secs(seconds(
            "footprint_wait_seconds",
            DEFAULT_FOOTPRINT_WAIT_SECONDS,
        ));
        let poll = Duration::from_secs(
            seconds("footprint_poll_seconds", DEFAULT_FOOTPRINT_POLL_SECONDS).max(1),
        );
        let owner = ReservationOwnerContext {
            owner_run_id: run_id.to_string(),
            owner_metadata_json: Some(
                json!({"deferred_footprint": true, "activity": activity_name}).to_string(),
            ),
        };
        let started = Instant::now();
        loop {
            let index = TaskLockIndex::load(self, &task_ids)?
                .without_status_holders(&self.unacquired_deferred_waiters(run_id)?, &task_ids);
            let output = reserve_with_index(
                self,
                json!({"task_ids": task_ids, "ttl_seconds": FOOTPRINT_TTL_SECONDS}),
                None,
                None,
                Some(owner.clone()),
                &index,
                EmptyTaskSurfacePolicy::Admit,
            )?;
            if output["reserved"] == true {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= wait {
                return Err(OrbitError::Execution(format!(
                    "footprint_unavailable: `{activity_name}` may edit the resumed published \
                     delivery only once its context locks are free, and after {}s they are \
                     still held: {}",
                    elapsed.as_secs(),
                    holders(&output["conflicts"])
                )));
            }
            if self
                .get_job_run_backend(run_id)?
                .is_none_or(|run| run.state != JobRunState::Running)
            {
                return Err(OrbitError::Execution(format!(
                    "footprint_unavailable: run stopped while `{activity_name}` waited for its \
                     context locks"
                )));
            }
            std::thread::sleep(poll.min(wait - elapsed));
        }
    }
}

impl OrbitRuntime {
    /// The tasks of the other running runs that deferred their footprint and
    /// have taken no file of it yet.
    fn unacquired_deferred_waiters(&self, run_id: &str) -> Result<BTreeSet<String>, OrbitError> {
        let acquired = runs_holding_file_reservations(self)?;
        let running = self.list_job_runs_observed(JobRunListParams {
            states: vec![JobRunState::Running],
            ..Default::default()
        })?;
        let mut waiters = BTreeSet::new();
        for run in running {
            let Some(input) = run
                .input
                .as_ref()
                .filter(|input| input[FOOTPRINT_DEFERRED] == true)
            else {
                continue;
            };
            if run.run_id != run_id && !acquired.contains(&run.run_id) {
                waiters.extend(parse_task_ids(input)?);
            }
        }
        Ok(waiters)
    }
}

/// `file held by holder` for each conflict a refused reservation names.
fn holders(conflicts: &Value) -> String {
    let named = conflicts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|conflict| {
            Some(format!(
                "{} held by {}",
                conflict["file"].as_str()?,
                conflict["held_by_id"].as_str()?
            ))
        })
        .collect::<Vec<_>>();
    if named.is_empty() {
        "no holder was named".to_string()
    } else {
        named.join(", ")
    }
}
