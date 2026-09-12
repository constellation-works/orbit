use clap::{CommandFactory, Parser, error::ErrorKind};

use crate::command::task::TaskSubcommand;
use crate::command::{Cli, Commands};

/// The trimmed `orbit task` surface has 11 subcommands. ORB-10428 returns lock
/// administration here.
const EXPECTED_TASK_SUBCOMMANDS: [&str; 11] = [
    "add", "artifact", "locks", "list", "show", "lint", "update", "archive", "export", "import",
    "reindex",
];

const REMOVED_TASK_SUBCOMMANDS: [&str; 7] = [
    "approve",
    "reject",
    "unarchive",
    "delete",
    "templates",
    "prune-context",
    "review-thread",
];

fn task_help() -> String {
    let err = match Cli::try_parse_from(["orbit", "task", "--help"]) {
        Ok(_) => panic!("task help should exit before parsing a subcommand"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::DisplayHelp);
    err.to_string()
}

fn root_help_section<'a>(help: &'a str, heading: &str) -> &'a str {
    let (_, after_heading) = help
        .split_once(&format!("{heading}:\n"))
        .unwrap_or_else(|| panic!("root help missing `{heading}` section:\n{help}"));
    after_heading
        .split_once("\n\n")
        .map_or(after_heading, |(section, _)| section)
}

#[test]
fn task_help_lists_exactly_the_trimmed_subcommand_set() {
    let help = task_help();
    for subcommand in EXPECTED_TASK_SUBCOMMANDS {
        assert!(
            help.lines().any(|line| {
                line.trim_start()
                    .strip_prefix(subcommand)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
            }),
            "task help missing `{subcommand}`:\n{help}"
        );
    }
}

/// `start` still parses — callers that predate `task update` owning approval
/// keep working for a couple of releases — but it is off the help surface, so
/// nothing new discovers it.
#[test]
fn deprecated_task_start_still_parses_but_is_hidden_from_help() {
    Cli::try_parse_from(["orbit", "task", "start", "ORB-00001"])
        .expect("deprecated `task start` still parses");
    let help = task_help();
    assert!(
        !help.lines().any(|line| {
            line.trim_start()
                .strip_prefix("start")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        }),
        "task help should not advertise `start`:\n{help}"
    );
}

#[test]
fn removed_task_subcommands_are_rejected() {
    for subcommand in REMOVED_TASK_SUBCOMMANDS {
        let err = match Cli::try_parse_from(["orbit", "task", subcommand]) {
            Ok(_) => panic!("`orbit task {subcommand}` should not parse"),
            Err(err) => err,
        };
        assert!(
            matches!(
                err.kind(),
                ErrorKind::InvalidSubcommand | ErrorKind::UnknownArgument
            ),
            "`orbit task {subcommand}` should be unknown, got {:?}",
            err.kind()
        );
    }
}

#[test]
fn task_help_describes_update_status_transitions() {
    let help = task_help();
    assert!(
        help.contains("--approve"),
        "update help should advertise the approval step:\n{help}"
    );
    assert!(!help.contains("proposed → archived"), "{help}");
    assert!(!help.contains("review → backlog"), "{help}");
}

#[test]
fn task_migration_workspace_selector_is_distinct_from_global_workspace() {
    let cli = Cli::try_parse_from([
        "orbit",
        "--workspace",
        "catalog-workspace",
        "task",
        "import",
        "tasks.tar.zst",
        "--task-workspace",
        "repo-abc123",
    ])
    .expect("task import should accept both workspace selectors");

    assert_eq!(cli.workspace.as_deref(), Some("catalog-workspace"));
    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Import(args) = task.command else {
        panic!("expected task import command");
    };
    assert_eq!(args.task_workspace.as_deref(), Some("repo-abc123"));

    for (subcommand, args) in [
        ("export", vec!["--output", "tasks.tar.zst"]),
        ("reindex", Vec::new()),
    ] {
        let mut argv = vec![
            "orbit",
            "task",
            subcommand,
            "--task-workspace",
            "repo-abc123",
        ];
        argv.extend(args);
        Cli::try_parse_from(argv).unwrap_or_else(|error| {
            panic!("task {subcommand} should accept --task-workspace: {error}")
        });
    }
}

#[test]
fn root_help_groups_scheduler_commands_in_layer_order() {
    let help = Cli::command().render_long_help().to_string();
    assert_eq!(
        root_help_section(&help, "Scheduler"),
        "  clock       Inspect, control, and manually tick the host scheduler\n  sweep       Compatibility alias for `orbit clock tick`\n  routine     Inspect and control scheduled routines on this host\n  auto-task   Define recurring auto-task templates (the scheduler primitive)",
        "{help}"
    );
    assert!(
        !root_help_section(&help, "Operate").contains("sweep"),
        "{help}"
    );
    assert!(
        !root_help_section(&help, "Operate").contains("routine"),
        "{help}"
    );
    assert!(
        !root_help_section(&help, "Definitions").contains("auto-task"),
        "{help}"
    );
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("locks")),
        "{help}"
    );
}

#[test]
fn lock_administration_lives_under_task() {
    // `--locked` was removed from `task list`.
    let err = match Cli::try_parse_from(["orbit", "task", "list", "--locked"]) {
        Ok(_) => panic!("`task list --locked` should no longer parse"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::UnknownArgument);

    // ORB-10428: locks are task administration.
    Cli::try_parse_from(["orbit", "task", "locks", "list"]).expect("task locks list parses");
    Cli::try_parse_from(["orbit", "task", "locks", "list", "--json"])
        .expect("task locks list --json parses");
    Cli::try_parse_from(["orbit", "task", "locks", "release", "R-123"])
        .expect("task locks release <id> parses");

    let err = match Cli::try_parse_from(["orbit", "task", "locks", "release"]) {
        Ok(_) => panic!("`task locks release` requires a reservation id"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);

    let err = match Cli::try_parse_from(["orbit", "locks", "list"]) {
        Ok(_) => panic!("top-level `locks` should not parse"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
}

#[test]
fn task_lint_accepts_sweep_and_fix_forms() {
    Cli::try_parse_from(["orbit", "task", "lint"]).expect("bare lint sweeps");
    Cli::try_parse_from(["orbit", "task", "lint", "--fix"]).expect("lint --fix sweeps");
    Cli::try_parse_from(["orbit", "task", "lint", "--write"]).expect("--write aliases --fix");
    Cli::try_parse_from(["orbit", "task", "lint", "ORB-00001", "--fix"]).expect("lint <id> --fix");
    Cli::try_parse_from(["orbit", "task", "lint", "--fix", "--status", "review"])
        .expect("sweep accepts --status");

    let err =
        match Cli::try_parse_from(["orbit", "task", "lint", "ORB-00001", "--status", "review"]) {
            Ok(_) => panic!("--status must be sweep-only"),
            Err(err) => err,
        };
    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
}
