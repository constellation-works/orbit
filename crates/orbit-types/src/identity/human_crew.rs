//! Human crews: `[crews.<name>] kind = "human"` names an operator, not an
//! agent. Tasks assigned to one wait for that person; no drain, follower or
//! task pilot ever picks them up.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The `[crews.<name>] kind` value that declares a human crew.
pub const HUMAN_CREW_KIND: &str = "human";

/// The `[crews.<name>] kind` value of an ordinary agent crew, the default.
pub const AGENT_CREW_KIND: &str = "agent";

/// An operator crew. It has no provider, model, effort or env: nothing
/// dispatches it, so it is kept apart from the agent crew registry every pool,
/// default and redraw resolves against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanCrew {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// The human crew a task crew value names, or `None` when it names an agent
/// crew or nothing. The value is compared trimmed and whole: a human crew has
/// no `@<effort>` form.
#[must_use]
pub fn find_human_crew<'a>(
    name: Option<&str>,
    human_crews: &'a BTreeMap<String, HumanCrew>,
) -> Option<&'a HumanCrew> {
    let name = name.map(str::trim).filter(|name| !name.is_empty())?;
    human_crews.get(name)
}

/// Refusal text for a crew pool, default, overlay or dispatch that names a
/// human crew. `subject` says what named it.
#[must_use]
pub fn human_crew_refusal(subject: &str, crew: &str) -> String {
    format!(
        "{subject} names crew '{crew}', which is a human crew ([crews.{crew}] kind = \
         \"{HUMAN_CREW_KIND}\"); human crews take operator work only, so no agent dispatch, crew \
         pool, default_crew, experiment arm or redraw may use them"
    )
}
