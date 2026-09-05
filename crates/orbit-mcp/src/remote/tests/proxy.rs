//! Direct SSH stdio proxy tests.

use super::super::proxy::{
    LOCAL_CALLER_MACHINE_ID_FALLBACK, RemoteProxyArgs, caller_machine_id_at, remote_serve_command,
    ssh_command,
};

fn args(ssh_host: &str) -> RemoteProxyArgs {
    RemoteProxyArgs {
        ssh_host: ssh_host.to_string(),
        orchestrator: None,
    }
}

fn args_with_orchestrator(ssh_host: &str, orchestrator: &str) -> RemoteProxyArgs {
    RemoteProxyArgs {
        ssh_host: ssh_host.to_string(),
        orchestrator: Some(orchestrator.to_string()),
    }
}

fn argv(command: &std::process::Command) -> Vec<String> {
    command
        .get_args()
        .map(|value| value.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn ssh_invocation_is_one_non_pty_remote_stdio_server() {
    let command = ssh_command(&args("orbit-box"), "hm_client");

    assert_eq!(command.get_program(), "ssh");
    assert_eq!(
        argv(&command),
        vec![
            "-T",
            "--",
            "orbit-box",
            "orbit mcp serve --remote-caller-machine-id 'hm_client'",
        ]
    );
}

#[test]
fn ssh_invocation_has_no_tunnel_listener_or_capability_policy() {
    let command = ssh_command(&args("orbit-box"), "hm_client");
    let joined = argv(&command).join(" ");

    for forbidden in ["-L", "-tt", "--listen", "--capabilities"] {
        assert!(
            !joined.contains(forbidden),
            "unexpected {forbidden}: {joined}"
        );
    }
}

#[test]
fn remote_command_quotes_the_audit_identity_for_the_remote_shell() {
    assert_eq!(
        remote_serve_command("host/local", None),
        "orbit mcp serve --remote-caller-machine-id 'host/local'"
    );
    assert_eq!(
        remote_serve_command("hm_machine", None),
        "orbit mcp serve --remote-caller-machine-id 'hm_machine'"
    );
}

#[test]
fn orchestrator_attribution_default_is_forwarded_and_quoted() {
    assert_eq!(
        remote_serve_command("hm_machine", Some("hub crew")),
        "orbit mcp serve --remote-caller-machine-id 'hm_machine' --orchestrator 'hub crew'"
    );
    assert_eq!(
        *argv(&ssh_command(
            &args_with_orchestrator("orbit-box", "hub"),
            "hm_client"
        ))
        .last()
        .expect("remote argv"),
        "orbit mcp serve --remote-caller-machine-id 'hm_client' --orchestrator 'hub'"
    );
}

#[test]
fn a_blank_orchestrator_default_forwards_nothing() {
    // An omitted flag and a whitespace-only one are the same absent default,
    // so neither can reach a destination as an unresolvable empty crew name.
    for blank in [None, Some(""), Some("   ")] {
        assert_eq!(
            remote_serve_command("hm_machine", blank),
            "orbit mcp serve --remote-caller-machine-id 'hm_machine'"
        );
    }
}

#[test]
fn persisted_machine_identity_is_forwarded() {
    let root = tempfile::tempdir().expect("global root");
    let outcome = orbit_registry::ensure_host_identity(root.path(), || {
        Ok(orbit_registry::NewHostIdentity {
            host_id: "client".to_string(),
            task_prefix: "CL".to_string(),
        })
    })
    .expect("host identity");

    assert_eq!(
        caller_machine_id_at(Some(root.path())),
        outcome.identity().machine_id
    );
}

#[test]
fn legacy_persisted_machine_identity_is_forwarded() {
    let root = tempfile::tempdir().expect("global root");
    std::fs::write(
        root.path().join("host.toml"),
        "schema_version = 1\nmachine_id = \"hm_legacy\"\nhost_id = \"client\"\n",
    )
    .expect("legacy host identity");

    assert_eq!(caller_machine_id_at(Some(root.path())), "hm_legacy");
}

#[test]
fn absent_or_unreadable_identity_uses_the_audit_fallback() {
    let absent = tempfile::tempdir().expect("absent global root");
    assert_eq!(
        caller_machine_id_at(Some(absent.path())),
        LOCAL_CALLER_MACHINE_ID_FALLBACK
    );
    assert_eq!(caller_machine_id_at(None), LOCAL_CALLER_MACHINE_ID_FALLBACK);

    let malformed = tempfile::tempdir().expect("malformed global root");
    std::fs::write(malformed.path().join("host.toml"), "not valid toml = [")
        .expect("malformed host identity");
    assert_eq!(
        caller_machine_id_at(Some(malformed.path())),
        LOCAL_CALLER_MACHINE_ID_FALLBACK
    );
}
