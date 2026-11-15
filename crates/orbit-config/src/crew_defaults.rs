//! The built-in crew registry: the crews a config without `[crews]` resolves to.
//!
//! A private leaf: it depends only on `std`, `orbit-common` and `orbit-types`, so
//! snapshot admission, resolved config, layering provenance and seeding can all
//! read the same defaults without depending on one another.
//! `scripts/check-dependency-direction.sh` rejects any import of a higher
//! config module here.

use std::collections::BTreeMap;

use orbit_common::model_defaults::{
    ANTIGRAVITY_ARGON_MODEL, ANTIGRAVITY_DEFAULT_MODEL, CLAUDE_DEFAULT_STRONG, CLAUDE_DEFAULT_WEAK,
    CLAUDE_FABLE_MODEL, CLAUDE_HAIKU_MODEL, CODEX_ASTRA_MODEL, CODEX_LUNA_MODEL, CODEX_SOL_MODEL,
    CODEX_TERRA_MODEL, COPILOT_DEFAULT_MODEL, GROK_DEFAULT_MODEL, MUSE_DEFAULT_MODEL,
    MUSE_WONDER_FULL_MODEL, MUSE_WONDER_MODEL,
};
use orbit_types::identity::{Crew, CrewAssignment};

pub(crate) fn builtin_default_crews() -> BTreeMap<String, Crew> {
    let mut crews = BTreeMap::new();
    for (name, model, provider) in [
        ("opus", CLAUDE_DEFAULT_STRONG, "claude"),
        ("sonnet", CLAUDE_DEFAULT_WEAK, "claude"),
        ("haiku", CLAUDE_HAIKU_MODEL, "claude"),
        ("fable", CLAUDE_FABLE_MODEL, "claude"),
        ("sol", CODEX_SOL_MODEL, "codex"),
        ("terra", CODEX_TERRA_MODEL, "codex"),
        ("luna", CODEX_LUNA_MODEL, "codex"),
        ("astra", CODEX_ASTRA_MODEL, "codex"),
        ("gemini-flash", ANTIGRAVITY_DEFAULT_MODEL, "antigravity"),
        ("argon", ANTIGRAVITY_ARGON_MODEL, "antigravity"),
        ("grok", GROK_DEFAULT_MODEL, "grok"),
        ("copilot", COPILOT_DEFAULT_MODEL, "copilot"),
        ("muse-spark", MUSE_DEFAULT_MODEL, "muse"),
        ("muse-wonder", MUSE_WONDER_MODEL, "muse"),
        ("muse-wonder-full", MUSE_WONDER_FULL_MODEL, "muse"),
    ] {
        crews.insert(
            name.to_string(),
            Crew {
                name: name.to_string(),
                assignment: crew_assignment(model, provider),
                description: None,
                tags: Vec::new(),
                enabled: true,
                env: Vec::new(),
                vision: true,
            },
        );
    }
    crews
}

fn crew_assignment(model: &str, provider: &str) -> CrewAssignment {
    CrewAssignment {
        model: model.to_string(),
        provider: provider.to_string(),
        effort: None,
    }
}
