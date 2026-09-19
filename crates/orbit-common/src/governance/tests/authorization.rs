use std::collections::BTreeSet;

use super::super::authorization::*;
use orbit_types::tool::{McpCapability, ToolSessionContext};

fn envelope() -> CallerEnvelope {
    CallerEnvelope::default()
}

fn operation(id: &str) -> &'static GovernedOperation {
    GOVERNED_OPERATIONS
        .iter()
        .find(|operation| operation.id == id)
        .expect("governed operation is declared")
}

#[test]
fn registry_declares_each_operation_once_with_a_capability() {
    let mut seen = BTreeSet::new();
    for operation in GOVERNED_OPERATIONS {
        assert!(
            seen.insert((operation.surface, operation.id)),
            "duplicate governed operation: {}",
            operation.id
        );
        assert!(
            !operation.allowed.is_empty(),
            "governed operation '{}' allows no capability, which would deny every caller",
            operation.id
        );
        assert!(
            !operation.rationale.trim().is_empty(),
            "governed operation '{}' has no rationale to show a denied caller",
            operation.id
        );
        match operation.surface {
            OperationSurface::Tool => assert!(
                !operation.id.contains(' '),
                "tool operation '{}' must be a canonical tool name",
                operation.id
            ),
            OperationSurface::CliCommand => assert!(
                operation.id.split(' ').count() == 2,
                "command operation '{}' must be `<command> <subcommand>`",
                operation.id
            ),
            OperationSurface::Dashboard => assert!(
                operation.id.split('.').count() == 2,
                "dashboard operation '{}' must be `<domain>.<action>`",
                operation.id
            ),
        }
    }
}

#[test]
fn lookup_is_surface_scoped() {
    assert!(governed_tool("orbit.task.delete").is_some());
    assert!(governed_tool("workspace teardown").is_none());
    assert!(governed_command("workspace", "teardown").is_some());
    assert!(governed_command("orbit.task.delete", "").is_none());
    assert!(governed_tool("orbit.task.show").is_none());
    assert!(governed_command("workspace", "list").is_none());
    assert!(governed_dashboard("routine.toggle").is_some());
    assert!(governed_dashboard("clock.service").is_some());
    assert!(governed_dashboard("clock.cadence").is_some());
    assert!(governed_dashboard("auto_task.toggle").is_some());
    assert!(governed_dashboard("auto_task.mint").is_some());
    assert!(governed_dashboard("routine.list").is_none());
}

#[test]
fn session_grants_win_over_process_signals() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        session_capabilities: BTreeSet::from([McpCapability::Runner]),
        agent_declared: true,
        interactive_terminal: true,
        operator_override: true,
        ..envelope()
    });
    assert_eq!(caller.provenance(), CallerProvenance::Session);
    assert_eq!(caller.grants(), &BTreeSet::from([McpCapability::Runner]));
}

#[test]
fn agent_envelope_outranks_a_terminal() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        agent_declared: true,
        interactive_terminal: true,
        ..envelope()
    });
    assert_eq!(caller.provenance(), CallerProvenance::AgentEnvelope);
    assert_eq!(caller.grants(), &BTreeSet::from([McpCapability::Agent]));
}

#[test]
fn override_outranks_an_agent_envelope_and_is_marked() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        operator_override: true,
        agent_declared: true,
        ..envelope()
    });
    assert_eq!(caller.provenance(), CallerProvenance::OperatorOverride);
    assert!(caller.is_override());
    assert!(authorize(operation("orbit.task.delete"), &caller).is_ok());
}

#[test]
fn a_terminal_resolves_to_operator() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        interactive_terminal: true,
        ..envelope()
    });
    assert_eq!(caller.provenance(), CallerProvenance::InteractiveTerminal);
    assert!(authorize(operation("workspace teardown"), &caller).is_ok());
}

#[test]
fn an_unidentified_caller_is_denied() {
    let caller = CallerCapabilities::resolve(&envelope());
    assert_eq!(caller.provenance(), CallerProvenance::Unknown);
    assert!(caller.grants().is_empty());

    let denial = authorize(operation("orbit.task.delete"), &caller).expect_err("must deny");
    assert_eq!(denial.provenance, CallerProvenance::Unknown);
    assert_eq!(denial.granted, "none");
}

#[test]
fn an_agent_is_denied_out_of_scope_destruction() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        agent_declared: true,
        ..envelope()
    });
    for id in [
        "orbit.task.delete",
        "orbit.task.reject",
        "orbit.semantic.uninstall",
    ] {
        assert!(
            authorize(operation(id), &caller).is_err(),
            "agent must not reach '{id}'"
        );
    }
}

#[test]
fn dashboard_operations_require_an_operator() {
    let agent = CallerCapabilities::resolve(&CallerEnvelope {
        agent_declared: true,
        ..envelope()
    });
    let operator = CallerCapabilities::resolve(&CallerEnvelope {
        interactive_terminal: true,
        ..envelope()
    });
    for id in [
        "routine.toggle",
        "clock.service",
        "clock.cadence",
        "auto_task.toggle",
        "auto_task.mint",
    ] {
        let operation = governed_dashboard(id).expect("dashboard operation is declared");
        assert!(authorize(operation, &agent).is_err());
        assert!(authorize(operation, &operator).is_ok());
    }
}

#[test]
fn a_run_retains_the_operations_it_dispatches() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        session_capabilities: BTreeSet::from([McpCapability::Runner]),
        ..envelope()
    });
    assert!(authorize(operation("orbit.task.locks.release"), &caller).is_ok());
    assert!(authorize(operation("gc worktrees"), &caller).is_ok());
    // Runner sanctions what the run dispatches, not everything destructive.
    assert!(authorize(operation("orbit.task.delete"), &caller).is_err());
}

