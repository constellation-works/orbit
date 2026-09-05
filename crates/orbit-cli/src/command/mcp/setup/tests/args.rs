use clap::Parser;

use super::super::args::{McpProvider, ProviderSelectionArgs, ProviderSelectionMode};
use crate::command::mcp::McpSubcommand;
use crate::command::{Cli, Commands};

#[test]
fn cli_parses_federated_init_as_an_explicit_opt_in() {
    let cli = Cli::parse_from(["orbit", "mcp", "init", "--federated", "--codex"]);
    match cli.command {
        Commands::Mcp(command) => match command.command {
            McpSubcommand::Init(args) => assert!(args.federated),
            _ => panic!("expected mcp init"),
        },
        _ => panic!("expected top-level mcp command"),
    }

    let cli = Cli::parse_from(["orbit", "mcp", "init", "--codex"]);
    match cli.command {
        Commands::Mcp(command) => match command.command {
            McpSubcommand::Init(args) => assert!(!args.federated),
            _ => panic!("expected mcp init"),
        },
        _ => panic!("expected top-level mcp command"),
    }
}

#[test]
fn cli_parses_federated_remove_as_an_explicit_opt_in() {
    let cli = Cli::parse_from(["orbit", "mcp", "remove", "--federated", "--codex"]);
    match cli.command {
        Commands::Mcp(command) => match command.command {
            McpSubcommand::Remove(args) => assert!(args.federated),
            _ => panic!("expected mcp remove"),
        },
        _ => panic!("expected top-level mcp command"),
    }
}

#[test]
fn provider_selection_defaults_to_auto() {
    let args = ProviderSelectionArgs::default();
    assert!(matches!(
        args.resolve_mode().expect("resolve mode"),
        ProviderSelectionMode::Auto
    ));
}

#[test]
fn provider_selection_rejects_conflicting_flags() {
    let args = ProviderSelectionArgs {
        auto: true,
        claude: true,
        ..ProviderSelectionArgs::default()
    };
    assert!(args.resolve_mode().is_err());
}

#[test]
fn provider_selection_all_includes_every_supported_provider() {
    let args = ProviderSelectionArgs {
        all: true,
        ..ProviderSelectionArgs::default()
    };
    match args.resolve_mode().expect("resolve mode") {
        ProviderSelectionMode::Explicit(providers) => assert_eq!(
            providers,
            vec![
                McpProvider::Claude,
                McpProvider::Codex,
                McpProvider::Gemini,
                McpProvider::Antigravity,
                McpProvider::Grok,
                McpProvider::Cursor,
                McpProvider::Vscode,
                McpProvider::Windsurf,
            ]
        ),
        ProviderSelectionMode::Auto => panic!("expected explicit provider set"),
    }
}

#[test]
fn provider_selection_rejects_auto_combined_with_new_flags() {
    for flag in ["client", "grok", "cursor", "vscode", "windsurf"] {
        let mut args = ProviderSelectionArgs {
            auto: true,
            ..ProviderSelectionArgs::default()
        };
        match flag {
            "client" => args.clients.push(McpProvider::Grok),
            "grok" => args.grok = true,
            "cursor" => args.cursor = true,
            "vscode" => args.vscode = true,
            "windsurf" => args.windsurf = true,
            _ => unreachable!(),
        }
        assert!(
            args.resolve_mode().is_err(),
            "--auto + --{flag} should error"
        );
    }
}

#[test]
fn provider_selection_accepts_client_aliases() {
    let args = ProviderSelectionArgs {
        clients: vec![McpProvider::Grok, McpProvider::Codex, McpProvider::Grok],
        grok: true,
        ..ProviderSelectionArgs::default()
    };
    match args.resolve_mode().expect("resolve mode") {
        ProviderSelectionMode::Explicit(providers) => {
            assert_eq!(providers, vec![McpProvider::Codex, McpProvider::Grok]);
        }
        ProviderSelectionMode::Auto => panic!("expected explicit provider set"),
    }
}
