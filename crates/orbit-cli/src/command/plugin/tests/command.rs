//! The CLI half of the plugin namespace rules: the reserved-command list in
//! `orbit-types` has to equal the real clap tree, which only this crate sees.

use clap::{CommandFactory, Parser};
use orbit_types::plugin::RESERVED_CLI_COMMANDS;

use super::super::PluginSubcommand;
use crate::command::{Cli, Commands};

#[test]
fn reserved_cli_commands_match_the_shipped_command_tree() {
    let mut shipped: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_string())
        .collect();
    shipped.sort();
    let mut reserved: Vec<String> = RESERVED_CLI_COMMANDS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    reserved.sort();

    // `help` is reserved but is not a `Commands` variant, so compare the
    // shipped tree against the reserved list minus that one entry.
    reserved.retain(|name| name != "help");
    assert_eq!(
        shipped, reserved,
        "orbit_types::plugin::RESERVED_CLI_COMMANDS drifted from the clap tree. A plugin \
         namespace equal to a command name would shadow it, so add or remove the name there \
         in the same change that adds or removes the command"
    );
}

#[test]
fn cli_parses_the_plugin_lifecycle() {
    let cli = Cli::parse_from([
        "orbit",
        "plugin",
        "add",
        "./demo",
        "--enable",
        "--grant",
        "fs,network",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Add(args) => {
                assert_eq!(args.source, "./demo");
                assert!(args.enable && !args.force);
                assert_eq!(args.grants, ["fs", "network"]);
            }
            _ => panic!("expected plugin add"),
        },
        _ => panic!("expected the plugin command"),
    }

    let cli = Cli::parse_from(["orbit", "plugin", "sync", "--dry-run"]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Sync(args) => assert!(args.dry_run),
            _ => panic!("expected plugin sync"),
        },
        _ => panic!("expected the plugin command"),
    }

    let cli = Cli::parse_from([
        "orbit",
        "plugin",
        "migrate",
        "./bin/tool",
        "--name",
        "graph",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Migrate(args) => {
                assert_eq!(args.binary, "./bin/tool");
                assert_eq!(args.name.as_deref(), Some("graph"));
                assert_eq!(args.version, "0.1.0");
            }
            _ => panic!("expected plugin migrate"),
        },
        _ => panic!("expected the plugin command"),
    }
}
