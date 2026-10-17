//! Why an admission pass stopped, with its store failure kept typed.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;

/// The failure of one admission pass.
///
/// A store read or write keeps its [`OrbitError`], so a drain can tell a
/// lock that stayed held past its acquisition deadline, which a later pass
/// may find released, from a fault that repeating the pass would only hit
/// again. Every other failure is already a [`DispatchError`]. Either one
/// becomes the same `DeterministicActionFailed` the pass always raised.
#[derive(Debug)]
pub(crate) enum PassError {
    Dispatch(DispatchError),
    Store(Box<StoreFailure>),
}

/// A store failure inside an admission pass.
#[derive(Debug)]
pub(crate) struct StoreFailure {
    action: String,
    /// What the pass was doing, as the failure message names it.
    context: String,
    error: OrbitError,
}

impl PassError {
    /// A store failure while the pass was doing `context`.
    pub(crate) fn store(action: &str, context: impl Into<String>, error: OrbitError) -> Self {
        Self::Store(Box::new(StoreFailure {
            action: action.to_string(),
            context: context.into(),
            error,
        }))
    }

    /// Whether the pass lost only to a lock wait
    /// ([`OrbitError::is_lock_busy`]): nothing was decided under the lock, so
    /// the same pass can succeed once its holder lets go.
    pub(crate) fn is_lock_busy(&self) -> bool {
        matches!(self, Self::Store(failure) if failure.error.is_lock_busy())
    }

    /// The failure's message, without the action that raised it.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Dispatch(error) => error.to_string(),
            Self::Store(failure) => format!("{}: {}", failure.context, failure.error),
        }
    }
}

impl From<DispatchError> for PassError {
    fn from(error: DispatchError) -> Self {
        Self::Dispatch(error)
    }
}

impl From<PassError> for DispatchError {
    fn from(error: PassError) -> Self {
        match error {
            PassError::Dispatch(error) => error,
            PassError::Store(failure) => {
                let reason = format!("{}: {}", failure.context, failure.error);
                DispatchError::action_failed(&failure.action, reason)
            }
        }
    }
}

impl std::fmt::Display for PassError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dispatch(error) => error.fmt(formatter),
            Self::Store(failure) => {
                DispatchError::action_failed(&failure.action, self.reason()).fmt(formatter)
            }
        }
    }
}
