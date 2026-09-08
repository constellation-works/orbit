use clap::{CommandFactory, Parser};

use super::super::{Cli, Commands};

#[test]
fn search_workspaces_flag_is_repeatable_and_comma_delimited() {
    let cli = Cli::parse_from([
        "orbit",
        "search",
        "query",
        "--workspaces",
        "alpha",
        "--workspaces",
        "beta,gamma",
    ]);
    assert_eq!(cli.workspace.as_deref(), None);
    match cli.command {
        Commands::Search(args) => {
            assert_eq!(args.query.as_deref(), Some("query"));
            assert_eq!(
                args.workspaces,
                vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
            );
            assert!(!args.all_workspaces);
        }
        _ => panic!("expected top-level search command"),
    }
}

#[test]
fn global_workspace_and_search_workspaces_are_distinct() {
    let cli = Cli::parse_from([
        "orbit",
        "--workspace",
        "bound",
        "search",
        "query",
        "--workspaces",
        "alpha",
        "--workspaces",
        "beta",
    ]);
    assert_eq!(cli.workspace.as_deref(), Some("bound"));
    match cli.command {
        Commands::Search(args) => {
            assert_eq!(
                args.workspaces,
                vec!["alpha".to_string(), "beta".to_string()]
            );
            assert!(!args.all_workspaces);
        }
        _ => panic!("expected top-level search command"),
    }
}

#[test]
fn post_subcommand_workspace_binds_global_routing_not_search_scope() {
    let cli = Cli::parse_from(["orbit", "search", "query", "--workspace", "bound"]);
    assert_eq!(cli.workspace.as_deref(), Some("bound"));
    match cli.command {
        Commands::Search(args) => {
            assert!(
                args.workspaces.is_empty(),
                "search scope must not reuse --workspace: {:?}",
                args.workspaces
            );
        }
        _ => panic!("expected top-level search command"),
    }
}

#[test]
fn search_all_workspaces_parses_without_selectors() {
    let cli = Cli::parse_from(["orbit", "search", "query", "--all-workspaces"]);
    assert_eq!(cli.workspace.as_deref(), None);
    match cli.command {
        Commands::Search(args) => {
            assert!(args.all_workspaces);
            assert!(args.workspaces.is_empty());
        }
        _ => panic!("expected top-level search command"),
    }
}

#[test]
fn search_help_distinguishes_global_workspace_from_federated_scope() {
    let help = match Cli::try_parse_from(["orbit", "search", "--help"]) {
        Ok(_) => panic!("--help exits before parsing"),
        Err(error) => error.to_string(),
    };
    assert!(
        help.contains("--workspace <SELECTOR>"),
        "search --help must still show the global routing selector: {help}"
    );
    assert!(
        help.contains("--workspaces <SELECTOR>"),
        "search --help must show federated --workspaces scope: {help}"
    );
    assert!(
        help.contains("--all-workspaces"),
        "search --help must keep --all-workspaces: {help}"
    );
    assert!(
        help.contains("orbit --workspace"),
        "search --help must distinguish federated scope from global routing: {help}"
    );
}

#[test]
fn search_command_debug_assert_accepts_the_resolved_grammar() {
    Cli::command()
        .find_subcommand("search")
        .expect("search is a top-level command")
        .clone()
        .debug_assert();
}
