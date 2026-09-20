//! The `name[:weight]` pool grammar, admitted once for configuration,
//! `orbit config set` and CLI overrides alike [ORB-12604].

use std::collections::BTreeMap;

use orbit_types::identity::{Crew, CrewAssignment};

use crate::{CrewPoolEntry, canonical_crew_pool, canonical_crew_pool_entries};

const SETTING: &str = "workflow.medium_complexity_crews";

fn crews() -> BTreeMap<String, Crew> {
    ["grok", "opus", "sol", "terra"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                Crew {
                    name: name.to_string(),
                    assignment: CrewAssignment {
                        model: format!("{name}-model"),
                        provider: "claude".to_string(),
                        effort: None,
                    },
                    description: None,
                    tags: Vec::new(),
                },
            )
        })
        .collect()
}

fn pool(entries: &[&str]) -> Result<Vec<String>, String> {
    let entries: Vec<String> = entries.iter().map(ToString::to_string).collect();
    canonical_crew_pool(&entries, &crews(), SETTING)
        .map(|pool| pool.to_setting_value())
        .map_err(|error| error.to_string())
}

#[test]
fn bare_pools_trim_deduplicate_and_weigh_one_ticket_each() {
    let entries = vec![
        "terra".to_string(),
        " grok ".to_string(),
        "terra".to_string(),
    ];
    let canonical = canonical_crew_pool(&entries, &crews(), SETTING).expect("bare pool");
    assert!(!canonical.weighted);
    assert_eq!(
        canonical.entries,
        vec![CrewPoolEntry::bare("grok"), CrewPoolEntry::bare("terra")]
    );
    // Rendering stays unsuffixed, so a pre-weights configuration round-trips.
    assert_eq!(canonical.to_setting_value(), vec!["grok", "terra"]);
    assert_eq!(pool(&[]), Ok(Vec::new()));
}

#[test]
fn weighted_pools_keep_their_weights_and_render_with_the_suffix() {
    let entries = vec![
        "grok:70".to_string(),
        " opus : 10 ".to_string(),
        "sol:0".to_string(),
    ];
    let canonical = canonical_crew_pool(&entries, &crews(), SETTING).expect("weighted pool");
    assert!(canonical.weighted);
    assert_eq!(
        canonical.entries,
        vec![
            CrewPoolEntry {
                name: "grok".into(),
                weight: 70
            },
            CrewPoolEntry {
                name: "opus".into(),
                weight: 10
            },
            // Weight 0 parks a crew without deleting it from the pool.
            CrewPoolEntry {
                name: "sol".into(),
                weight: 0
            },
        ]
    );
    assert_eq!(
        canonical.to_setting_value(),
        vec!["grok:70", "opus:10", "sol:0"]
    );
}

#[test]
fn malformed_pools_name_the_setting_they_came_from() {
    for (entries, expected) in [
        (vec!["grok:50", "terra"], "all bare crew names"),
        (vec!["grok", "terra:50"], "all bare crew names"),
        (vec!["grok:50", "grok:20"], "more than once"),
        (vec!["grok:-1"], "non-negative whole number"),
        (vec!["grok:2.5"], "non-negative whole number"),
        (vec!["grok:"], "non-negative whole number"),
        (vec!["grok:70", "terra:"], "non-negative whole number"),
        (vec!["grok:0", "terra:0"], "weight above 0"),
        (vec![":50"], "non-empty crew names"),
        (vec!["missing:50"], "is not defined in [crews.*]"),
    ] {
        let error = pool(&entries).expect_err(&format!("{entries:?} must be rejected"));
        assert!(error.contains(SETTING), "{error}");
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn a_captured_pool_reresolves_without_failing_a_running_admission() {
    let entries = vec![
        CrewPoolEntry {
            name: "terra".into(),
            weight: 30,
        },
        CrewPoolEntry::bare("grok"),
    ];
    assert_eq!(
        canonical_crew_pool_entries(&entries, &crews(), SETTING).expect("captured pool"),
        vec![
            CrewPoolEntry::bare("grok"),
            CrewPoolEntry {
                name: "terra".into(),
                weight: 30
            },
        ]
    );
    let retired = vec![CrewPoolEntry::bare("retired")];
    let error = canonical_crew_pool_entries(&retired, &crews(), SETTING)
        .expect_err("a crew deleted since admission");
    assert!(error.to_string().contains(SETTING), "{error}");
}

#[test]
fn persisted_entries_deserialize_from_the_plain_name_shape_too() {
    let legacy: Vec<CrewPoolEntry> =
        serde_json::from_value(serde_json::json!(["grok", "terra"])).expect("legacy shape");
    assert_eq!(
        legacy,
        vec![CrewPoolEntry::bare("grok"), CrewPoolEntry::bare("terra")]
    );
    let weighted: Vec<CrewPoolEntry> =
        serde_json::from_value(serde_json::json!([{"name": "grok", "weight": 70}]))
            .expect("weighted shape");
    assert_eq!(
        weighted,
        vec![CrewPoolEntry {
            name: "grok".into(),
            weight: 70
        }]
    );
    assert_eq!(
        serde_json::to_value(&weighted).expect("serialize"),
        serde_json::json!([{"name": "grok", "weight": 70}])
    );
}
