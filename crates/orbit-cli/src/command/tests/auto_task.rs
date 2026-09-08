use clap::Parser;
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::{CoverageClass, DeliveryTrigger};

use crate::command::auto_task::AutoTaskSubcommand;
use crate::command::{Cli, Commands};

#[test]
fn auto_task_add_accepts_required_tools_and_legacy_alias() {
    for flag in ["--required-tools", "--required-tool"] {
        let cli = Cli::try_parse_from([
            "orbit",
            "auto-task",
            "add",
            "--name",
            "required-tools",
            "--every-minutes",
            "5",
            "--title",
            "Required tools",
            flag,
            "proc.spawn,orbit.task.show",
        ])
        .expect("parse auto-task add required tools");

        let Commands::AutoTask(auto_task) = cli.command else {
            panic!("expected auto-task command");
        };
        let AutoTaskSubcommand::Add(args) = auto_task.command else {
            panic!("expected auto-task add command");
        };

        assert_eq!(args.required_tools, ["proc.spawn", "orbit.task.show"]);
    }
}

#[test]
fn auto_task_update_accepts_required_tools() {
    let cli = Cli::try_parse_from([
        "orbit",
        "auto-task",
        "update",
        "required-tools",
        "--required-tools",
        "proc.spawn,orbit.task.show",
    ])
    .expect("parse auto-task update required tools");

    let Commands::AutoTask(auto_task) = cli.command else {
        panic!("expected auto-task command");
    };
    let AutoTaskSubcommand::Update(args) = auto_task.command else {
        panic!("expected auto-task update command");
    };

    assert_eq!(
        args.required_tools.as_deref(),
        Some("proc.spawn,orbit.task.show")
    );
}

#[test]
fn delivery_schedule_summary_uses_coverage_wire_names() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    for (name, coverage, expected) in [
        (
            "integrated",
            CoverageClass::IntegratedQaV1,
            "integrated_qa_v1",
        ),
        (
            "review",
            CoverageClass::LandedCodeReviewV1,
            "landed_code_review_v1",
        ),
    ] {
        let definition = runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.to_string(),
                description: "Delivery coverage fixture.".to_string(),
                schedule: AutoTaskSchedule::Deliveries {
                    deliveries_landed: DeliveryTrigger {
                        owner_machine: None,
                        branch: "agent-main".to_string(),
                        threshold: 3,
                        max_wait_minutes: 60,
                        coverage,
                        max_items: 20,
                        retries: 0,
                    },
                },
                template: AutoTaskTemplate {
                    title: "Examine deliveries".to_string(),
                    description: "Inspect delivery coverage.".to_string(),
                    acceptance_criteria: vec!["Coverage is recorded.".to_string()],
                    task_type: TaskType::Chore,
                    tags: vec![],
                    required_tools: vec![],
                    priority: TaskPriority::Medium,
                    crew: None,
                    status: TaskStatus::Backlog,
                },
                dedupe: DedupePolicy::SkipIfOpen,
            })
            .expect("add delivery auto-task");

        assert_eq!(
            super::super::auto_task::output::schedule_summary(&definition),
            format!("deliveries=3 branch=agent-main coverage={expected}")
        );
    }
}
