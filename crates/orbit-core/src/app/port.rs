//! Port bindings between apps and lower modules.
//!
//! This is the one file that names both the lower module owning a port trait
//! and the higher app implementing it. Each binding exposes the implementor
//! under its port name and asserts at compile time that it satisfies the trait.

pub(crate) use super::evolution::TaskPort as TaskEvolution;
pub(crate) use super::experiment::{
    DistributedPort as DistributedExperiment, JobPort as JobExperiment,
    TagInstructionPort as TagInstructionExperiment, TaskPort as TaskExperiment,
};
pub(crate) use super::notice::TaskPort as TaskNotice;
pub(crate) use super::resident::TaskPort as TaskResident;
pub(crate) use super::routine::ResidentRoutineStatus;
use crate::app::resident::ResidentRoutineStatusPort;
use crate::app::{agent, codebase};
use crate::application::distributed::DistributedExperimentPort;
use crate::application::job::JobExperimentPort;
use crate::application::review::ReviewCandidateSizePort;
use crate::application::tag_instructions::TagInstructionExperimentPort;
use crate::application::task::{
    TaskCodebasePort, TaskCorpusPort, TaskEvolutionPort, TaskExperimentPort, TaskNoticePort,
    TaskResidentPort,
};

const _: () = {
    const fn binds_task_evolution<T: TaskEvolutionPort>() {}
    const fn binds_task_notice<T: TaskNoticePort>() {}
    const fn binds_task_resident<T: TaskResidentPort>() {}

    binds_task_evolution::<TaskEvolution<'_>>();
    binds_task_notice::<TaskNotice<'_>>();
    binds_task_resident::<TaskResident<'_>>();
};

const _: () = {
    const fn binds_distributed_experiment<T: DistributedExperimentPort>() {}
    const fn binds_job_experiment<T: JobExperimentPort>() {}
    const fn binds_tag_instruction_experiment<T: TagInstructionExperimentPort>() {}
    const fn binds_task_experiment<T: TaskExperimentPort>() {}

    binds_distributed_experiment::<DistributedExperiment<'_>>();
    binds_job_experiment::<JobExperiment<'_>>();
    binds_tag_instruction_experiment::<TagInstructionExperiment<'_>>();
    binds_task_experiment::<TaskExperiment<'_>>();
};

// Compile-time proof that each bound implementor satisfies its port trait.
const _: for<'a> fn(&'a TaskCorpus<'a>) -> &'a dyn TaskCorpusPort = |port| port;

const _: for<'a> fn(&'a TaskCodebase<'a>) -> &'a dyn TaskCodebasePort = |port| port;

const _: for<'a> fn(&'a ReviewCandidateSize<'a>) -> &'a dyn ReviewCandidateSizePort = |port| port;

impl ResidentRoutineStatusPort for ResidentRoutineStatus<'_> {
    fn disabled_resident_cycle_routine_name(&self) -> Option<String> {
        self.disabled_cycle_routine_name()
    }
}

/// Binds `task`'s [`TaskCorpusPort`] to `app::agent`'s implementation.
pub(crate) type TaskCorpus<'a> = agent::TaskPort<'a>;

/// Binds `task`'s [`TaskCodebasePort`] to `app::codebase`'s implementation.
pub(crate) type TaskCodebase<'a> = codebase::TaskPort<'a>;

/// Binds `review`'s [`ReviewCandidateSizePort`] to `app::codebase`'s
/// implementation.
pub(crate) type ReviewCandidateSize<'a> = codebase::ReviewPort<'a>;
