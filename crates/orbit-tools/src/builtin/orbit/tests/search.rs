//! Schema tests for orbit.search tool.
//
// Migrated from nested `search/tests/` (anti-pattern child of source)
// to sibling layout under `orbit/tests/` per ORB-00243 and
// docs/design-patterns/test_layout.md.

use super::super::search::*;
use crate::Tool;

#[test]
fn search_schema_advertises_only_lexical_inputs() {
    let schema = OrbitSearchTool.schema();
    let params = schema
        .parameters
        .iter()
        .map(|param| (param.name.as_str(), param.param_type.as_str()))
        .collect::<Vec<_>>();

    assert!(!params.contains(&("hybrid", "boolean")));
    assert!(!params.contains(&("semantic", "string")));
    assert!(!params.iter().any(|(name, _)| *name == "related"));
    assert!(!params.iter().any(|(name, _)| *name == "field"));
    assert!(!params.iter().any(|(name, _)| *name == "embedding_model"));
}

#[test]
fn search_schema_exposes_federated_scope_without_shadowing_the_routing_selector() {
    let schema = OrbitSearchTool.schema();
    let params = schema
        .parameters
        .iter()
        .map(|param| (param.name.as_str(), param.param_type.as_str()))
        .collect::<Vec<_>>();

    // ORB-11027: the scope is `workspaces`; `workspace` stays reserved for the
    // MCP routing selector, which `orbit-mcp` injects on every
    // workspace-scoped tool.
    assert!(params.contains(&("workspaces", "string_list")));
    assert!(params.contains(&("all_workspaces", "boolean")));
    assert!(!params.iter().any(|(name, _)| *name == "workspace"));
}
