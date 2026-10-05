use clap::{ArgAction, Args};
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::{OrbitError, OrbitRuntime, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::task::TaskArtifact;

use crate::command::{CommandOut, Execute, Payload};

use super::output::task_to_json_for_runtime;

#[derive(Args)]
pub struct TaskUpdateArgs {
    /// Task ID
    pub id: String,
    /// New title
    #[arg(long)]
    pub title: Option<String>,
    /// New description (empty string clears)
    #[arg(long)]
    pub description: Option<String>,
    /// Replacement acceptance criteria. Repeat the flag for multiple criteria.
    #[arg(long = "acceptance-criteria")]
    pub acceptance_criteria: Vec<String>,
    /// Replacement dependency task IDs. Repeat or comma-separate for multiple dependencies (empty string clears).
    #[arg(long, alias = "dependency", action = ArgAction::Append, value_delimiter = ',')]
    pub dependencies: Vec<String>,
    /// Replacement task tags. Repeat or comma-separate for multiple tags.
    /// `os:linux`, `os:macos` or `os:windows` limits which host OS may run the
    /// task from its next admission; any other `os:` value is rejected.
    #[arg(long = "tag", action = ArgAction::Append, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// New task plan (empty string clears)
    #[arg(long, alias = "instructions")]
    pub plan: Option<String>,
    /// New execution summary (empty string clears)
    #[arg(long)]
    pub execution_summary: Option<String>,
    /// Append a task comment
    #[arg(long)]
    pub comment: Option<String>,
    /// New status
    #[arg(long, value_enum)]
    pub status: Option<TaskUpdateStatusArg>,
    /// New task type
    #[arg(long = "type", value_enum)]
    pub task_type: Option<TaskType>,
    /// New dispatch priority
    #[arg(long, value_enum)]
    pub priority: Option<TaskPriority>,
    /// Task complexity
    #[arg(long, value_enum)]
    pub complexity: Option<TaskComplexity>,
    /// Explicit planning attribution label (empty string clears)
    #[arg(long)]
    pub planned_by: Option<String>,
    /// Explicit implementation attribution label (empty string clears)
    #[arg(long)]
    pub implemented_by: Option<String>,
    /// PR review status: `approve` or `request-changes` (empty string clears)
    #[arg(long, value_parser = parse_pr_status)]
    pub pr_status: Option<String>,
    /// Job run ID to associate with the task (empty string clears)
    #[arg(long)]
    pub job_run_id: Option<String>,
    /// Named crew to use when running this task (empty string draws a fresh one)
    #[arg(long)]
    pub crew: Option<String>,
    /// Named crew responsible for orchestration attribution (empty string clears)
    #[arg(long)]
    pub orchestrator: Option<String>,
    /// Replace the whole task context list. Omit to preserve it; an empty string clears it.
    /// To extend scope, include all existing selectors plus the new ones. Repeat or comma-separate for multiple selectors.
    /// Prefer `file:`, `dir:`, or `symbol:` forms; legacy raw paths are accepted and upgraded.
    /// Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up.
    #[arg(long = "context", alias = "context-files", action = ArgAction::Append, value_delimiter = ',')]
    pub context_files: Vec<String>,
    /// Accept context selectors whose target does not exist yet (for work that creates the file)
    #[arg(long)]
    pub allow_missing_context: bool,
    /// Task artifact write in `path=content` form. Repeat for multiple artifacts.
    #[arg(long = "artifact")]
    pub artifacts: Vec<String>,
    /// Explicit agent model to persist on the task artifact
    #[arg(long)]
    pub model: Option<String>,
    /// Take the task's next approval step: `proposed` -> `backlog`, or
    /// `review` -> `done`. The transition is chosen from the current status,
    /// so it cannot be combined with field edits or an explicit `--status`.
    #[arg(long, conflicts_with_all = APPROVE_CONFLICTS)]
    pub approve: bool,
    /// Note recorded on the approval's status history entry (with `--approve`)
    #[arg(long, requires = "approve")]
    pub note: Option<String>,
    /// Discard the candidate the task's last failed run preserved, so its next
    /// run implements fresh instead of resuming it. Combine with `--status
    /// backlog` to requeue.
    #[arg(long)]
    pub discard_candidate: bool,
    /// Apply `--status` even when the task lifecycle refuses the transition
    /// (for example reopening a done task). Human-operator override: the
    /// change is recorded in task history as `forced`.
    #[arg(long, requires = "status")]
    pub force: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Every field-mutation argument on this command. `--approve` performs a
/// guarded transition the domain derives from the task's current status; an
/// edit alongside it would be a second, differently-attributed write on the
/// same invocation, and `--status` would be a direct contradiction of the
/// transition being requested. Rejecting the combination in the parser keeps
/// approval one write with one history entry.
const APPROVE_CONFLICTS: [&str; 21] = [
    "force",
    "title",
    "description",
    "acceptance_criteria",
    "dependencies",
    "tags",
    "plan",
    "execution_summary",
    "status",
    "task_type",
    "priority",
    "complexity",
    "planned_by",
    "implemented_by",
    "pr_status",
    "job_run_id",
    "crew",
    "orchestrator",
    "context_files",
    "artifacts",
    "discard_candidate",
];

impl Execute for TaskUpdateArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let TaskUpdateArgs {
            id,
            title,
            description,
            acceptance_criteria,
            dependencies,
            tags,
            plan,
            execution_summary,
            comment,
            status,
            task_type,
            priority,
            complexity,
            planned_by,
            implemented_by,
            pr_status,
            job_run_id,
            crew,
            orchestrator,
            context_files,
            allow_missing_context,
            artifacts,
            model,
            approve,
            note,
            force,
            discard_candidate,
            json: _,
        } = self;

        if approve {
            let (agent, model) = super::mutation_identity(model);
            let task = runtime.approve_task_with_identity(&id, note, comment, agent, model)?;
            return Ok(Payload::detail(
                task_to_json_for_runtime(runtime, &task)?,
                format!("Approved task '{}' -> {}", task.id, task.status),
            )
            .into());
        }

        let pr_status = pr_status.map(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        });
        let job_run_id = job_run_id.map(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        });
        let crew = crew.map(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        });
        let orchestrator = orchestrator.map(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        });
        let planned_by = planned_by.map(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        });
        let implemented_by = implemented_by.map(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        });
        let acceptance_criteria = (!acceptance_criteria.is_empty()).then_some(acceptance_criteria);
        let dependencies = parse_replacement_list(dependencies);
        let tags = (!tags.is_empty()).then_some(tags);
        let upsert_artifacts = parse_artifact_args(&artifacts)?;
        let context_files = parse_replacement_list(context_files);
        if !allow_missing_context && let Some(candidates) = context_files.as_deref() {
            runtime.ensure_context_selectors_exist(candidates)?;
        }
        let changes_nothing = title.is_none()
            && description.is_none()
            && acceptance_criteria.is_none()
            && dependencies.is_none()
            && tags.is_none()
            && plan.is_none()
            && execution_summary.is_none()
            && comment.is_none()
            && status.is_none()
            && task_type.is_none()
            && priority.is_none()
            && complexity.is_none()
            && planned_by.is_none()
            && implemented_by.is_none()
            && pr_status.is_none()
            && job_run_id.is_none()
            && crew.is_none()
            && orchestrator.is_none()
            && context_files.is_none()
            && upsert_artifacts.is_empty()
            && !discard_candidate;
        if changes_nothing {
            return Err(OrbitError::InvalidInput(
                "nothing to update: pass at least one field flag, e.g. `--status` or `--title` (see `orbit task update --help`)"
                    .to_string(),
            ));
        }
        if let Some(dependencies) = dependencies.as_deref() {
            super::warn_unreadable_dependencies(runtime, dependencies);
        }
        let (agent, model) = super::mutation_identity(model);

        let params = TaskUpdateParams {
            title,
            description,
            acceptance_criteria,
            dependencies,
            tags,
            plan,
            execution_summary,
            comment,
            status: status.map(Into::into),
            task_type,
            priority,
            complexity,
            planned_by,
            implemented_by,
            pr_status,
            job_run_id,
            crew,
            orchestrator,
            context_files,
            upsert_artifacts,
            discard_candidate,
            ..Default::default()
        };
        let task = if force {
            runtime.force_update_task_with_identity(&id, params, agent, model)?
        } else {
            runtime.update_task_with_identity(&id, params, agent, model)?
        };

        Ok(Payload::detail(
            task_to_json_for_runtime(runtime, &task)?,
            format!("Updated task '{}'", task.id),
        )
        .into())
    }
}

