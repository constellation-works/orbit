//! The one create-or-update run-state write the run controls share.
//!
//! Cancellation, admissions-stop and graceful drain-cancel each need
//! "mutate the pipeline state row transactionally; if the row does not
//! exist yet, create an empty one and try again". They used to hand-write
//! that loop (and its race-test hook) per control; this is the single
//! implementation, in the initialise-then-retry form: the caller's closure
//! only ever runs inside `update_run_state`'s transaction, so a worker
//! checkpoint that lands during initialisation is merged, never replaced.

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::{JobRunState, PipelineState, RunStateUpdate};
use serde_json::Value;

use crate::OrbitRuntime;

/// The refusal every live-run control shares: a finished run admits no
/// further control, whichever surface asked.
pub(super) fn terminal_run_error(run_id: &str, state: JobRunState) -> OrbitError {
    OrbitError::JobValidation(format!(
        "job run '{run_id}' is {state}; a terminal run admits no further work"
    ))
}

/// Whether [`OrbitRuntime::update_or_create_run_state`] initialises a
/// missing pipeline state row before applying the caller's update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunStateCreation {
    /// Initialise an empty state from `job_id`/`input` and retry the
    /// transaction; never returns [`RunStateUpdate::NoState`].
    CreateIfMissing,
    /// Keep the "no state row → do nothing" behaviour: return
    /// [`RunStateUpdate::NoState`] without initialising.
    PreserveMissing,
}

impl OrbitRuntime {
    /// Apply `update` to `run_id`'s pipeline state in the store's
    /// transaction, initialising an empty state from `job_id`/`input`
    /// first when the run has none yet and `creation` allows it.
    ///
    /// A missing run is a not-found error either way. On success the
    /// result is [`RunStateUpdate::Updated`].
    pub(super) fn update_or_create_run_state(
        &self,
        run_id: &str,
        job_id: &str,
        input: Option<&Value>,
        creation: RunStateCreation,
        update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
    ) -> Result<RunStateUpdate, OrbitError> {
        loop {
            #[cfg(test)]
            run_run_state_write_hook();
            match self.stores().jobs().update_run_state(run_id, update)? {
                RunStateUpdate::Updated => return Ok(RunStateUpdate::Updated),
                RunStateUpdate::NotFound => {
                    return Err(OrbitError::not_found(
                        NotFoundKind::JobRun,
                        run_id.to_string(),
                    ));
                }
                RunStateUpdate::NoState if creation == RunStateCreation::PreserveMissing => {
                    return Ok(RunStateUpdate::NoState);
                }
                RunStateUpdate::NoState => {
                    #[cfg(test)]
                    run_run_state_write_hook();
                    let state = PipelineState::new(
                        run_id.to_string(),
                        job_id.to_string(),
                        input
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Default::default())),
                    );
                    // Insert-if-absent: `false` means a worker initialised
                    // first, and the retry applies `update` to its document.
                    self.stores().jobs().initialize_run_state(run_id, &state)?;
                }
            }
        }
    }
}

// One-shot, thread-local seam at the write boundary: deterministic tests
// can commit worker progress before the update transaction, or between
// the `NoState` result and initialisation. No production hook exists, and
// taking it before invocation permits a follow-up interleaving.
#[cfg(test)]
thread_local! {
    static RUN_STATE_WRITE_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) fn set_run_state_write_hook(hook: impl FnOnce() + 'static) {
    RUN_STATE_WRITE_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(super) fn run_run_state_write_hook() {
    let hook = RUN_STATE_WRITE_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
