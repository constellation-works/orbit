use clap::{CommandFactory, Parser};

use crate::command::task::TaskSubcommand;
use crate::command::{Cli, Commands};
use crate::output::table::{Column, Table};

#[test]
fn task_list_defaults_and_parsing() {
    let cli = Cli::parse_from(["orbit", "task", "list"]);
    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::List(args) = task.command else {
        panic!("expected task list command");
    };

    assert_eq!(args.limit, 50);
    assert!(args.status.is_empty());
    assert!(!args.all);
    assert!(!args.json);
}

#[test]
fn task_list_parses_explicit_limit_and_statuses() {
    let cli = Cli::parse_from([
        "orbit",
        "task",
        "list",
        "--limit",
        "100",
        "--status",
        "backlog,in-progress",
    ]);
    let Commands::Task(task) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::List(args) = task.command else {
        panic!("expected task list command");
    };

    assert_eq!(args.limit, 100);
    assert_eq!(args.status.len(), 2);
}

#[test]
fn task_list_help_documents_status_aware_ordering() {
    let mut cmd = Cli::command();
    let task_cmd = cmd.find_subcommand_mut("task").expect("task subcommand");
    let list_cmd = task_cmd
        .find_subcommand_mut("list")
        .expect("list subcommand");
    let help = list_cmd.render_help().to_string();

    assert!(
        help.contains("status-aware rule"),
        "help must mention status-aware rule:\n{help}"
    );
    assert!(
        help.contains("non-terminal"),
        "help must mention non-terminal tasks:\n{help}"
    );
}

#[test]
fn table_trailing_notices_are_stored_and_rendered() {
    let table = Table::new(vec![Column::new("ID"), Column::new("TITLE")]).trailing_notice(
        "showing 50 of 60 tasks (newest first); use --limit N or a filter to see more",
    );

    assert_eq!(table.trailing_notices().len(), 1);
    assert_eq!(
        table.trailing_notices()[0],
        "showing 50 of 60 tasks (newest first); use --limit N or a filter to see more"
    );
}
