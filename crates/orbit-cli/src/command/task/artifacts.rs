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
    let artifacts = runtime.get_task_artifacts(task_id)?;
    let values: Vec<serde_json::Value> = artifacts
        .iter()
        .map(|a| {
            serde_json::json!({
                "path": a.path,
                "media_type": a.media_type,
                "size": a.content.len(),
            })
        })
        .collect();
    let doc = serde_json::Value::Array(values);

    if artifacts.is_empty() {
        return Ok(
            Payload::detail(doc, format!("No artifacts found for task '{task_id}'.")).into(),
        );
    }

    let mut lines = Vec::new();
    for a in &artifacts {
        lines.push(format!(
            "--- {} ({}, {} bytes) ---",
            a.path,
            a.media_type,
            a.content.len()
        ));
        if let Some(content) = a.text_content() {
            lines.push(content.to_string());
        } else {
            lines.push("[binary content omitted]".to_string());
        }
    }
    Ok(Payload::detail(doc, lines.join("\n")).into())
}
