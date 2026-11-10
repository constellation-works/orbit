//! Tool families dispatch through `action`: each action's required fields are
//! checked before any handler runs, and a refusal names those fields.

use orbit_common::OrbitError;
use orbit_tools::{ToolContext, ToolRegistry};
use orbit_types::tool::TOOL_FAMILIES;
use serde_json::{Value, json};

/// Every family action that requires fields, with those fields as a caller
/// names them (an operation's own `action` is `mode`).
const REQUIRED: &[(&str, &str, &[&str])] = &[
    (
        "orbit.task.recover",
        "recover_claim",
        &[
            "claim_id",
            "expected_phase",
            "status",
            "reason",
            "request_key",
        ],
    ),
    ("orbit.task.recover", "requeue", &["id", "reason"]),
    ("orbit.task.recover", "repilot", &["id"]),
    (
        "orbit.task.recover",
        "retry_handoff",
        &[
            "handoff_id",
            "expected_candidate_commit",
            "expected_base_commit",
            "expected_latest_attempt",
            "expected_latest_state",
            "reason",
            "request_key",
        ],
    ),
    (
        "orbit.task.recover",
        "revoke_handoff",
        &[
            "handoff_id",
            "expected_candidate_commit",
            "expected_base_commit",
            "reason",
            "request_key",
        ],
    ),
    (
        "orbit.task.recover",
        "review_reset",
        &["id", "lineage_key", "reason"],
    ),
    ("orbit.task.recover", "reconcile_review", &["mode", "id"]),
    ("orbit.run", "show", &["id"]),
    ("orbit.run", "observe", &["id"]),
    ("orbit.run", "resume", &["id"]),
    ("orbit.workflow", "auto", &["mode"]),
    ("orbit.workflow", "ship", &["task_ids"]),
    ("orbit.workflow", "land", &["task_id"]),
    ("orbit.auto_task", "add", &["name", "schedule", "template"]),
    ("orbit.auto_task", "update", &["name"]),
    ("orbit.auto_task", "mint", &["name"]),
    ("orbit.decision", "show", &["id"]),
    (
        "orbit.decision",
        "raise",
        &["task_id", "question", "options"],
    ),
    ("orbit.codebase", "show", &["path"]),
    ("orbit.codebase", "related", &["path"]),
    ("orbit.friction", "add", &["body"]),
    ("orbit.friction", "update", &["id"]),
    ("orbit.task.artifact", "get", &["id", "path"]),
    ("orbit.task.artifact", "put", &["id", "source_path"]),
];

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
}

fn invalid_input(registry: &ToolRegistry, tool: &str, input: Value) -> String {
    match registry.execute(tool, &ToolContext::default(), input.clone()) {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("{tool} {input} must be refused as invalid input, got {other:?}"),
    }
}

#[test]
fn each_family_action_refuses_a_call_missing_its_required_fields_by_name() {
    let registry = registry();
    for (family, action, fields) in REQUIRED {
        let message = invalid_input(&registry, family, json!({ "action": action }));
        assert!(
            message.contains(&format!("action `{action}`")),
            "{family} {action}: refusal must name the action: {message}"
        );
        for field in *fields {
            assert!(
                message.contains(&format!("`{field}`")),
                "{family} {action}: refusal must name required `{field}`: {message}"
            );
        }

        // Supplying the first of several fields still names it as
        // required, but no longer as missing.
        let Some((first, rest)) = fields.split_first().filter(|(_, rest)| !rest.is_empty()) else {
            continue;
        };
        let message = invalid_input(
            &registry,
            family,
            json!({ "action": action, *first: "supplied" }),
        );
        let missing = message
            .split_once("missing ")
            .map(|(_, missing)| missing)
            .unwrap_or_default();
        assert!(
            !missing.contains(&format!("`{first}`")),
            "{family} {action}: a supplied `{first}` is not missing: {message}"
        );
        for field in rest {
            assert!(
                missing.contains(&format!("`{field}`")),
                "{family} {action}: `{field}` is still missing: {message}"
            );
        }
    }
}

