//! `orbit task start` — hidden compatibility shim.
//!
//! Approval and the move to `in-progress` both live on `orbit task update`
//! now (`--approve`, `--status in-progress`), so this verb is off the help
//! surface. It still runs, unchanged, because scripts and agent prompts that
//! predate the move are still calling it; it warns on stderr and will be
//! removed after a couple of releases.
//!
//! Nothing else in the tree should reach for it. The `orbit.task.start` tool
//! remains the pipeline's own entrypoint and is not affected.

use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::output::task_to_json_for_runtime;

/// Printed once per invocation, to stderr, so a caller piping `--json` into a
/// parser sees the notice without it corrupting the document on stdout.
const DEPRECATION_NOTICE: &str = "note: `orbit task start` is deprecated and hidden from help. \
Use `orbit task update <id> --approve` to approve proposed work, and \
`orbit task update <id> --status in-progress` to take it.";

#[derive(Args)]
pub struct TaskStartArgs {
    /// Task ID
    pub id: String,
    /// Optional lifecycle note (records proposal approval when starting proposed work)
    #[arg(long)]
    pub note: Option<String>,
    /// Append a task comment
    #[arg(long)]
    pub comment: Option<String>,
    /// Crew override for this start
    #[arg(long)]
    pub crew: Option<String>,
    /// Explicit agent model to persist on the task artifact
    #[arg(long)]
    pub model: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskStartArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        eprintln!("{DEPRECATION_NOTICE}");
        let (agent, model) = super::mutation_identity(self.model);
        let task = runtime.start_task_with_identity_and_crew(
            &self.id,
            self.note,
            self.comment,
            agent,
            model,
            self.crew,
        )?;
        Ok(Payload::detail(
            task_to_json_for_runtime(runtime, &task)?,
            format!("Started task '{}'", task.id),
        )
        .into())
    }
}
