use clap::{ArgAction, Args};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    ExternalRef, OrbitRuntime, TaskComplexity, TaskCreateStatus, TaskPriority, TaskType,
};

use crate::command::{CommandOut, Execute, Payload};

use super::output::task_to_json_for_runtime;

#[derive(Args)]
pub struct TaskAddArgs {
    /// Parent task ID for hierarchical decomposition
    #[arg(long = "parent")]
    pub parent_id: Option<String>,
    /// Task title
    #[arg(long)]
    pub title: String,
    /// Task description
    #[arg(long, default_value = "")]
    pub description: String,
    /// Acceptance criteria. Repeat the flag for multiple criteria.
    #[arg(long = "acceptance-criteria", action = ArgAction::Append)]
    pub acceptance_criteria: Vec<String>,
    /// Dependency task IDs. Repeat or comma-separate for multiple dependencies.
    #[arg(long, alias = "dependency", action = ArgAction::Append, value_delimiter = ',')]
    pub dependencies: Vec<String>,
    /// Task tags. Repeat or comma-separate for multiple tags.
    #[arg(long = "tag", action = ArgAction::Append, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Exact canonical tool names the task adds to its agent activity baseline.
    #[arg(
        long = "required-tools",
        alias = "required-tool",
        action = ArgAction::Append,
        value_delimiter = ','
    )]
    pub required_tools: Vec<String>,
    /// Optional task plan payload. Leave blank for the executing agent or planning activity to author later.
    #[arg(long, default_value = "")]
    pub plan: String,
    /// External tracker reference in `system:id` form. Repeat for multiple refs.
    #[arg(long = "ref", action = ArgAction::Append)]
    pub external_refs: Vec<String>,
    /// Task context selectors. Repeat or comma-separate for multiple selectors.
    /// Prefer `file:`, `dir:`, or `symbol:` forms; legacy raw paths are accepted and upgraded.
    #[arg(long, action = ArgAction::Append, value_delimiter = ',')]
    pub context: Vec<String>,
    /// Workspace path for the task, relative to the selected workspace
    #[arg(long = "workspace-path")]
    pub workspace_path: Option<String>,
    /// Priority level
    #[arg(long, value_enum, default_value_t = TaskPriority::Medium)]
    pub priority: TaskPriority,
    /// Task complexity (low, medium, or hard)
    #[arg(long, value_enum)]
    pub complexity: TaskComplexity,
    /// Task type
    #[arg(long = "type", value_enum)]
    pub task_type: Option<TaskType>,
    /// Initial task status
    #[arg(long, value_enum)]
    pub status: Option<TaskCreateStatus>,
    /// For bug tasks: the originating task whose implementation introduced the defect
    #[arg(long = "source-task")]
    pub source_task: Option<String>,
    /// Named crew to use when running this task
    #[arg(long)]
    pub crew: Option<String>,
    /// Named crew responsible for orchestration attribution
    #[arg(long)]
    pub orchestrator: Option<String>,
    /// Explicit agent model to persist on the task artifact
    #[arg(long)]
    pub model: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskAddArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let (agent, model) = super::mutation_identity(self.model);
        if let Some(parent_id) = self.parent_id.as_deref()
            && runtime.get_task(parent_id).is_err()
        {
            eprintln!("warning: parent task '{parent_id}' was not found; creating subtask anyway");
        }

        let task = runtime.add_task_with_identity(
            TaskAddParams {
                parent_id: self.parent_id,
                title: self.title,
                description: self.description,
                acceptance_criteria: self.acceptance_criteria,
                dependencies: self.dependencies,
                relations: Vec::new(),
                tags: self.tags,
                required_tools: self.required_tools,
                plan: self.plan,
                comment: None,
                context_files: self.context,
                workspace_path: self.workspace_path,
                priority: self.priority,
                complexity: self.complexity,
                task_type: self.task_type,
                status: self.status.map(Into::into),
                system_created: false,
                external_refs: self
                    .external_refs
                    .iter()
                    .map(|raw| ExternalRef::parse_key(raw))
                    .collect::<Result<Vec<_>, _>>()?,
                source_task_id: self.source_task.clone(),
                crew: self.crew,
                orchestrator: self.orchestrator,
            },
            agent,
            model,
        )?;

        Ok(Payload::detail(task_to_json_for_runtime(runtime, &task)?, task.id).into())
    }
}
