//! Direct SSH stdio proxy tests.

use super::super::identity::McpSessionAuthority;
use super::super::proxy::{
    LOCAL_CALLER_MACHINE_ID_FALLBACK, RemoteProxyArgs, caller_machine_id_at, remote_serve_command,
    ssh_command,
};

/// Every test here composes a destination argv, which is agent-downgraded when
/// the composing process looks like an agent. The suite may itself run inside a
/// managed Orbit run, so state the ambient answer rather than inheriting it.
fn as_an_operator_shell() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::unset(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied(),
    )
}

/// The same cleared environment with one agent marker set. One guard, because
/// [`orbit_common::test_env::scoped`] takes a process-wide lock that a second
/// live guard on this thread would deadlock on.
fn as_an_agent(marker: &str, value: &str) -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .map(|name| (*name, (*name == marker).then_some(value))),
    )
}

fn args(ssh_host: &str) -> RemoteProxyArgs {
    RemoteProxyArgs {
        ssh_host: ssh_host.to_string(),
        orchestrator: None,
        authority: McpSessionAuthority::Agent,
    }
}

fn args_with_orchestrator(ssh_host: &str, orchestrator: &str) -> RemoteProxyArgs {
    RemoteProxyArgs {
        ssh_host: ssh_host.to_string(),
        orchestrator: Some(orchestrator.to_string()),
        authority: McpSessionAuthority::Agent,
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
    let _shell = as_an_operator_shell();
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
    let _shell = as_an_operator_shell();
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
    let _shell = as_an_operator_shell();
    assert_eq!(
        remote_serve_command("host/local", None, McpSessionAuthority::Agent),
        "orbit mcp serve --remote-caller-machine-id 'host/local'"
    );
    assert_eq!(
        remote_serve_command("hm_machine", None, McpSessionAuthority::Agent),
        "orbit mcp serve --remote-caller-machine-id 'hm_machine'"
    );
}

/// [ORB-12564] The operator statement travels. An SSH login to a destination is
/// ownership of it, so the destination is asked for exactly the authority this
/// client holds rather than for a ceiling it has to look up.
#[test]
fn operator_authority_propagates_into_the_destination_argv() {
    let _shell = as_an_operator_shell();

    assert_eq!(
        remote_serve_command("hm_machine", None, McpSessionAuthority::Operator),
        "orbit mcp serve --operator --remote-caller-machine-id 'hm_machine'"
    );
    assert_eq!(
        remote_serve_command("hm_machine", Some("hub"), McpSessionAuthority::Operator),
        "orbit mcp serve --operator --remote-caller-machine-id 'hm_machine' --orchestrator 'hub'"
    );
}

/// The one caller-side guard that matters: Orbit governs agents, not people. A
/// client running inside a managed run never hands operator authority onward,
/// whatever the process that launched it held.
#[test]
fn a_client_running_as_an_agent_never_propagates_operator() {
    for (marker, value) in [
        ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
        ("ORBIT_AGENT_NAME", "claude"),
        ("ORBIT_TASK_ACTOR_KIND", "agent"),
    ] {
        let _agent = as_an_agent(marker, value);

        assert_eq!(
            remote_serve_command("hm_machine", None, McpSessionAuthority::Operator),
            "orbit mcp serve --remote-caller-machine-id 'hm_machine'",
            "{marker} must suppress operator propagation"
        );
    }
}

#[test]
fn orchestrator_attribution_default_is_forwarded_and_quoted() {
    let _shell = as_an_operator_shell();
    assert_eq!(
        remote_serve_command("hm_machine", Some("hub crew"), McpSessionAuthority::Agent),
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
    let _shell = as_an_operator_shell();
    // An omitted flag and a whitespace-only one are the same absent default,
    // so neither can reach a destination as an unresolvable empty crew name.
    for blank in [None, Some(""), Some("   ")] {
        assert_eq!(
            remote_serve_command("hm_machine", blank, McpSessionAuthority::Agent),
            "orbit mcp serve --remote-caller-machine-id 'hm_machine'"
        );
    }
}

#[test]
fn persisted_machine_identity_is_forwarded() {
    let root = tempfile::tempdir().expect("global root");
    let outcome = orbit_registry::ensure_machine_identity(root.path(), || {
        Ok(orbit_registry::NewMachineIdentity {
            name: "client".to_string(),
            task_prefix: "CL".to_string(),
        })
    })
    .expect("machine identity");

    assert_eq!(
        caller_machine_id_at(Some(root.path())),
        outcome.identity().id
    );
}

/// A pre-ORB-12725 `host.toml` is folded into `[machine]` on the first read,
/// so the machine keeps forwarding the identity it was already minting under.
#[test]
fn legacy_host_toml_identity_is_migrated_and_forwarded() {
    let root = tempfile::tempdir().expect("global root");
    std::fs::write(
        root.path().join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_legacy\"\nhost_id = \"client\"\ntask_prefix = \"LG\"\n",
    )
    .expect("legacy host identity");

    assert_eq!(caller_machine_id_at(Some(root.path())), "hm_legacy");
    assert!(!root.path().join("host.toml").exists());
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
    std::fs::write(malformed.path().join("config.toml"), "not valid toml = [")
        .expect("malformed machine identity");
    assert_eq!(
        caller_machine_id_at(Some(malformed.path())),
        LOCAL_CALLER_MACHINE_ID_FALLBACK
    );
}
