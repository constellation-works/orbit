use crate::command::ExecuteCore;
use clap::{Args, Subcommand};
use orbit_core::Core;

use crate::command::CommandOut;
use crate::command::run::{JobReplayArgs, JobResumeArgs, JobRunArgs, JobRunPipelineWorkerArgs};

use super::list::JobListArgs;
use super::show::JobShowArgs;

#[derive(Args)]
#[command(about = "Define, list, and manage job workflows")]
pub struct JobCommand {
    #[command(subcommand)]
    pub command: JobSubcommand,
}

impl ExecuteCore for JobCommand {
    fn execute(self, core: &Core) -> CommandOut {
        self.command.execute(core)
    }
}

#[derive(Subcommand)]
pub enum JobSubcommand {
    /// List all registered jobs
    List(JobListArgs),
    /// Show details of a specific job
    Show(JobShowArgs),
    /// Execute a schemaVersion 2 job by ID or YAML path
    Run(JobRunArgs),
    /// Replay a previous job run from step 0 using the current job definition
    Replay(JobReplayArgs),
    /// Submit a detached resume of an interrupted job run from its step checkpoints
    Resume(JobResumeArgs),
    /// Internal worker entrypoint for persisted pipeline runs
    #[command(name = "run-pipeline-worker", hide = true)]
    RunPipelineWorker(JobRunPipelineWorkerArgs),
}

impl JobSubcommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self {
            JobSubcommand::List(args) => args.execute(core),
            JobSubcommand::Show(args) => args.execute(core),
            JobSubcommand::Run(args) => args.submit(core, orbit_core::app::run::JobEntry::JobRun),
            JobSubcommand::Replay(args) => args.execute(core),
            JobSubcommand::Resume(args) => args.execute(core),
            JobSubcommand::RunPipelineWorker(args) => args.execute(core),
        }
    }
}
