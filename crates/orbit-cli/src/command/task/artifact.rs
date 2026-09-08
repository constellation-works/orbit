use std::path::PathBuf;

use clap::{Args, Subcommand};
use orbit_common::{NotFoundKind, OrbitError, task_artifact_from_source_file};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::task::{ArtifactPresentation, artifact_presentation};

use crate::command::{CommandOut, Execute, Payload};

use super::output::task_to_json_for_runtime;

#[derive(Args)]
#[command(about = "Manage task artifact files")]
pub struct TaskArtifactCommand {
    #[command(subcommand)]
    pub command: TaskArtifactSubcommand,
}

impl Execute for TaskArtifactCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum TaskArtifactSubcommand {
    /// Store a UTF-8 source file under a task's artifacts directory
    Put(TaskArtifactPutArgs),
    /// Read one stored artifact back out of a task
    Get(TaskArtifactGetArgs),
}

impl Execute for TaskArtifactSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            TaskArtifactSubcommand::Put(args) => args.execute(runtime),
            TaskArtifactSubcommand::Get(args) => args.execute(runtime),
        }
    }
}

#[derive(Args)]
pub struct TaskArtifactPutArgs {
    /// Task ID
    pub id: String,
    /// UTF-8 source file to store as a task artifact
    pub source_path: PathBuf,
    /// Artifact path relative to the task artifacts directory. Defaults to the source file name.
    #[arg(long = "path")]
    pub artifact_path: Option<String>,
    /// Explicit agent model to persist on the task artifact update
    #[arg(long)]
    pub model: Option<String>,
    /// Output the updated task as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskArtifactPutArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let TaskArtifactPutArgs {
            id,
            source_path,
            artifact_path,
            model,
            json: _,
        } = self;
        let (agent, model) = super::mutation_identity(model);
        let artifact = task_artifact_from_source_file(&source_path, artifact_path.as_deref())?;
        let artifact_path = artifact.path.clone();
        let task = runtime.update_task_with_identity(
            &id,
            TaskUpdateParams {
                upsert_artifacts: vec![artifact],
                ..Default::default()
            },
            agent,
            model,
        )?;

        Ok(Payload::detail(
            task_to_json_for_runtime(runtime, &task)?,
            format!("Stored artifact '{artifact_path}' on task '{}'", task.id),
        )
        .into())
    }
}

#[derive(Args)]
pub struct TaskArtifactGetArgs {
    /// Task ID that owns the artifact
    pub id: String,
    /// Artifact path relative to the task artifacts directory, as listed by
    /// `orbit task artifacts --task <ID>`
    pub path: String,
    /// Write the artifact's bytes to this file instead of printing them
    #[arg(long = "out")]
    pub out: Option<PathBuf>,
    /// Output the artifact's metadata as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskArtifactGetArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let TaskArtifactGetArgs {
            id,
            path,
            out,
            json: _,
        } = self;
        // Resolve the owning task first so an unknown id fails as a task
        // not-found rather than as a missing artifact.
        let task = runtime.get_task(&id)?;
        let artifact = runtime.get_task_artifact(&task.id, &path)?.ok_or_else(|| {
            OrbitError::not_found(NotFoundKind::Artifact, format!("{}/{path}", task.id))
        })?;
        let presentation = artifact_presentation(&artifact.media_type, &artifact.content);

        if let Some(out) = &out {
            std::fs::write(out, &artifact.content).map_err(|error| {
                OrbitError::Io(format!("write artifact to '{}': {error}", out.display()))
            })?;
        }

        let doc = serde_json::json!({
            "id": task.id,
            "path": artifact.path,
            "media_type": artifact.media_type,
            "size": artifact.content.len(),
            "presentation": presentation.as_str(),
            "written_to": out.as_ref().map(|out| out.display().to_string()),
        });

        if let Some(out) = out {
            return Ok(Payload::detail(
                doc,
                format!(
                    "Wrote {} bytes of '{}' ({}) to {}",
                    artifact.content.len(),
                    artifact.path,
                    artifact.media_type,
                    out.display()
                ),
            )
            .into());
        }

        // Only UTF-8 text is safe to write to a terminal. Anything else needs a
        // destination file rather than a screenful of raw bytes.
        match presentation {
            ArtifactPresentation::Text => {
                let text = artifact.text_content().unwrap_or_default().to_string();
                Ok(Payload::detail(doc, text).into())
            }
            ArtifactPresentation::Image | ArtifactPresentation::Opaque => {
                Err(OrbitError::InvalidInput(format!(
                    "artifact '{}' on task '{}' is {} ({} bytes) and is not printable; re-run with --out <FILE> to save it",
                    artifact.path,
                    task.id,
                    artifact.media_type,
                    artifact.content.len()
                )))
            }
        }
    }
}
