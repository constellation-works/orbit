use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::run;
use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(about = "View artifacts for a job run or task")]
pub struct ArtifactsCommand {
    /// Run ID or task ID to inspect
    pub id: String,

    /// Treat the ID as a task ID instead of a run ID
    #[arg(long)]
    pub task: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for ArtifactsCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.task {
            return show_task_artifacts(runtime, &self.id);
        }

        eprintln!("[deprecated] use \"orbit run show {}\"", self.id);
        run::run_show_payload(runtime, Some(&self.id), None)
    }
}

fn show_task_artifacts(runtime: &OrbitRuntime, task_id: &str) -> CommandOut {
    let artifacts = runtime.get_task_artifact_manifest(task_id)?;
    let doc = orbit_types::task::serialize_task_artifacts(&artifacts);

    if artifacts.is_empty() {
        return Ok(
            Payload::detail(doc, format!("No artifacts found for task '{task_id}'.")).into(),
        );
    }

    let mut lines = Vec::new();
    for a in &artifacts {
        lines.push(format!(
            "--- {} ({}, {} bytes) ---",
            a.path, a.media_type, a.size_bytes
        ));
    }
    Ok(Payload::detail(doc, lines.join("\n")).into())
}
