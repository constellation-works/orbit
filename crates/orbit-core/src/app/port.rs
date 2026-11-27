//! Bind ports owned by lower modules to the apps that supply their answers.
//!
//! This is the one file that names both a lower module's port trait and the
//! higher module's implementor.

pub(crate) use super::routine::ResidentRoutineStatus;
use crate::app::agent;
use crate::application::resident::ResidentRoutineStatusPort;
use crate::application::task::TaskCorpusPort;

impl ResidentRoutineStatusPort for ResidentRoutineStatus<'_> {
    fn disabled_resident_cycle_routine_name(&self) -> Option<String> {
        self.disabled_cycle_routine_name()
    }
}

// Compile-time proof that the bound implementor satisfies the port trait.
const _: for<'a> fn(&'a TaskCorpus<'a>) -> &'a dyn TaskCorpusPort = |port| port;

/// Binds `task`'s [`TaskCorpusPort`] to `app::agent`'s implementation.
pub(crate) type TaskCorpus<'a> = agent::TaskPort<'a>;
