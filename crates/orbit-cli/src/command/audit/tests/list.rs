use clap::Parser;

use crate::command::{Cli, Commands, audit::AuditSubcommand};

#[test]
fn global_workspace_selector_does_not_populate_audit_workspace_id_filter() {
    for selector in ["qa-10484", "ws_qa-10484"] {
        let cli = Cli::try_parse_from(["orbit", "--workspace", selector, "audit", "list"])
            .expect("global workspace selector should parse");
        assert_eq!(cli.workspace.as_deref(), Some(selector));

        let Commands::Audit(command) = cli.command else {
            panic!("expected audit command");
        };
        let AuditSubcommand::List(args) = command.command else {
            panic!("expected audit list command");
        };
        assert_eq!(args.workspace_id, None);
    }
}

#[test]
fn audit_workspace_id_filter_has_a_distinct_flag() {
    let cli = Cli::try_parse_from(["orbit", "audit", "list", "--workspace-id", "ws_qa-10484"])
        .expect("explicit audit workspace ID should parse");

    let Commands::Audit(command) = cli.command else {
        panic!("expected audit command");
    };
    let AuditSubcommand::List(args) = command.command else {
        panic!("expected audit list command");
    };
    assert_eq!(args.workspace_id.as_deref(), Some("ws_qa-10484"));
}
