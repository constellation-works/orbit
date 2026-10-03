//! The `name[:weight]` pool grammar, admitted once for configuration,
//! `orbit config set` and CLI overrides alike [ORB-12604].

use crate::canonical_crew_pool;
use orbit_types::identity::{Crew, CrewAssignment};
use std::collections::BTreeMap;

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
                    enabled: true,
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
