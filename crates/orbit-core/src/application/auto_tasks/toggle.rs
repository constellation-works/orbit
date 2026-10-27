//! The one auto-task toggle command behind the tool, the checked operator
//! control and the dashboard [ORB-18764].
//!
//! Each caller contract is an [`AutoTaskToggleProfile`] of
//! [`OrbitRuntime::auto_task_toggle_command`]: the command owns the compare,
//! the effective-state no-op and the write, and its outcome says whether an
//! operation was attempted, so a transport only renders it and records the
//! audit row it already owns.

use orbit_common::OrbitError;
use orbit_types::workflow::AutoTaskDefinition;

use crate::OrbitRuntime;

/// How a toggle is checked before it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoTaskToggleProfile {
    /// The ordinary tool toggle: write and stamp whatever the current state.
    Unconditional,
    /// The checked operator control: compare the raw flag under the cursor
    /// lock, refuse a stale one as invalid input, and otherwise write and
    /// stamp, even when the definition is already in the requested state.
    Checked {
        /// The raw `enabled` flag the caller observed.
        expected_enabled: bool,
    },
    /// The dashboard toggle: compare the observed raw flag before writing,
    /// answer a request the effective switch already satisfies without
    /// writing, then write as [`Self::Checked`]. Disable on a legacy-enabled
    /// after-landing consumer (raw flag still false) is not a no-op: it writes
    /// and the operator stamp clears the legacy enablement.
    Observed {
        /// The raw `enabled` flag the caller observed.
        expected_enabled: bool,
    },
}

/// What a toggle did. Only [`Self::Written`] is an attempted operation a
/// transport audits; the other outcomes return before any write.
#[derive(Debug, Clone, PartialEq)]
pub enum AutoTaskToggleOutcome {
    /// The definition was written and stamped; it holds what now loads.
    Written(AutoTaskDefinition),
    /// The effective switch already matched the request, so nothing was
    /// written ([`AutoTaskToggleProfile::Observed`] only).
    AlreadySet(AutoTaskDefinition),
    /// The observed raw flag was stale before any write was attempted
    /// ([`AutoTaskToggleProfile::Observed`] only).
    Stale {
        /// The definition's current raw `enabled` flag.
        actual_enabled: bool,
    },
    /// No definition of that name was found before any write was attempted
    /// ([`AutoTaskToggleProfile::Observed`] only).
    NotFound,
}

impl AutoTaskToggleOutcome {
    /// The definition a toggle left, for callers that report it whole. A
    /// pre-write refusal becomes the error the locked compare would raise.
    pub fn into_definition(self, name: &str) -> Result<AutoTaskDefinition, OrbitError> {
        match self {
            Self::Written(definition) | Self::AlreadySet(definition) => Ok(definition),
            Self::Stale { .. } => Err(stale_auto_task_state()),
            Self::NotFound => Err(OrbitError::InvalidInput(format!(
                "no such auto-task '{name}'"
            ))),
        }
    }
}

/// Why a toggle failed, split by whether a write was attempted: a transport
/// audits a refused or failed write, never a failed pre-write read.
#[derive(Debug)]
pub enum AutoTaskToggleError {
    /// Reading the definition before writing failed; no write was attempted.
    Read(OrbitError),
    /// The write was attempted and refused or failed; nothing changed.
    Write(OrbitError),
}

impl From<AutoTaskToggleError> for OrbitError {
    fn from(error: AutoTaskToggleError) -> Self {
        match error {
            AutoTaskToggleError::Read(error) | AutoTaskToggleError::Write(error) => error,
        }
    }
}

impl OrbitRuntime {
    /// Enable or disable a definition (the kill-switch) under `profile`.
    /// Disabling pauses an auto-task and preserves the definition;
    /// `auto_task_delete` removes it.
    pub fn auto_task_toggle_command(
        &self,
        name: &str,
        enabled: bool,
        profile: AutoTaskToggleProfile,
    ) -> Result<AutoTaskToggleOutcome, AutoTaskToggleError> {
        let expected = match profile {
            AutoTaskToggleProfile::Unconditional => None,
            AutoTaskToggleProfile::Checked { expected_enabled } => Some(expected_enabled),
            AutoTaskToggleProfile::Observed { expected_enabled } => {
                let Some(current) = self
                    .auto_task_show(name)
                    .map_err(AutoTaskToggleError::Read)?
                else {
                    return Ok(AutoTaskToggleOutcome::NotFound);
                };
                if current.enabled != expected_enabled {
                    return Ok(AutoTaskToggleOutcome::Stale {
                        actual_enabled: current.enabled,
                    });
                }
                // A legacy after-landing consumer is on while its raw flag is
                // still false. Comparing the request to that raw flag would
                // report Disable as a no-op and leave the consumer running.
                if self.auto_task_enabled(&current) == enabled {
                    return Ok(AutoTaskToggleOutcome::AlreadySet(current));
                }
                Some(expected_enabled)
            }
        };
        self.write_auto_task_enabled(name, expected, enabled)
            .map(AutoTaskToggleOutcome::Written)
            .map_err(AutoTaskToggleError::Write)
    }

    /// [`Self::auto_task_toggle_command`] under
    /// [`AutoTaskToggleProfile::Unconditional`].
    pub fn auto_task_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        self.auto_task_toggle_command(name, enabled, AutoTaskToggleProfile::Unconditional)?
            .into_definition(name)
    }

    /// Set the enabled flag while holding the scheduler cursor lock, first
    /// comparing the raw flag with `expected` when one is given.
    fn write_auto_task_enabled(
        &self,
        name: &str,
        expected: Option<bool>,
        enabled: bool,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.try_edit_auto_task(name, |definition| {
            if expected.is_some_and(|expected| definition.enabled != expected) {
                return Err(stale_auto_task_state());
            }
            definition.enabled = enabled;
            Ok(())
        })
    }
}

fn stale_auto_task_state() -> OrbitError {
    OrbitError::InvalidInput("auto-task state changed; refresh before retrying".into())
}
