use clap::FromArgMatches;

use super::super::discover::cli_command;
use crate::command::{Cli, Commands};

#[test]
fn decision_submit_cli_preserves_signed_raw_message() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../orbit-core/tests/fixtures/decision-simple-body.json"
    ))
    .expect("signed message fixture");
    let signed = fixture["raw_message"].as_str().expect("raw message");
    let matches = cli_command(&[])
        .try_get_matches_from([
            "orbit",
            "decision",
            "submit",
            "request",
            "1",
            "--channel",
            "email",
            "--token",
            "token",
            "--raw-message",
            signed,
        ])
        .expect("parse the real decision submit CLI");
    let cli = Cli::from_arg_matches(&matches).expect("project CLI input");
    let Commands::Decision(command) = cli.command else {
        panic!("expected decision submit");
    };
    assert_eq!(
        command.command.input["raw_message"]
            .as_str()
            .unwrap()
            .as_bytes(),
        signed.as_bytes(),
        "CLI forwarding must preserve the signed simple body, including its final space and CRLF (ORB-18396)"
    );
}
