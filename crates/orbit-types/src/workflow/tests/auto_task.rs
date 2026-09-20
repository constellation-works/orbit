use crate::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use crate::workflow::{AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};

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
            complexity: None,
            crew: None,
            status: TaskStatus::Backlog,
        },
        dedupe: Default::default(),
        skip_if_unchanged: None,
        created_by: None,
        created_at: String::new(),
        updated_by: None,
        updated_at: String::new(),
    }
}

#[test]
fn template_complexity_roundtrips_explicit_values_and_preserves_legacy_omission() {
    let mut definition = definition_with_interval(60);
    definition.template.complexity = Some(TaskComplexity::Hard);

    let yaml = serde_yaml::to_string(&definition).expect("serialize definition");
    assert!(yaml.contains("  complexity: hard\n"), "{yaml}");
    let explicit: AutoTaskDefinition = serde_yaml::from_str(&yaml).expect("load explicit value");
    assert_eq!(explicit.template.complexity, Some(TaskComplexity::Hard));

    let legacy = yaml.replace("  complexity: hard\n", "");
    let omitted: AutoTaskDefinition = serde_yaml::from_str(&legacy).expect("load legacy omission");
    assert_eq!(omitted.template.complexity, None);
    assert!(
        !serde_yaml::to_string(&omitted)
            .expect("serialize legacy definition")
            .contains("complexity:"),
        "legacy omission must remain distinguishable from an explicit assessment"
    );
}

#[test]
fn definition_validation_rejects_explicit_unassessed_complexity() {
    let mut definition = definition_with_interval(60);
    definition.template.complexity = Some(TaskComplexity::Unassessed);

    let error = definition
        .validate()
        .expect_err("template complexity must be assessed");
    assert!(
        error.to_string().contains("low, medium, hard, or xhard"),
        "{error}"
    );
}

#[test]
fn validation_rejects_out_of_range_interval() {
    assert!(definition_with_interval(u64::MAX).validate().is_err());
}

#[test]
fn dedupe_policy_display_matches_its_serialized_wire_token() {
    for (policy, token) in [
        (DedupePolicy::SkipIfOpen, "skip_if_open"),
        (DedupePolicy::Always, "always"),
    ] {
        assert_eq!(policy.to_string(), token);
        assert_eq!(policy.as_str(), token);
        assert_eq!(
            serde_json::to_value(policy).expect("serialize dedupe policy"),
            serde_json::Value::String(token.to_string())
        );
    }
}
