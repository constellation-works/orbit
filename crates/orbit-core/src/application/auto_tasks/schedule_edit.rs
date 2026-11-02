//! The one auto-task schedule edit command behind the ordinary update tool and
//! the dashboard schedule editor [ORB-21173].
//!
//! Each caller contract is an [`AutoTaskScheduleProfile`] of
//! [`OrbitRuntime::auto_task_schedule_command`]: the command owns the
//! compare-and-set, the equal-schedule no-op, retired-coverage rejection,
//! validation and the write, and its outcome says whether an operation was
//! attempted, so a transport only renders it and records the audit row it
//! already owns. The ordinary `auto_task.update` edit may combine a schedule
//! with other fields; it stays one locked edit and applies the same
//! `admit_schedule` and `stage_schedule` steps inside it.

use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule};

use crate::OrbitRuntime;

/// How a schedule is checked before it writes.
#[derive(Debug, Clone, PartialEq)]
pub enum AutoTaskScheduleProfile {
    /// The ordinary update: write, validate and stamp whatever the current
    /// schedule is, even when it already equals the request.
    Unconditional,
    /// The dashboard editor: compare the observed schedule before writing,
    /// answer a request the stored schedule already satisfies without
    /// validating, writing or stamping, then write with the observed schedule
    /// rechecked under the cursor lock.
    Observed {
        /// The schedule the caller observed.
        expected_schedule: Box<AutoTaskSchedule>,
    },
}

/// What a schedule edit did. Only [`Self::Written`] is an attempted operation
/// a transport audits; the other outcomes return before any write.
#[derive(Debug, Clone, PartialEq)]
pub enum AutoTaskScheduleOutcome {
    /// The definition was written and stamped; it holds what now loads.
    Written(AutoTaskDefinition),
    /// The stored schedule already matched the request, so nothing was
    /// validated or written ([`AutoTaskScheduleProfile::Observed`] only).
    AlreadySet(AutoTaskDefinition),
    /// The observed schedule was stale before any write was attempted
    /// ([`AutoTaskScheduleProfile::Observed`] only).
    Stale {
        /// The definition's current schedule.
        actual_schedule: Box<AutoTaskSchedule>,
    },
    /// No definition of that name was found before any write was attempted
    /// ([`AutoTaskScheduleProfile::Observed`] only).
    NotFound,
}

impl AutoTaskScheduleOutcome {
    /// The definition a schedule edit left, for callers that report it whole.
    /// A pre-write refusal becomes the error the locked compare would raise.
    pub fn into_definition(self, name: &str) -> Result<AutoTaskDefinition, OrbitError> {
        match self {
            Self::Written(definition) | Self::AlreadySet(definition) => Ok(definition),
            Self::Stale { .. } => Err(stale_auto_task_schedule()),
            Self::NotFound => Err(OrbitError::InvalidInput(format!(
                "no such auto-task '{name}'"
            ))),
        }
    }
}

/// Why a schedule edit failed, split by whether a write was attempted: a
/// transport audits a refused or failed write, never a failed pre-write read.
#[derive(Debug)]
pub enum AutoTaskScheduleError {
    /// Reading the definition before writing failed; no write was attempted.
    Read(OrbitError),
    /// The write was attempted and refused or failed; nothing changed.
    Write(OrbitError),
}

impl From<AutoTaskScheduleError> for OrbitError {
    fn from(error: AutoTaskScheduleError) -> Self {
        match error {
            AutoTaskScheduleError::Read(error) | AutoTaskScheduleError::Write(error) => error,
        }
    }
}

impl OrbitRuntime {
    /// Replace a definition's schedule under `profile`.
    pub fn auto_task_schedule_command(
        &self,
        name: &str,
        schedule: AutoTaskSchedule,
        profile: AutoTaskScheduleProfile,
    ) -> Result<AutoTaskScheduleOutcome, AutoTaskScheduleError> {
        let expected = match profile {
            AutoTaskScheduleProfile::Unconditional => None,
            AutoTaskScheduleProfile::Observed { expected_schedule } => {
                // The readable fallback is inspection only; the write below
                // still needs the validated checkout-local definition.
                let Some(current) = self
                    .auto_task_show(name)
                    .map_err(AutoTaskScheduleError::Read)?
                else {
                    return Ok(AutoTaskScheduleOutcome::NotFound);
                };
                if current.schedule != *expected_schedule {
                    return Ok(AutoTaskScheduleOutcome::Stale {
                        actual_schedule: Box::new(current.schedule),
                    });
                }
                if current.schedule == schedule {
                    return Ok(AutoTaskScheduleOutcome::AlreadySet(current));
                }
                Some(expected_schedule)
            }
        };
        self.write_auto_task_schedule(name, expected.as_deref(), schedule)
            .map(AutoTaskScheduleOutcome::Written)
            .map_err(AutoTaskScheduleError::Write)
    }

    /// Replace the schedule while holding the scheduler cursor lock, first
    /// comparing the stored schedule with `expected` when one is given.
    fn write_auto_task_schedule(
        &self,
        name: &str,
        expected: Option<&AutoTaskSchedule>,
        schedule: AutoTaskSchedule,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        admit_schedule(&schedule)?;
        self.try_edit_auto_task(name, |definition| {
            stage_schedule(definition, expected, schedule)
        })
    }
}

/// New schedules cannot select the retired QA coverage. Loading a definition
/// that already names it stays allowed, so a not-yet-refreshed workspace copy
/// does not fail every clock tick.
pub(super) fn admit_schedule(schedule: &AutoTaskSchedule) -> Result<(), OrbitError> {
    if let AutoTaskSchedule::Deliveries {
        deliveries_landed, ..
    } = schedule
        && deliveries_landed.coverage
            == orbit_types::workflow::automation::CoverageClass::IntegratedQaV1
    {
        return Err(OrbitError::InvalidInput(
            "coverage `integrated_qa_v1` is retired; delivery triggers use `landed_code_review_v1`"
                .into(),
        ));
    }
    Ok(())
}

/// Set `definition.schedule` inside a locked edit, first comparing the stored
/// schedule with `expected` when one is given. The edit's own validation and
/// stamp follow.
pub(super) fn stage_schedule(
    definition: &mut AutoTaskDefinition,
    expected: Option<&AutoTaskSchedule>,
    schedule: AutoTaskSchedule,
) -> Result<(), OrbitError> {
    if expected.is_some_and(|expected| &definition.schedule != expected) {
        return Err(stale_auto_task_schedule());
    }
    definition.schedule = schedule;
    Ok(())
}

fn stale_auto_task_schedule() -> OrbitError {
    OrbitError::InvalidInput("auto-task schedule changed; refresh before retrying".into())
}
