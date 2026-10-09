use clap::{ArgAction, Args};
use orbit_core::{
    AutoTaskTemplatePatch, AutoTaskUpdateParams, DedupePolicy, OrbitRuntime, TaskComplexity,
    TaskPriority, TaskStatus, TaskType,
};

use crate::command::{CommandOut, Execute, Payload};

use super::output::definition_to_json;
use super::schedule_args::resolve_schedule;

/// Patch an existing definition. Only provided fields change; template flags
/// merge with the latest definition under the edit lock.
#[derive(Args)]
pub struct AutoTaskUpdateArgs {
    /// Definition name
    pub name: String,
    /// New description
    #[arg(long)]
    pub description: Option<String>,
    /// New 5-field cron expression (mutually exclusive with `--every-minutes`)
    #[arg(long)]
    pub cron: Option<String>,
    /// New interval in minutes (mutually exclusive with `--cron`)
    #[arg(long = "every-minutes")]
    pub every_minutes: Option<u64>,
    /// Delivery trigger JSON: branch, threshold, max_wait_minutes, coverage; optional owner_machine (defaults to this workspace's registered owner machine), max_items, retries.
    #[arg(long)]
    pub deliveries_landed: Option<String>,
    /// New dedupe policy
    #[arg(long, value_enum)]
    pub dedupe: Option<DedupePolicy>,
    /// New task title
    #[arg(long)]
    pub title: Option<String>,
    /// New task body
    #[arg(long)]
    pub body: Option<String>,
    /// Replace acceptance criteria. Repeat for multiple.
    #[arg(long = "criterion", action = ArgAction::Append)]
    pub criteria: Vec<String>,
    /// New task type
    #[arg(long = "type", value_enum)]
    pub task_type: Option<TaskType>,
    /// Replace tags. Repeat for multiple.
    #[arg(long = "tag", action = ArgAction::Append)]
    pub tags: Vec<String>,
    /// Replacement exact canonical tools copied to minted tasks (empty clears).
    #[arg(long = "required-tools")]
    pub required_tools: Option<String>,
    /// New priority
    #[arg(long, value_enum)]
    pub priority: Option<TaskPriority>,
    /// New assessed complexity for minted tasks
    #[arg(long, value_enum)]
    pub complexity: Option<TaskComplexity>,
    /// New crew override
    #[arg(long)]
    pub crew: Option<String>,
    /// New minted-task status
    #[arg(long, value_enum)]
    pub status: Option<TaskStatus>,
    /// Waive a settled failed delivery batch, retaining its coverage gap.
    #[arg(long, requires = "waiver_reason")]
    pub waive_batch: Option<String>,
    /// Required explanation for a batch waiver.
    #[arg(long, requires = "waive_batch")]
    pub waiver_reason: Option<String>,
}

impl AutoTaskUpdateArgs {
    fn touches_template(&self) -> bool {
        self.title.is_some()
            || self.body.is_some()
            || !self.criteria.is_empty()
            || self.task_type.is_some()
            || !self.tags.is_empty()
            || self.required_tools.is_some()
            || self.priority.is_some()
            || self.complexity.is_some()
            || self.crew.is_some()
            || self.status.is_some()
    }
}

impl Execute for AutoTaskUpdateArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let schedule = resolve_schedule(
            self.cron.clone(),
            self.every_minutes,
            self.deliveries_landed.clone(),
        )?;

        // Core merges only named fields under the same lock as the write.
        let template = if self.touches_template() {
            Some(AutoTaskTemplatePatch {
                title: self.title,
                description: self.body,
                acceptance_criteria: (!self.criteria.is_empty()).then_some(self.criteria),
                task_type: self.task_type,
                tags: (!self.tags.is_empty()).then_some(self.tags),
                required_tools: self.required_tools.as_deref().map(crate::parse::csv_to_vec),
                priority: self.priority,
                complexity: self.complexity.map(Some),
                crew: self.crew.map(Some),
                status: self.status,
                ..Default::default()
            })
        } else {
            None
        };

        let waived = self.waive_batch.is_some();
        let definition = runtime.auto_task_update(
            &self.name,
            AutoTaskUpdateParams {
                waive_batch: self.waive_batch.map(|batch_id| {
                    orbit_types::workflow::automation::WaiveBatchRequest {
                        batch_id,
                        reason: self.waiver_reason.unwrap_or_default(),
                    }
                }),
                description: self.description,
                schedule,
                dedupe: self.dedupe,
                template,
                enabled: None,
            },
        )?;

        let required_tool_warnings = runtime.auto_task_update_tool_warnings(&definition, waived)?;
        let mut document = definition_to_json(&definition);
        if !required_tool_warnings.is_empty()
            && let Some(object) = document.as_object_mut()
        {
            object.insert(
                "warnings".to_string(),
                serde_json::json!(required_tool_warnings),
            );
        }
        Ok(Payload::detail(document, definition.name).into())
    }
}
