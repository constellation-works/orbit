//! Named crew pools for automatic task admission. Empty pools disable selection.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::identity::{Crew, resolve_crew};
use orbit_types::task::TaskComplexity;
use serde::{Deserialize, Serialize};

/// Separates a pool entry's crew name from its relative draw weight.
const WEIGHT_SEPARATOR: char = ':';
/// Every member of a bare pool carries one ticket, so the draw is uniform.
const BARE_WEIGHT: u32 = 1;

/// Per-complexity configuration or run overrides. `None` inherits the matching
/// configured pool; `Some([])` explicitly disables that pool for a run.
///
/// Each entry is written `name` or `name:weight`; a pool is either all bare or
/// all weighted. [`canonical_crew_pool`] is the one place that grammar is
/// admitted, so configuration, `orbit config set` and CLI overrides reject the
/// same malformed pools.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityCrewPools {
    /// Pool for low-complexity tasks.
    pub low: Option<Vec<String>>,
    /// Pool for medium-complexity tasks.
    pub medium: Option<Vec<String>>,
    /// Pool for hard-complexity tasks.
    pub hard: Option<Vec<String>>,
    /// Pool for xhard-complexity tasks, the reserved top tier.
    pub xhard: Option<Vec<String>>,
}

impl ComplexityCrewPools {
    /// The matching pool; unassessed tasks have no automatic pool.
    pub fn pool(&self, complexity: TaskComplexity) -> Option<&[String]> {
        match complexity {
            TaskComplexity::Low => self.low.as_deref(),
            TaskComplexity::Medium => self.medium.as_deref(),
            TaskComplexity::Hard => self.hard.as_deref(),
            TaskComplexity::XHard => self.xhard.as_deref(),
            TaskComplexity::Unassessed => None,
        }
    }
}

/// One validated pool member: a canonical crew name and its relative,
/// non-negative draw weight. Weights are ratios, not percentages, so they need
/// not sum to anything in particular; weight `0` parks a crew without deleting
/// it from the pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrewPoolEntry {
    /// Canonical crew name, as resolved against the configured registry.
    pub name: String,
    /// Relative number of tickets this crew holds in the draw.
    pub weight: u32,
}

impl CrewPoolEntry {
    /// A bare pool member, holding the single ticket a name carried before
    /// weights existed.
    pub fn bare(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            weight: BARE_WEIGHT,
        }
    }
}

/// Pools persisted before weights existed are plain crew names, so a stored
/// entry deserialises from either shape and an old run resumes without a
/// reroll.
impl<'de> Deserialize<'de> for CrewPoolEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Legacy(String),
            Weighted { name: String, weight: u32 },
        }

        Ok(match Shape::deserialize(deserializer)? {
            Shape::Legacy(name) => Self::bare(name),
            Shape::Weighted { name, weight } => Self { name, weight },
        })
    }
}

/// A validated pool: its canonical entries plus whether the operator wrote
/// weights. The flag only drives rendering, so a bare pool still round-trips
/// through `orbit config get` without growing `:1` suffixes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanonicalCrewPool {
    /// Members in canonical name order, one ticket bucket each.
    pub entries: Vec<CrewPoolEntry>,
    /// Whether the pool was written with explicit weights.
    pub weighted: bool,
}

impl CanonicalCrewPool {
    /// The `name[:weight]` strings this pool is stored as in `config.toml`.
    pub fn to_setting_value(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| {
                if self.weighted {
                    format!("{}{WEIGHT_SEPARATOR}{}", entry.name, entry.weight)
                } else {
                    entry.name.clone()
                }
            })
            .collect()
    }
}

/// Validate and canonicalize one pool at configuration or run admission.
///
/// Bare pools keep their historical behaviour: entries are trimmed, resolved
/// against the crew registry, and duplicates collapse to one ticket per crew.
/// A weighted pool names each crew once and must give at least one of them a
/// weight above `0`. Mixing the two forms is a configuration error.
pub fn canonical_crew_pool(
    entries: &[String],
    crews: &BTreeMap<String, Crew>,
    setting: &str,
) -> Result<CanonicalCrewPool, OrbitError> {
    let mut weighted: Option<bool> = None;
    let mut canonical: BTreeMap<String, u32> = BTreeMap::new();
    for raw in entries {
        let (name, weight) = parse_entry(raw, setting)?;
        if *weighted.get_or_insert(weight.is_some()) != weight.is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "{setting} must be either all bare crew names or all name:weight entries, not a \
                 mix of both"
            )));
        }
        let crew = resolve_crew(name, crews)
            .map_err(|error| OrbitError::InvalidInput(format!("{setting}: {error}")))?;
        let previous = canonical.insert(crew.name.clone(), weight.unwrap_or(BARE_WEIGHT));
        if previous.is_some() && weight.is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "{setting} names crew '{}' more than once; a weighted pool gives each crew one \
                 entry",
                crew.name
            )));
        }
    }
    let weighted = weighted.unwrap_or(false);
    let entries = collect_entries(canonical);
    if weighted && entries.iter().all(|entry| entry.weight == 0) {
        return Err(OrbitError::InvalidInput(format!(
            "{setting} must give at least one crew a weight above 0; use [] to disable the pool"
        )));
    }
    Ok(CanonicalCrewPool { entries, weighted })
}

/// Re-resolve an already admitted pool against the current crew registry.
///
/// The entries were canonical when the run was admitted, so this only has to
/// prove every crew still exists. Two entries that now collapse onto one crew
/// keep the first ticket rather than failing a run that is already running.
pub fn canonical_crew_pool_entries(
    entries: &[CrewPoolEntry],
    crews: &BTreeMap<String, Crew>,
    setting: &str,
) -> Result<Vec<CrewPoolEntry>, OrbitError> {
    let mut canonical: BTreeMap<String, u32> = BTreeMap::new();
    for entry in entries {
        let name = entry.name.trim();
        if name.is_empty() {
            return Err(OrbitError::InvalidInput(empty_name_message(setting)));
        }
        let crew = resolve_crew(name, crews)
            .map_err(|error| OrbitError::InvalidInput(format!("{setting}: {error}")))?;
        canonical.entry(crew.name).or_insert(entry.weight);
    }
    Ok(collect_entries(canonical))
}

fn collect_entries(canonical: BTreeMap<String, u32>) -> Vec<CrewPoolEntry> {
    canonical
        .into_iter()
        .map(|(name, weight)| CrewPoolEntry { name, weight })
        .collect()
}

/// Split one raw entry into its crew name and, when written, its weight.
///
/// The weight is taken from the last colon so the separator stays unambiguous:
/// a suffix that is not a non-negative whole number is a malformed weight, not
/// part of the crew name.
fn parse_entry<'a>(raw: &'a str, setting: &str) -> Result<(&'a str, Option<u32>), OrbitError> {
    let trimmed = raw.trim();
    let (name, weight) = match trimmed.rsplit_once(WEIGHT_SEPARATOR) {
        Some((name, weight)) => {
            let weight = weight.trim().parse::<u32>().map_err(|_| {
                OrbitError::InvalidInput(format!(
                    "{setting} entry '{trimmed}' must weigh a non-negative whole number, as in \
                     'grok:70'"
                ))
            })?;
            (name.trim(), Some(weight))
        }
        None => (trimmed, None),
    };
    if name.is_empty() {
        return Err(OrbitError::InvalidInput(empty_name_message(setting)));
    }
    Ok((name, weight))
}

fn empty_name_message(setting: &str) -> String {
    format!("{setting} must contain non-empty crew names; use [] to disable the pool")
}
