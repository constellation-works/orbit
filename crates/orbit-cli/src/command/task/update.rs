use clap::{ArgAction, Args};
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::{OrbitError, OrbitRuntime, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::task::TaskArtifact;
use serde_json::{Map, Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::output::task_to_json_for_runtime;

#[derive(Args)]
pub struct TaskUpdateArgs {
    #[command(flatten)]
    pub(crate) routing: super::command::TaskHostArgs,
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
    /// Accept context selectors whose target does not exist yet (for work that creates the file).
    /// Each missing selector is recorded as durable creation intent; dropping a selector from the
    /// list revokes it, and re-sending a recorded one needs no flag.
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
    /// Allow replacing tags when the replacement list drops a system identity tag (`ci-failure:*`).
    #[arg(
        long = "allow-drop-system-tags",
        alias = "allow-drop-system-tag",
        alias = "allow-dropping-system-tags"
    )]
    pub allow_drop_system_tags: bool,
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
const APPROVE_CONFLICTS: [&str; 22] = [
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
    "allow_drop_system_tags",
];

impl Execute for TaskUpdateArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let TaskUpdateArgs {
            routing: _,
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
            allow_drop_system_tags,
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
        let context_creation = match context_files.as_deref() {
            Some(candidates) if allow_missing_context => {
                runtime.authorize_missing_context(candidates)?
            }
            Some(candidates) => {
                runtime.ensure_context_selectors_exist_for_update(&id, candidates)?
            }
            None => Default::default(),
        };
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
            context_creation,
            upsert_artifacts,
            discard_candidate,
            allow_drop_system_tags,
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

impl TaskUpdateArgs {
    /// The `orbit.task.update` input for a write delivered to the host that
    /// holds the task [ORB-14449].
    ///
    /// Flags with no tool field are refused rather than dropped: `--force` is
    /// a human override on that host, and `--approve`, `--artifact` and
    /// `--discard-candidate` are not tool writes.
    pub(crate) fn remote_tool_input(&self, host_ssh: &str) -> Result<Value, OrbitError> {
        let unsupported = [
            ("--force", self.force),
            ("--approve", self.approve),
            ("--artifact", !self.artifacts.is_empty()),
            ("--discard-candidate", self.discard_candidate),
        ]
        .into_iter()
        .filter_map(|(flag, set)| set.then_some(flag))
        .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "{} cannot be routed to the host that holds task {}; run it there: `ssh {host_ssh} \
                 orbit task update {} …`",
                unsupported.join(", "),
                self.id,
                self.id
            )));
        }
        let mut input = Map::new();
        input.insert("id".into(), json!(self.id));
        let strings = [
            ("title", &self.title),
            ("description", &self.description),
            ("plan", &self.plan),
            ("execution_summary", &self.execution_summary),
            ("comment", &self.comment),
            ("planned_by", &self.planned_by),
            ("implemented_by", &self.implemented_by),
            ("pr_status", &self.pr_status),
            ("job_run_id", &self.job_run_id),
            ("crew", &self.crew),
            ("orchestrator", &self.orchestrator),
        ];
        for (key, value) in strings {
            if let Some(value) = value {
                input.insert(key.into(), json!(value));
            }
        }
        if !self.acceptance_criteria.is_empty() {
            input.insert(
                "acceptance_criteria".into(),
                json!(self.acceptance_criteria),
            );
        }
        if let Some(dependencies) = parse_replacement_list(self.dependencies.clone()) {
            input.insert("dependencies".into(), json!(dependencies));
        }
        if !self.tags.is_empty() {
            input.insert("tags".into(), json!(self.tags));
        }
        if let Some(context) = parse_replacement_list(self.context_files.clone()) {
            input.insert("context_files".into(), json!(context));
        }
        if self.allow_missing_context {
            input.insert("allow_missing_context".into(), json!(true));
        }
        if self.allow_drop_system_tags {
            input.insert("allow_drop_system_tags".into(), json!(true));
        }
        if let Some(status) = self.status {
            input.insert("status".into(), json!(TaskStatus::from(status).to_string()));
        }
        if let Some(task_type) = self.task_type {
            input.insert("type".into(), json!(task_type.to_string()));
        }
        if let Some(priority) = self.priority {
            input.insert("priority".into(), json!(priority.to_string()));
        }
        if let Some(complexity) = self.complexity {
            input.insert("complexity".into(), json!(complexity.to_string()));
        }
        if input.len() == 1 {
            return Err(OrbitError::InvalidInput(
                "nothing to update: pass at least one field flag, e.g. `--status` or `--title` (see `orbit task update --help`)"
                    .to_string(),
            ));
        }
        if let (_, Some(model)) = super::mutation_identity(self.model.clone()) {
            input.insert("model".into(), json!(model));
        }
        Ok(Value::Object(input))
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
