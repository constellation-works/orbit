//! Named crew pools for automatic task admission. Empty pools disable selection.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::identity::{Crew, resolve_crew};
use orbit_types::task::TaskComplexity;
use serde::{Deserialize, Serialize};

/// Per-complexity configuration or run overrides. `None` inherits the matching
/// configured pool; `Some([])` explicitly disables that pool for a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityCrewPools {
    /// Pool for low-complexity tasks.
    pub low: Option<Vec<String>>,
    /// Pool for medium-complexity tasks.
    pub medium: Option<Vec<String>>,
    /// Pool for hard-complexity tasks.
    pub hard: Option<Vec<String>>,
}

impl ComplexityCrewPools {
    /// The matching pool; unassessed tasks have no automatic pool.
    pub fn pool(&self, complexity: TaskComplexity) -> Option<&[String]> {
        match complexity {
            TaskComplexity::Low => self.low.as_deref(),
            TaskComplexity::Medium => self.medium.as_deref(),
            TaskComplexity::Hard => self.hard.as_deref(),
            TaskComplexity::Unassessed => None,
        }
    }
}

/// Validate and canonicalize one pool at configuration or run admission.
/// Duplicate entries have one ticket per canonical crew name.
pub fn canonical_crew_pool(
    names: &[String],
    crews: &BTreeMap<String, Crew>,
    setting: &str,
) -> Result<Vec<String>, OrbitError> {
    let mut canonical = BTreeSet::new();
    for name in names {
        if name.trim().is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "{setting} must contain non-empty crew names; use [] to disable the pool"
            )));
        }
        let crew = resolve_crew(name.trim(), crews)
            .map_err(|error| OrbitError::InvalidInput(format!("{setting}: {error}")))?;
        canonical.insert(crew.name);
    }
    Ok(canonical.into_iter().collect())
}
