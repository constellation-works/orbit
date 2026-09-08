use clap::{Parser, error::ErrorKind};

use crate::command::{Cli, Commands};

use super::super::TaskSubcommand;

#[test]
fn task_update_accepts_every_existing_status() {
    for status in [
        "proposed",
        "backlog",
        "in-progress",
        "review",
        "done",
        "blocked",
        "archived",
        "rejected",
        "someday",
    ] {
        Cli::try_parse_from(["orbit", "task", "update", "ORB-00001", "--status", status])
            .unwrap_or_else(|error| panic!("status {status} must parse: {error}"));
    }
}

#[test]
fn task_update_accepts_context_files_alias() {
    let cli = Cli::try_parse_from([
        "orbit",
        "task",
        "update",
        "ORB-00001",
        "--context-files",
        "file:src/lib.rs,dir:tests",
        "--json",
    ])
    .expect("parse task update with context-files");

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Update(args) = task.command else {
        panic!("expected task update command");
    };

    assert_eq!(args.id, "ORB-00001");
    assert_eq!(args.context_files, ["file:src/lib.rs", "dir:tests"]);
    assert!(args.json);
}

#[test]
fn task_update_parses_repeat_and_comma_delimited_lists() {
    let cli = Cli::parse_from([
        "orbit",
        "task",
        "update",
        "ORB-00001",
        "--dependencies",
        "ORB-00002,ORB-00003",
        "--dependency",
        "ORB-00004",
        "--context",
        "file:src/lib.rs,dir:tests",
        "--context-files",
        "symbol:Task",
    ]);

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Update(args) = task.command else {
        panic!("expected task update command");
    };

    assert_eq!(args.dependencies, ["ORB-00002", "ORB-00003", "ORB-00004"]);
    assert_eq!(
        args.context_files,
        ["file:src/lib.rs", "dir:tests", "symbol:Task"]
    );
}

#[test]
fn task_update_preserves_explicit_empty_list_values_for_clearing() {
    let cli = Cli::parse_from([
        "orbit",
        "task",
        "update",
        "ORB-00001",
        "--dependencies",
        "",
        "--context",
        "",
    ]);

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Update(args) = task.command else {
        panic!("expected task update command");
    };

    assert_eq!(args.dependencies, [""]);
    assert_eq!(args.context_files, [""]);
}

#[test]
fn task_update_acceptance_criteria_does_not_split_on_commas() {
    let cli = Cli::try_parse_from([
        "orbit",
        "task",
        "update",
        "ORB-00001",
        "--acceptance-criteria",
        "given X, then Y",
        "--acceptance-criteria",
        "given A, then B",
    ])
    .expect("parse task update acceptance criteria");

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Update(args) = task.command else {
        panic!("expected task update command");
    };

    assert_eq!(
        args.acceptance_criteria,
        ["given X, then Y", "given A, then B"]
    );
}

#[test]
fn task_update_complexity_uses_add_spellings() {
    for complexity in ["low", "medium", "hard"] {
        let cli = Cli::try_parse_from([
            "orbit",
            "task",
            "update",
            "ORB-00001",
            "--complexity",
            complexity,
        ])
        .expect("parse task update complexity");

        let Commands::Task(task) = cli.command else {
            panic!("expected task command");
        };
        let TaskSubcommand::Update(args) = task.command else {
            panic!("expected task update command");
        };

        assert_eq!(args.complexity.expect("complexity").to_string(), complexity);
    }
}

#[test]
fn task_update_rejects_required_tools() {
    let result = Cli::try_parse_from([
        "orbit",
        "task",
        "update",
        "ORB-00001",
        "--required-tools",
        "proc.spawn,orbit.task.show",
    ]);
    let Err(error) = result else {
        panic!("required tools are creation-only");
    };

    assert!(error.to_string().contains("--required-tools"), "{error}");
}

#[test]
fn task_update_approve_parses_with_note_and_comment() {
    let cli = Cli::try_parse_from([
        "orbit",
        "task",
        "update",
        "ORB-00001",
        "--approve",
        "--note",
        "looks right",
        "--comment",
        "shipping it",
    ])
    .expect("parse task update --approve");

    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Update(args) = task.command else {
        panic!("expected task update command");
    };

    assert!(args.approve);
    assert_eq!(args.note.as_deref(), Some("looks right"));
    assert_eq!(args.comment.as_deref(), Some("shipping it"));
    assert!(args.status.is_none());
}

/// The transition `--approve` takes is derived from the task's current status,
/// so pairing it with an explicit status or a field edit would be asking for
/// two contradictory writes.
#[test]
fn task_update_approve_conflicts_with_status_and_field_edits() {
    for args in [
        vec![
            "task",
            "update",
            "ORB-00001",
            "--approve",
            "--status",
            "done",
        ],
        vec!["task", "update", "ORB-00001", "--approve", "--title", "New"],
        vec!["task", "update", "ORB-00001", "--approve", "--tag", "chore"],
        vec![
            "task",
            "update",
            "ORB-00001",
            "--approve",
            "--context",
            "file:src/lib.rs",
        ],
    ] {
        let mut argv = vec!["orbit"];
        argv.extend(args);
        let err = match Cli::try_parse_from(&argv) {
            Ok(_) => panic!("{argv:?} should conflict with --approve"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict, "{argv:?}");
    }
}

/// A note only has somewhere to go on the approval's history entry.
#[test]
fn task_update_note_requires_approve() {
    let err = match Cli::try_parse_from(["orbit", "task", "update", "ORB-00001", "--note", "hi"]) {
        Ok(_) => panic!("--note without --approve should not parse"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
}
