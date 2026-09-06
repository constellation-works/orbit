#![allow(missing_docs)]

use orbit_common::test_fixtures::{TEST_CLAUDE_MODEL, TEST_CODEX_MODEL};
use orbit_types::identity::ReasoningEffort;
use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};

use crate::context::CrewConfig;

use super::super::crew::{ResolvedAgentSettings, apply_resolved_settings, resolve_from_config};

fn inline_spec() -> AgentLoopSpec {
    AgentLoopSpec {
        instruction: String::new(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: Some(TEST_CLAUDE_MODEL.to_string()),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Claude,
        wall_clock_timeout_seconds: 30,
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    }
}

#[test]
fn partial_crew_assignment_keeps_inline_fields() {
    let config = CrewConfig {
        provider: Some(Provider::Codex),
        model: None,
        reasoning_effort: None,
    };
    let resolved = resolve_from_config(&config, &inline_spec());
    assert_eq!(resolved.provider, Provider::Codex);
    assert_eq!(resolved.model.as_deref(), Some(TEST_CLAUDE_MODEL));
}

#[test]
fn full_crew_assignment_replaces_every_field() {
    let config = CrewConfig {
        provider: Some(Provider::Codex),
        model: Some(TEST_CODEX_MODEL.to_string()),
        reasoning_effort: None,
    };
    let resolved = resolve_from_config(&config, &inline_spec());
    assert_eq!(resolved.provider, Provider::Codex);
    assert_eq!(resolved.model.as_deref(), Some(TEST_CODEX_MODEL));
}

#[test]
fn apply_mutates_spec_in_place() {
    let mut spec = inline_spec();
    let resolved = ResolvedAgentSettings {
        provider: Provider::Codex,
        model: Some(TEST_CODEX_MODEL.to_string()),
        reasoning_effort: None,
    };
    apply_resolved_settings(&mut spec, &resolved);
    assert_eq!(spec.provider, Provider::Codex);
    assert_eq!(spec.model.as_deref(), Some(TEST_CODEX_MODEL));
}

#[test]
fn configured_effort_overrides_the_inline_baseline_with_the_selected_crew() {
    let config = CrewConfig {
        provider: Some(Provider::Codex),
        model: Some(TEST_CODEX_MODEL.to_string()),
        reasoning_effort: Some(ReasoningEffort::High),
    };
    let mut spec = inline_spec();
    spec.reasoning_effort = Some(ReasoningEffort::Low);

    let resolved = resolve_from_config(&config, &spec);
    apply_resolved_settings(&mut spec, &resolved);

    assert_eq!(spec.reasoning_effort, Some(ReasoningEffort::High));
}
