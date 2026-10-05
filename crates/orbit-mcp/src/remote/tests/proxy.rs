//! Direct SSH stdio proxy tests.

use super::super::identity::McpSessionAuthority;
use super::super::proxy::remote_serve_command;

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
