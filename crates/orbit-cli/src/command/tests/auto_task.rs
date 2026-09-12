use clap::Parser;
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::{CoverageClass, DeliveryTrigger};

use crate::command::auto_task::AutoTaskSubcommand;
use crate::command::{Cli, CommandOutput, Commands, Execute};
use crate::output::payload::{Block, View};

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
fn auto_task_add_execution_rejects_unknown_required_tools() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let cli = Cli::try_parse_from([
        "orbit",
        "auto-task",
        "add",
        "--name",
        "invalid-tools",
        "--every-minutes",
        "5",
        "--title",
        "Invalid requirement",
        "--required-tools",
        "orbit.task.shwo",
    ])
    .expect("parse auto-task add");
    let Commands::AutoTask(auto_task) = cli.command else {
        panic!("expected auto-task command");
    };
    let AutoTaskSubcommand::Add(args) = auto_task.command else {
        panic!("expected auto-task add command");
    };

    let error = args
        .execute(&runtime)
        .expect_err("unknown template tool must be rejected");
    assert!(
        error
            .to_string()
            .contains("unregistered tool 'orbit.task.shwo'")
    );
    assert!(
        error
            .did_you_mean()
            .is_some_and(|names| { names.iter().any(|name| name == "orbit.task.show") })
    );
}

#[test]
fn auto_task_add_execution_reports_disabled_required_tool_warning() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    runtime
        .disable_tool("orbit.task.list")
        .expect("disable tool");
    let cli = Cli::try_parse_from([
        "orbit",
        "auto-task",
        "add",
        "--name",
        "disabled-tool",
        "--every-minutes",
        "5",
        "--title",
        "Disabled requirement",
        "--required-tools",
        "orbit.task.list",
    ])
    .expect("parse auto-task add");
    let Commands::AutoTask(auto_task) = cli.command else {
        panic!("expected auto-task command");
    };
    let AutoTaskSubcommand::Add(args) = auto_task.command else {
        panic!("expected auto-task add command");
    };

    let CommandOutput::Payload(payload) = args.execute(&runtime).expect("auto-task add") else {
        panic!("auto-task add should return a payload");
    };
    let (document, _) = payload.into_view();
    assert!(document["warnings"].as_array().is_some_and(|warnings| {
        warnings.iter().any(|warning| {
            warning.as_str().is_some_and(|message| {
                message.contains("orbit.task.list") && message.contains("disabled")
            })
        })
    }));
}

#[test]
fn dedupe_accepts_both_spellings_and_agrees_across_file_json_and_show() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    for (flag_value, name) in [
        ("skip-if-open", "dedupe-kebab"),
        ("skip_if_open", "dedupe-snake"),
    ] {
        let cli = Cli::try_parse_from([
            "orbit",
            "auto-task",
            "add",
            "--name",
            name,
            "--every-minutes",
            "5",
            "--title",
            "Dedupe token",
            "--dedupe",
            flag_value,
        ])
        .unwrap_or_else(|error| panic!("parse auto-task add --dedupe {flag_value}: {error}"));
        let Commands::AutoTask(auto_task) = cli.command else {
            panic!("expected auto-task command");
        };
        let AutoTaskSubcommand::Add(args) = auto_task.command else {
            panic!("expected auto-task add command");
        };
        assert_eq!(args.dedupe, DedupePolicy::SkipIfOpen);

        let CommandOutput::Payload(payload) = args.execute(&runtime).expect("auto-task add") else {
            panic!("auto-task add should return a payload");
        };
        let (document, _) = payload.into_view();
        assert_eq!(document["dedupe"], "skip_if_open");

        let file_path = runtime
            .paths()
            .local_dir
            .join("auto_tasks")
            .join(format!("{name}.yaml"));
        let file_contents = std::fs::read_to_string(&file_path).expect("definition file");
        assert!(
            file_contents.contains("dedupe: skip_if_open"),
            "{file_contents}"
        );

        let show_cli = Cli::try_parse_from(["orbit", "auto-task", "show", name])
            .expect("parse auto-task show");
        let Commands::AutoTask(auto_task) = show_cli.command else {
            panic!("expected auto-task command");
        };
        let AutoTaskSubcommand::Show(show_args) = auto_task.command else {
            panic!("expected auto-task show command");
        };
        let CommandOutput::Payload(show_payload) =
            show_args.execute(&runtime).expect("auto-task show")
        else {
            panic!("auto-task show should return a payload");
        };
        let (_, view) = show_payload.into_view();
        let View::Blocks(blocks) = view else {
            panic!("expected block view");
        };
        let text = blocks
            .into_iter()
            .find_map(|block| match block {
                Block::Text(text) => Some(text),
                Block::Table(_) => None,
            })
            .expect("text block");
        assert!(text.contains("dedupe: skip_if_open"), "{text}");
        assert!(!text.contains("SkipIfOpen"), "{text}");
    }
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
