use serde_json::json;

use super::super::{CREW_DISCOVERY_SCHEMA_VERSION, CrewDiscoveryEntryV1, CrewDiscoveryV1};
use crate::identity::{Crew, CrewAssignment};

#[test]
fn crew_normalization_canonicalizes_alias_and_metadata() {
    let crew = Crew {
        name: " alpha ".to_string(),
        assignment: CrewAssignment {
            provider: " OpenAI ".to_string(),
            model: " gpt-test ".to_string(),
            effort: None,
        },
        description: Some("  Fast implementation  ".to_string()),
        tags: vec![
            " zeta ".to_string(),
            String::new(),
            "alpha".to_string(),
            "alpha".to_string(),
        ],
    };

    let normalized = CrewDiscoveryEntryV1::from_crew(&crew).expect("crew normalizes");
    assert_eq!(normalized.name, "alpha");
    assert_eq!(normalized.provider, "codex");
    assert_eq!(normalized.model, "gpt-test");
    assert_eq!(
        normalized.description.as_deref(),
        Some("Fast implementation")
    );
    assert_eq!(normalized.tags, vec!["alpha", "zeta"]);
}

#[test]
fn discovery_wire_shape_is_stable_after_rust_type_rename() {
    let discovery = CrewDiscoveryV1 {
        schema_version: CREW_DISCOVERY_SCHEMA_VERSION,
        workspace_id: "ws_alpha".to_string(),
        owner_machine_id: Some("hm_alpha".to_string()),
        default_crew: Some("alpha".to_string()),
        crews: vec![CrewDiscoveryEntryV1 {
            name: "alpha".to_string(),
            provider: "codex".to_string(),
            model: "gpt-test".to_string(),
            description: Some("Fast implementation".to_string()),
            tags: vec!["fast".to_string()],
        }],
    };

    assert_eq!(
        serde_json::to_value(discovery).expect("serialize crew discovery"),
        json!({
            "schema_version": 2,
            "workspace_id": "ws_alpha",
            "owner_machine_id": "hm_alpha",
            "default_crew": "alpha",
            "crews": [{
                "name": "alpha",
                "provider": "codex",
                "model": "gpt-test",
                "description": "Fast implementation",
                "tags": ["fast"]
            }]
        })
    );
}