#[test]
fn denial_names_the_capability_the_rationale_and_the_escape_hatch() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        agent_declared: true,
        ..envelope()
    });
    let denial =
        authorize(operation("orbit.task.locks.release"), &caller).expect_err("agent is denied");
    let message = denial.to_string();

    assert!(message.contains("orbit.task.locks.release"), "{message}");
    assert!(message.contains("operator or runner"), "{message}");
    assert!(message.contains("same files"), "{message}");
    assert!(message.contains("agent-envelope"), "{message}");
    assert!(message.contains(OPERATOR_OVERRIDE_ENV), "{message}");
}

#[test]
fn an_mcp_session_resolves_from_its_grants_alone() {
    let session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    };
    let envelope = CallerEnvelope::mcp_session(&session);

    assert_eq!(envelope.resolution, CapabilityResolution::SessionOnly);
    assert!(
        !envelope.operator_override && !envelope.agent_declared && !envelope.interactive_terminal,
        "the MCP envelope must observe nothing about the hosting process"
    );

    let caller = CallerCapabilities::resolve(&envelope);
    assert_eq!(caller.provenance(), CallerProvenance::Session);
    assert!(authorize(operation("orbit.workflow.run.list"), &caller).is_err());
}

#[test]
fn an_operator_mcp_session_reaches_governed_tools() {
    let session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Agent, McpCapability::Operator]),
        ..ToolSessionContext::default()
    };
    let caller = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&session));

    assert!(authorize(operation("orbit.workflow.run.list"), &caller).is_ok());
    assert!(authorize(operation("orbit.task.delete"), &caller).is_ok());
}

#[test]
fn a_session_only_denial_does_not_advise_an_override_it_would_ignore() {
    let caller =
        CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&ToolSessionContext::default()));
    let denial =
        authorize(operation("orbit.workflow.run.list"), &caller).expect_err("must be denied");
    let message = denial.to_string();

    assert_eq!(denial.resolution, CapabilityResolution::SessionOnly);
    assert!(
        !message.contains(&format!("re-run it with {OPERATOR_OVERRIDE_ENV}=1")),
        "the MCP surface ignores the override, so it must not be advised: {message}"
    );
    assert!(message.contains("orbit mcp serve --operator"), "{message}");
}

#[test]
fn ungoverned_operations_have_no_registry_entry_to_enforce() {
    // The registry is opt-in: an operation absent from it is not gated. This
    // pins retained public operations without granting any retired VCS/PR
    // operation through authorization.
    for tool in ["git.commit", "fs.write", "proc.spawn"] {
        assert!(
            governed_tool(tool).is_none(),
            "'{tool}' remains outside the governed-operation registry"
        );
    }
}

/// [ORB-12564] A session that arrived over SSH is decided exactly like a local
/// one — the destination honors the authority in the argv the caller composed,
/// because an SSH login to the destination is ownership of it. The forwarded
/// label rides along as attribution and nothing more.
fn remote_session(
    caller_machine_id: &str,
    effective: BTreeSet<McpCapability>,
) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: effective,
        caller_machine_id: Some(caller_machine_id.to_string()),
        transport: Some(orbit_types::tool::McpTransport::SshMcp),
        ..ToolSessionContext::default()
    }
}

#[test]
fn a_remote_session_resolves_the_same_way_a_local_stamp_does() {
    let remote = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&remote_session(
        "hm_alpha",
        BTreeSet::from([McpCapability::Agent, McpCapability::Operator]),
    )));
    let stamped = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Agent, McpCapability::Operator]),
        ..ToolSessionContext::default()
    }));

    assert_eq!(remote.provenance(), CallerProvenance::Session);
    assert_eq!(stamped.provenance(), CallerProvenance::Session);
    assert_eq!(remote.grants(), stamped.grants());
    assert!(
        authorize(operation("orbit.command.exec"), &remote).is_ok(),
        "an operator-propagated remote session performs governed operations"
    );
}

#[test]
fn a_remote_session_without_operator_is_refused_and_told_where_to_raise_it() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&remote_session(
        "hm_alpha",
        BTreeSet::from([McpCapability::Agent]),
    )));

    let denial = authorize(operation("orbit.command.exec"), &caller)
        .expect_err("an agent session must not reach command execution");
    let message = denial.to_string();

    assert!(message.contains("operator"), "{message}");
    assert!(
        message.contains("orbit mcp serve --operator"),
        "the remedy is on the calling side and must be named: {message}"
    );
    assert!(
        message.contains("propagates into the argv every SSH destination is started with"),
        "a remote caller must be told its own `--operator` is what reaches the destination: \
         {message}"
    );
    assert!(
        message.contains(&format!(
            "{OPERATOR_OVERRIDE_ENV} in that process's environment is \
             deliberately ignored"
        )),
        "the override is named only to say it is inert here: {message}"
    );
}

#[test]
fn the_forwarded_caller_label_survives_to_the_denial_for_attribution() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&remote_session(
        "hm_alpha",
        BTreeSet::from([McpCapability::Agent]),
    )));

    let denial = authorize(operation("orbit.workflow.ship"), &caller).expect_err("denied");

    assert_eq!(denial.remote_caller_machine_id.as_deref(), Some("hm_alpha"));
    assert_eq!(caller.remote_caller_machine_id(), Some("hm_alpha"));
}

#[test]
fn a_local_session_carries_no_remote_caller_attribution() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        caller_machine_id: Some("hm_self".to_string()),
        ..ToolSessionContext::default()
    }));

    assert_eq!(
        caller.remote_caller_machine_id(),
        None,
        "this machine's own identity is not a statement about a caller elsewhere"
    );
}
