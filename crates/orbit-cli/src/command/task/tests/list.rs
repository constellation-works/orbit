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

/// `--priority` help is the accepted value set, not a hand-kept copy of it: a
/// copy once omitted `critical`, which the flag has always accepted.
#[test]
fn task_list_priority_help_lists_every_accepted_value() {
    use clap::ValueEnum;
    use orbit_core::TaskPriority;

    let mut cmd = crate::cli_command(&[]);
    let list_cmd = cmd
        .find_subcommand_mut("task")
        .and_then(|task| task.find_subcommand_mut("list"))
        .expect("task list subcommand");
    let help = list_cmd.render_long_help().to_string();
    let priority_help: String = help
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("--priority"))
        .take_while(|line| !line.trim_start().starts_with("--type"))
        .collect::<Vec<_>>()
        .join("\n");

    for priority in TaskPriority::value_variants() {
        let name = priority
            .to_possible_value()
            .expect("priority has a value")
            .get_name()
            .to_string();
        Cli::try_parse_from(["orbit", "task", "list", "--priority", &name])
            .unwrap_or_else(|err| panic!("--priority {name} is accepted: {err}"));
        assert!(
            priority_help.contains(&name),
            "--priority help must list `{name}`:\n{priority_help}"
        );
    }
}