#[test]
fn a_family_action_keeps_its_operations_own_mode_field() {
    // Ship's `mode` (`pr` or `local`) is its own field, not a sub-command.
    let message = invalid_input(
        &registry(),
        "orbit.workflow",
        json!({ "action": "ship", "mode": "local" }),
    );
    assert!(
        message.contains("`task_ids`") && !message.contains("takes no `mode`"),
        "ship keeps its mode field: {message}"
    );
}

#[test]
fn a_family_action_name_is_refused_with_the_family_call_guidance() {
    let registry = registry();
    for name in ["orbit.run:observe", "orbit.workflow.run.observe"] {
        let error = registry
            .resolve_tool_call(name, json!({}))
            .expect_err("a family action name is not a tool");
        let message = error.to_string();
        assert!(
            message.contains(name),
            "{name}: refusal must name the called name: {message}"
        );
        assert!(
            message.contains("(call orbit.run with action \"observe\")"),
            "{name}: refusal must name the family call: {message}"
        );
        let error = registry
            .refuse_family_operation_name(name)
            .expect_err("a family action name is not a tool");
        assert!(
            error
                .to_string()
                .contains("(call orbit.run with action \"observe\")"),
            "{name}: refusal must name the family call: {error}"
        );
    }
    // An unknown action of a known family keeps the generic refusal path.
    assert!(
        registry
            .resolve_tool_call("orbit.run:bogus", json!({}))
            .is_ok(),
        "an unknown family action is not a known family action name"
    );

    // Executing the colon name directly is refused the same way, and no
    // route is added for it.
    let error = registry
        .execute(
            "orbit.run:observe",
            &ToolContext::default(),
            json!({ "id": "jrun-1" }),
        )
        .expect_err("a family action name is not an executable tool");
    assert!(
        error
            .to_string()
            .contains("(call orbit.run with action \"observe\")"),
        "execute refusal must name the family call: {error}"
    );
}

#[test]
fn every_family_action_with_required_fields_is_covered() {
    let registry = registry();
    for family in TOOL_FAMILIES {
        for action in family.actions {
            let schema = registry
                .get_schema(action.operation)
                .expect("a family action's operation is registered");
            let requires_fields = schema
                .parameters
                .iter()
                .any(|param| param.required && param.name != "workspace");
            let covered = REQUIRED
                .iter()
                .any(|(name, listed, _)| *name == family.name && *listed == action.action);
            assert_eq!(
                requires_fields, covered,
                "{} action {}: required-field coverage is out of step",
                family.name, action.action
            );
        }
    }
}

#[test]
fn each_family_refuses_a_missing_unknown_or_misplaced_selector() {
    let registry = registry();
    for family in TOOL_FAMILIES {
        let actions = family
            .actions
            .iter()
            .map(|action| action.action)
            .collect::<Vec<_>>();

        let missing = invalid_input(&registry, family.name, json!({}));
        assert!(
            missing.contains("requires `action`"),
            "{}: missing action: {missing}",
            family.name
        );
        for action in &actions {
            assert!(
                missing.contains(action),
                "{}: missing-action refusal must list `{action}`: {missing}",
                family.name
            );
        }

        let unknown = invalid_input(&registry, family.name, json!({ "action": "no_such" }));
        assert!(
            unknown.contains("unknown") && unknown.contains("no_such"),
            "{}: unknown action: {unknown}",
            family.name
        );

        let takes_no_mode = |action: &&orbit_types::tool::ToolFamilyAction| {
            !action.takes_mode
                && registry.get_schema(action.operation).is_some_and(|schema| {
                    schema.parameters.iter().all(|param| param.name != "mode")
                })
        };
        if let Some(action) = family.actions.iter().find(takes_no_mode) {
            let misplaced = invalid_input(
                &registry,
                family.name,
                json!({ "action": action.action, "mode": "status" }),
            );
            assert!(
                misplaced.contains("takes no `mode`"),
                "{} {}: mode on an action without one: {misplaced}",
                family.name,
                action.action
            );
        }
    }
}
