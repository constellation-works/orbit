//! Port bindings between apps and lower modules.
//!
//! This is the one file that names both the lower module owning a port trait
//! and the higher app implementing it. Each binding exposes the implementor
//! under its port name and asserts at compile time that it satisfies the trait.

pub(crate) use super::evolution::TaskPort as TaskEvolution;
pub(crate) use super::notice::TaskPort as TaskNotice;
pub(crate) use super::routine::ResidentRoutineStatus;
use crate::application::resident::ResidentRoutineStatusPort;
use crate::application::task::{TaskEvolutionPort, TaskNoticePort};

const _: () = {
    const fn binds_task_evolution<T: TaskEvolutionPort>() {}
    const fn binds_task_notice<T: TaskNoticePort>() {}

    binds_task_evolution::<TaskEvolution<'_>>();
    binds_task_notice::<TaskNotice<'_>>();
};

impl ResidentRoutineStatusPort for ResidentRoutineStatus<'_> {
    fn disabled_resident_cycle_routine_name(&self) -> Option<String> {
        self.disabled_cycle_routine_name()
    }
}
