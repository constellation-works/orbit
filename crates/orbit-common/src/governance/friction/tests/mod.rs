//! Registry invariants for the friction operation table.
//!
//! These guard the contract every derived surface leans on: verb ↔ spec is a
//! bijection, names are unique and well-formed, and the shipped MCP/CLI strings
//! are what consumers already depend on.

mod title;

use std::collections::BTreeSet;

use super::operations::{FRICTION_OPERATIONS, FrictionVerb, friction_operation};
use super::{
    DEFAULT_FRICTION_TAGS, FRICTION_TAG_ALIASES, friction_tag_aliases_literal,
    friction_tags_literal, normalize_friction_tag_aliases,
};
use crate::governance::operation::CliArgKind;
use orbit_types::tool::McpToolScope;

const ALL_VERBS: &[FrictionVerb] = &[
    FrictionVerb::Add,
    FrictionVerb::List,
    FrictionVerb::Show,
    FrictionVerb::Stats,
    FrictionVerb::Tags,
    FrictionVerb::Update,
    FrictionVerb::Resolve,
];

#[test]
fn every_verb_has_exactly_one_spec_and_vice_versa() {
    assert_eq!(FRICTION_OPERATIONS.len(), ALL_VERBS.len());
    for verb in ALL_VERBS {
        let matches = FRICTION_OPERATIONS
            .iter()
            .filter(|spec| spec.verb == *verb)
            .count();
        assert_eq!(matches, 1, "{verb:?} must appear exactly once");
        assert_eq!(verb.spec().verb, *verb, "{verb:?} spec lookup disagrees");
    }
}

#[test]
fn verb_names_and_tool_names_are_unique_and_consistent() {
    let names: BTreeSet<&str> = FRICTION_OPERATIONS.iter().map(|spec| spec.name).collect();
    assert_eq!(names.len(), FRICTION_OPERATIONS.len());

    for spec in FRICTION_OPERATIONS {
        assert_eq!(
            spec.tool_name,
            format!("orbit.friction.{}", spec.name),
            "tool name must be orbit.friction.<verb>"
        );
        assert_eq!(spec.verb.as_str(), spec.name);
        assert_eq!(spec.verb.tool_name(), spec.tool_name);
    }
}

#[test]
fn parameter_names_are_unique_within_each_operation() {
    for spec in FRICTION_OPERATIONS {
        let names: BTreeSet<&str> = spec.params.iter().map(|param| param.name).collect();
        assert_eq!(
            names.len(),
            spec.params.len(),
            "{} has duplicate parameter names",
            spec.name
        );
    }
}

#[test]
fn at_most_one_positional_per_operation_and_it_is_the_id() {
    for spec in FRICTION_OPERATIONS {
        let positionals: Vec<&str> = spec
            .params
            .iter()
            .filter(|param| {
                matches!(
                    param.cli.map(|binding| binding.kind),
                    Some(CliArgKind::Positional)
                )
            })
            .map(|param| param.name)
            .collect();
        assert!(
            positionals.len() <= 1,
            "{} declares multiple positionals: {positionals:?}",
            spec.name
        );
        if let Some(name) = positionals.first() {
            assert_eq!(*name, "id");
        }
    }
}

#[test]
fn subcommand_order_is_the_shipped_help_order() {
    let order: Vec<&str> = FRICTION_OPERATIONS.iter().map(|spec| spec.name).collect();
    assert_eq!(
        order,
        vec!["add", "list", "show", "stats", "tags", "update", "resolve"],
        "`orbit friction --help` lists subcommands in registry order"
    );
}

#[test]
fn mcp_exposure_matches_the_shipped_conformance_contract() {
    // docs/design/mcp-bridge/references/conformance-v1.yaml
    for verb in [FrictionVerb::Add, FrictionVerb::List, FrictionVerb::Update] {
        assert_eq!(verb.spec().mcp_scope, Some(McpToolScope::WorkspaceRequired));
    }
    // Reads that `list` already covers, aggregate stats, and operator
    // resolution stay on the CLI / dashboard surface [ORB-10798].
    for verb in [
        FrictionVerb::Show,
        FrictionVerb::Tags,
        FrictionVerb::Stats,
        FrictionVerb::Resolve,
    ] {
        assert_eq!(verb.spec().mcp_scope, None);
    }
}

#[test]
fn only_add_rejects_the_legacy_agent_field() {
    for spec in FRICTION_OPERATIONS {
        assert_eq!(
            spec.rejects_agent_field,
            spec.verb == FrictionVerb::Add,
            "{} agent-field policy drifted",
            spec.name
        );
    }
}

#[test]
fn tags_parameter_descriptions_list_the_default_taxonomy() {
    for verb in [FrictionVerb::Add, FrictionVerb::Update] {
        let param = verb
            .spec()
            .params
            .iter()
            .find(|param| param.name == "tags")
            .expect("tags parameter");
        let description = param
            .mcp_description
            .expect("tags is advertised over MCP")
            .resolve();
        assert!(
            description.contains(&friction_tags_literal()),
            "{description}"
        );
        for (tag, _gloss) in DEFAULT_FRICTION_TAGS {
            assert!(description.contains(tag), "{description} should list {tag}");
        }
        assert!(
            description.contains(&friction_tag_aliases_literal()),
            "{description}"
        );
    }
}

#[test]
fn every_friction_tag_alias_normalizes_to_its_canonical_category() {
    for (alias, canonical) in FRICTION_TAG_ALIASES {
        let (tags, substitutions) = normalize_friction_tag_aliases(vec![(*alias).to_string()]);
        assert_eq!(tags, vec![*canonical], "alias {alias}");
        assert_eq!(
            substitutions,
            vec![(alias.to_string(), canonical.to_string())],
            "alias {alias} must be reported to the caller"
        );
    }
}

#[test]
fn tag_flag_is_singular_while_the_wire_field_is_plural() {
    for verb in [FrictionVerb::Add, FrictionVerb::Update] {
        let param = verb
            .spec()
            .params
            .iter()
            .find(|param| param.name == "tags")
            .expect("tags parameter");
        assert_eq!(param.cli_long(), Some("tag"));
        assert!(matches!(
            param.cli.map(|binding| binding.kind),
            Some(CliArgKind::Flag {
                delimiter: Some(','),
                ..
            })
        ));
    }

    // `list` filters on a single tag, so there the wire field is singular too.
    let list_tag = FrictionVerb::List
        .spec()
        .params
        .iter()
        .find(|param| param.name == "tag")
        .expect("tag filter");
    assert_eq!(list_tag.cli_long(), Some("tag"));
}

#[test]
fn lookup_by_name_round_trips_every_verb() {
    for spec in FRICTION_OPERATIONS {
        assert_eq!(
            friction_operation(spec.name).map(|found| found.tool_name),
            Some(spec.tool_name)
        );
    }
    assert!(friction_operation("nope").is_none());
}
