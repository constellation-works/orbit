//! Registry invariants for the operation-mode operation table.

use std::collections::BTreeSet;

use super::operations::{OPERATION_MODE_OPERATIONS, OperationModeVerb, operation_mode_operation};
use crate::governance::operation::CliArgKind;

const ALL_VERBS: &[OperationModeVerb] = &[
    OperationModeVerb::Explain,
    OperationModeVerb::Enable,
    OperationModeVerb::List,
    OperationModeVerb::Show,
    OperationModeVerb::Stop,
    OperationModeVerb::Revoke,
];

#[test]
fn every_verb_has_exactly_one_spec_and_vice_versa() {
    assert_eq!(OPERATION_MODE_OPERATIONS.len(), ALL_VERBS.len());
    for verb in ALL_VERBS {
        let matches = OPERATION_MODE_OPERATIONS
            .iter()
            .filter(|spec| spec.verb == *verb)
            .count();
        assert_eq!(matches, 1, "{verb:?} must appear exactly once");
        assert_eq!(verb.spec().verb, *verb);
        assert_eq!(
            operation_mode_operation(verb.as_str()).map(|spec| spec.verb),
            Some(*verb)
        );
    }
}

#[test]
fn verb_names_tool_names_and_parameters_are_unique_and_consistent() {
    let names: BTreeSet<&str> = OPERATION_MODE_OPERATIONS
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(names.len(), OPERATION_MODE_OPERATIONS.len());
    for spec in OPERATION_MODE_OPERATIONS {
        assert_eq!(spec.tool_name, format!("orbit.operation.{}", spec.name));
        assert_eq!(spec.verb.tool_name(), spec.tool_name);
        let params: BTreeSet<&str> = spec.params.iter().map(|param| param.name).collect();
        assert_eq!(
            params.len(),
            spec.params.len(),
            "{} has duplicate params",
            spec.name
        );
        let positionals = spec
            .params
            .iter()
            .filter(|param| {
                matches!(
                    param.cli.map(|binding| binding.kind),
                    Some(CliArgKind::Positional)
                )
            })
            .count();
        assert!(
            positionals <= 1,
            "{} declares multiple positionals",
            spec.name
        );
        // Every CLI flag is kebab-case: an underscore would be a wire name
        // leaking into argv.
        for param in spec.params {
            if let Some(long) = param.cli_long() {
                assert!(
                    !long.contains('_'),
                    "{}: flag --{long} is not kebab-case",
                    spec.name
                );
            }
        }
        assert!(!spec.rejects_agent_field);
    }
}

#[test]
fn subcommand_order_is_the_shipped_help_order() {
    let order: Vec<&str> = OPERATION_MODE_OPERATIONS
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        order,
        vec!["explain", "enable", "list", "show", "stop", "revoke"]
    );
}

#[test]
fn mcp_exposure_is_cli_and_dashboard_only() {
    for spec in OPERATION_MODE_OPERATIONS {
        assert!(spec.mcp_scope.is_none(), "{} stays off MCP", spec.tool_name);
    }
}

#[test]
fn enable_requires_scope_window_and_rights() {
    let required: Vec<&str> = OperationModeVerb::Enable
        .spec()
        .params
        .iter()
        .filter(|param| param.required)
        .map(|param| param.name)
        .collect();
    assert_eq!(required, vec!["task_ids", "window", "rights"]);
}
