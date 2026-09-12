use clap::{CommandFactory, Parser};

use crate::command::task::TaskSubcommand;
use crate::command::{Cli, Commands};

#[test]
fn task_add_parses_repeat_and_comma_delimited_lists() {
    let cli = Cli::parse_from([
        "orbit",
        "task",
        "add",
        "--title",
        "List parsing",
        "--description",
        "Plain description",
        "--plan",
        "Plain plan",
        "--priority",
        "high",
        "--complexity",
        "hard",
        "--type",
        "bug",
        "--acceptance-criteria",
        "first",
        "--acceptance-criteria",
        "second",
        "--tag",
        "cli,surface",
        "--tag",
        "test",
        "--dependencies",
        "ORB-00001,ORB-00002",
        "--dependency",
        "ORB-00003",
        "--required-tools",
        "proc.spawn,orbit.task.show",
        "--required-tools",
        "orbit.task.update",
        "--context",
        "file:one.rs,file:two.rs",
        "--context",
        "dir:three",
    ]);

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Add(args) = task.command else {
        panic!("expected task add command");
    };

    assert_eq!(args.acceptance_criteria, ["first", "second"]);
    assert_eq!(args.tags, ["cli", "surface", "test"]);
    assert_eq!(args.dependencies, ["ORB-00001", "ORB-00002", "ORB-00003"]);
    assert_eq!(
        args.required_tools,
        ["proc.spawn", "orbit.task.show", "orbit.task.update"]
    );
    assert_eq!(args.context, ["file:one.rs", "file:two.rs", "dir:three"]);
    assert_eq!(args.description, "Plain description");
    assert_eq!(args.plan, "Plain plan");
    assert_eq!(args.priority, orbit_core::TaskPriority::High);
    assert_eq!(args.complexity, orbit_core::TaskComplexity::Hard);
    assert_eq!(args.task_type, Some(orbit_core::TaskType::Bug));
}

#[test]
fn task_add_accepts_legacy_required_tool_alias() {
    let cli = Cli::try_parse_from([
        "orbit",
        "task",
        "add",
        "--title",
        "Legacy required tool",
        "--complexity",
        "low",
        "--required-tool",
        "proc.spawn",
    ])
    .expect("parse task add with legacy required-tool alias");

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Add(args) = task.command else {
        panic!("expected task add command");
    };

    assert_eq!(args.required_tools, ["proc.spawn"]);
}

#[test]
fn task_add_acceptance_criteria_does_not_split_on_commas() {
    let cli = Cli::parse_from([
        "orbit",
        "task",
        "add",
        "--title",
        "Comma criteria",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "given X, then Y",
        "--acceptance-criteria",
        "given A, then B",
    ]);

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Add(args) = task.command else {
        panic!("expected task add command");
    };

    assert_eq!(
        args.acceptance_criteria,
        ["given X, then Y", "given A, then B"]
    );
}

#[test]
fn task_add_rejects_removed_workspace_path_hint() {
    let result = Cli::try_parse_from([
        "orbit",
        "task",
        "add",
        "--title",
        "Workspace-scoped task",
        "--complexity",
        "low",
        "--workspace",
        "ws_nebula",
        "--workspace-path",
        "packages/orbit",
    ]);

    assert!(result.is_err());
}

#[test]
fn task_add_status_only_advertises_creation_legal_values() {
    assert!(
        Cli::try_parse_from([
            "orbit",
            "task",
            "add",
            "--title",
            "Bad status",
            "--status",
            "done",
        ])
        .is_err()
    );
    let mut command = Cli::command();
    let add = command
        .find_subcommand_mut("task")
        .expect("task command")
        .find_subcommand_mut("add")
        .expect("task add command");
    let rendered = add.render_long_help().to_string();

    assert!(
        !rendered.contains("--template"),
        "removed task-template flag leaked into help: {rendered}"
    );

    assert!(
        rendered.contains("--complexity"),
        "complexity must be advertised: {rendered}"
    );
    assert!(
        rendered.contains("filesystem anchor only"),
        "--context help must document that a `symbol:` name is not verified: {rendered}"
    );
    assert!(
        !rendered.contains("Optional task complexity"),
        "complexity must not be described as optional: {rendered}"
    );
    assert!(
        !rendered.contains("unassessed"),
        "unassessed is reserved for automated creation: {rendered}"
    );

    for legal in ["- proposed:", "- backlog:", "- someday:"] {
        assert!(rendered.contains(legal), "{rendered}");
    }
    for illegal in [
        "archived",
        "friction",
        "done",
        "review",
        "rejected",
        "blocked",
        "in-progress",
    ] {
        assert!(
            !rendered.contains(illegal),
            "{illegal} leaked into help: {rendered}"
        );
    }
}

#[test]
fn task_add_requires_assessed_complexity() {
    assert!(
        Cli::try_parse_from(["orbit", "task", "add", "--title", "No complexity"]).is_err(),
        "missing --complexity must fail"
    );
    assert!(
        Cli::try_parse_from([
            "orbit",
            "task",
            "add",
            "--title",
            "Automated non-answer",
            "--complexity",
            "unassessed",
        ])
        .is_err(),
        "unassessed is not a CLI create value"
    );
}

#[test]
fn removed_task_flags_are_rejected() {
    for args in [
        vec!["task", "add", "--title", "Removed", "--agent", "codex"],
        vec!["task", "add", "--title", "Removed", "--comment", "legacy"],
        vec!["task", "add", "--title", "Removed", "--template", "feature"],
        vec![
            "task",
            "add",
            "--title",
            "Removed",
            "--instructions",
            "legacy",
        ],
        vec!["task", "update", "ORB-00001", "--agent", "codex"],
        vec!["task", "start", "ORB-00001", "--agent", "codex"],
        vec![
            "task",
            "artifact",
            "put",
            "ORB-00001",
            "summary.md",
            "--agent",
            "codex",
        ],
    ] {
        let mut argv = vec!["orbit"];
        argv.extend(args);
        assert!(Cli::try_parse_from(argv).is_err());
    }
}
