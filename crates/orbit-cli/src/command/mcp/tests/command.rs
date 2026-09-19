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
    // Unlike `--workspace`, which describes the server this process is,
    // attribution travels to whichever server actually creates the task — so
    // it is accepted alongside every mode.
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

/// [ORB-12564] `--operator` is accepted on both client modes, where it is the
/// operator statement for every SSH destination the client opens. It used to
/// conflict with `--mode`, which is what made operator-over-SSH need a
/// destination-side grant.
#[test]
fn operator_authority_is_accepted_on_the_client_modes() {
    let federated = parse(&["serve", "--mode", "federated", "--operator"]);
    assert_eq!(federated.mode, Some(ServeMode::Federated));
    assert!(federated.operator);

    let remote = parse(&["serve", "--mode", "remote", "box", "--operator"]);
    assert_eq!(remote.mode, Some(ServeMode::Remote));
    assert!(remote.operator);
}

/// The retired Tier 2 argv must not be quietly accepted: a caller that still
/// sends it gets a parse error rather than a session it did not ask for.
#[test]
fn the_retired_forced_command_flags_are_gone() {
    for argv in [
        vec!["serve", "--accept-ssh"],
        vec!["serve", "--caller", "hm_caller"],
        vec!["serve", "--caller-key-fingerprint", "SHA256:x"],
    ] {
        assert!(
            ServeCli::try_parse_from(argv.clone()).is_err(),
            "{argv:?} must no longer parse"
        );
    }
}
