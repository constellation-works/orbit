use clap::{ArgAction, Args};
use orbit_core::{
    AutoTaskAddParams, AutoTaskTemplate, DedupePolicy, OrbitRuntime, TaskPriority, TaskStatus,
    TaskType,
};

use crate::command::{CommandOut, Execute, Payload};

use super::output::definition_to_json;
use super::schedule_args::require_schedule;

#[derive(Args)]
pub struct AutoTaskAddArgs {
    /// Unique definition name (lowercase alphanumeric, `-`/`_`, starts alphanumeric)
    #[arg(long)]
    pub name: String,
    /// Human description of the recurring chore
    #[arg(long, default_value = "")]
    pub description: String,
    /// 5-field cron expression (mutually exclusive with `--every-minutes`)
    #[arg(long)]
    pub cron: Option<String>,
    /// Interval in minutes (mutually exclusive with `--cron`)
    #[arg(long = "every-minutes")]
    pub every_minutes: Option<u64>,
    /// Delivery trigger JSON: branch, threshold, max_wait_minutes, coverage; optional owner_machine (defaults to this workspace's registered owner machine), max_items, retries.
    #[arg(long)]
    pub deliveries_landed: Option<String>,
    /// Title of each minted task
    #[arg(long)]
    pub title: String,
    /// Body / instruction of each minted task
    #[arg(long, default_value = "")]
    pub body: String,
    /// Acceptance criterion. Repeat for multiple.
    #[arg(long = "criterion", action = ArgAction::Append)]
    pub criteria: Vec<String>,
    /// Task type (defaults to chore)
    #[arg(long = "type", value_enum, default_value_t = TaskType::Chore)]
    pub task_type: TaskType,
    /// Tag applied to each minted task (in addition to the provenance tag). Repeat.
    #[arg(long = "tag", action = ArgAction::Append)]
    pub tags: Vec<String>,
    /// Exact canonical tool names copied to every minted task.
    #[arg(
        long = "required-tools",
        alias = "required-tool",
        action = ArgAction::Append,
        value_delimiter = ','
    )]
    pub required_tools: Vec<String>,
    /// Priority (defaults to medium)
    #[arg(long, value_enum, default_value_t = TaskPriority::Medium)]
    pub priority: TaskPriority,
    /// Crew override for minted tasks
    #[arg(long)]
    pub crew: Option<String>,
    /// Status each minted task enters (defaults to backlog)
    #[arg(long, value_enum, default_value_t = TaskStatus::Backlog)]
    pub status: TaskStatus,
    /// Dedupe policy (defaults to skip-if-open)
    #[arg(long, value_enum, default_value_t = DedupePolicy::SkipIfOpen)]
    pub dedupe: DedupePolicy,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskAddArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let schedule = require_schedule(self.cron, self.every_minutes, self.deliveries_landed)?;
        // Minted tasks receive TaskComplexity::Unassessed (not a real
        // assessment). Definitions do not carry complexity; the mint path
        // stamps the explicit non-answer so create never produces a gap.
        let template = AutoTaskTemplate {
            title: self.title,
            description: self.body,
            acceptance_criteria: self.criteria,
            task_type: self.task_type,
            tags: self.tags,
            required_tools: self.required_tools,
            priority: self.priority,
            crew: self.crew,
            status: self.status,
        };
        let definition = runtime.auto_task_add(AutoTaskAddParams {
            name: self.name,
            description: self.description,
            schedule,
            template,
            dedupe: self.dedupe,
        })?;

        Ok(Payload::detail(definition_to_json(&definition), definition.name).into())
    }
}
