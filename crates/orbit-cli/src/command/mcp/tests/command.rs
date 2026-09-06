//! Argument surface for `orbit mcp serve` [ORB-11313].

use clap::Parser;

use super::*;

/// Minimal parser so the subcommand's own arguments can be exercised without
/// the whole CLI tree.
#[derive(Parser)]
struct ServeCli {
    #[command(flatten)]
    args: ServeArgs,
}

fn parse(argv: &[&str]) -> ServeArgs {
    ServeCli::parse_from(argv).args
}

#[test]
fn serve_binds_no_orchestrator_by_default() {
    let args = parse(&["serve"]);
    assert_eq!(args.orchestrator, None);
}

#[test]
fn the_orchestrator_default_is_independent_of_operator_authority() {
    // Attribution and authority are separate switches: naming an orchestrator
    // must not raise a session above the agent capability, and asking for
    // operator authority must not attribute anything.
    let attribution_only = parse(&["serve", "--orchestrator", "hub"]);
    assert_eq!(attribution_only.orchestrator.as_deref(), Some("hub"));
    assert!(!attribution_only.operator);

    let authority_only = parse(&["serve", "--operator"]);
    assert!(authority_only.operator);
    assert_eq!(authority_only.orchestrator, None);

    let both = parse(&["serve", "--operator", "--orchestrator", "hub"]);
    assert!(both.operator);
    assert_eq!(both.orchestrator.as_deref(), Some("hub"));
}

#[test]
fn the_orchestrator_default_is_forwarded_by_the_client_modes() {
    // Unlike `--operator` and `--workspace`, which describe the server this
    // process is, attribution travels to whichever server actually creates the
    // task — so it is accepted alongside every mode.
    let remote = parse(&["serve", "--mode", "remote", "box", "--orchestrator", "hub"]);
    assert_eq!(remote.mode, Some(ServeMode::Remote));
    assert_eq!(remote.orchestrator.as_deref(), Some("hub"));

    let federated = parse(&["serve", "--mode", "federated", "--orchestrator", "hub"]);
    assert_eq!(federated.mode, Some(ServeMode::Federated));
    assert_eq!(federated.orchestrator.as_deref(), Some("hub"));
}

#[test]
fn the_orchestrator_default_takes_a_crew_name() {
    assert!(
        ServeCli::try_parse_from(["serve", "--orchestrator"]).is_err(),
        "`--orchestrator` must require a crew name rather than acting as a flag"
    );
}
