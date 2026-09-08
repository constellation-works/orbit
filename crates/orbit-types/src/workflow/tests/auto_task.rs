use crate::task::{TaskPriority, TaskStatus, TaskType};
use crate::workflow::{AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate};

fn definition_with_interval(every_minutes: u64) -> AutoTaskDefinition {
    AutoTaskDefinition {
        schema_version: 1,
        name: "weekly-review".to_string(),
        description: String::new(),
        enabled: true,
        schedule: AutoTaskSchedule::Interval { every_minutes },
        template: AutoTaskTemplate {
            title: "Review work".to_string(),
            description: String::new(),
            acceptance_criteria: Vec::new(),
            task_type: TaskType::Chore,
            tags: Vec::new(),
            required_tools: Vec::new(),
            priority: TaskPriority::Medium,
            crew: None,
            status: TaskStatus::Backlog,
        },
        dedupe: Default::default(),
        created_by: None,
        created_at: String::new(),
        updated_by: None,
        updated_at: String::new(),
    }
}

#[test]
fn validation_rejects_out_of_range_interval() {
    assert!(definition_with_interval(u64::MAX).validate().is_err());
}
