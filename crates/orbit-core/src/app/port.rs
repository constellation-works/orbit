//! Port bindings between apps and lower modules.
//!
//! This is the one file that names both the lower module owning a port trait
//! and the higher app implementing it. Each binding exposes the implementor
//! under its port name and asserts at compile time that it satisfies the trait.

pub(crate) use super::evolution::TaskPort as TaskEvolution;
pub(crate) use super::notice::TaskPort as TaskNotice;
pub(crate) use super::routine::ResidentRoutineStatus;
use crate::app::agent;
use crate::application::resident::ResidentRoutineStatusPort;
use crate::application::task::{TaskCorpusPort, TaskEvolutionPort, TaskNoticePort};

const _: () = {
    const fn binds_task_evolution<T: TaskEvolutionPort>() {}
    const fn binds_task_notice<T: TaskNoticePort>() {}

    binds_task_evolution::<TaskEvolution<'_>>();
    binds_task_notice::<TaskNotice<'_>>();
};

// Compile-time proof that the bound implementor satisfies the port trait.
const _: for<'a> fn(&'a TaskCorpus<'a>) -> &'a dyn TaskCorpusPort = |port| port;

impl ResidentRoutineStatusPort for ResidentRoutineStatus<'_> {
    fn disabled_resident_cycle_routine_name(&self) -> Option<String> {
        self.disabled_cycle_routine_name()
    }
}

/// Binds `task`'s [`TaskCorpusPort`] to `app::agent`'s implementation.
pub(crate) type TaskCorpus<'a> = agent::TaskPort<'a>;