/// Validate `--pr-status` at the parser: the value is stored verbatim and read
/// back as a merge gate, so a typo must be refused here rather than persisted
/// as a status no reader recognizes. An empty value is kept: it clears the field.
pub(super) fn parse_pr_status(value: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "approve" | "approved" | "request-changes" | "request_changes"
        | "changes-requested" | "changes_requested" => Ok(value.to_string()),
        _ => Err(format!(
            "unknown PR status '{value}': expected `approve` or `request-changes` (empty string clears)"
        )),
    }
}

fn parse_replacement_list(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then(|| {
        values
            .into_iter()
            .flat_map(|value| crate::parse::csv_to_vec(&value))
            .collect()
    })
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum TaskUpdateStatusArg {
    Proposed,
    Backlog,
    Someday,
    #[value(name = "in-progress", alias = "in_progress")]
    InProgress,
    Review,
    Done,
    Blocked,
    Archived,
    Rejected,
}

impl From<TaskUpdateStatusArg> for TaskStatus {
    fn from(value: TaskUpdateStatusArg) -> Self {
        match value {
            TaskUpdateStatusArg::Proposed => TaskStatus::Proposed,
            TaskUpdateStatusArg::Backlog => TaskStatus::Backlog,
            TaskUpdateStatusArg::Someday => TaskStatus::Someday,
            TaskUpdateStatusArg::InProgress => TaskStatus::InProgress,
            TaskUpdateStatusArg::Review => TaskStatus::Review,
            TaskUpdateStatusArg::Done => TaskStatus::Done,
            TaskUpdateStatusArg::Blocked => TaskStatus::Blocked,
            TaskUpdateStatusArg::Archived => TaskStatus::Archived,
            TaskUpdateStatusArg::Rejected => TaskStatus::Rejected,
        }
    }
}

fn parse_artifact_args(raw_values: &[String]) -> Result<Vec<TaskArtifact>, OrbitError> {
    raw_values
        .iter()
        .map(|raw| {
            let Some((path, content)) = raw.split_once('=') else {
                return Err(OrbitError::InvalidInput(format!(
                    "task artifact must use `path=content` form, got `{raw}`"
                )));
            };
            let path = path.trim();
            if path.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "task artifact path must not be empty".to_string(),
                ));
            }
            Ok(TaskArtifact::from_text(path, content))
        })
        .collect()
}
