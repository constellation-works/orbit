//! `workflow.crew_performance_types`: the ordered task-type list the
//! implementer scorecard classifies tasks by [crew-performance scope].

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::fs::path_glob::RelativePathGlob;
use serde::{Deserialize, Serialize};

use crate::ConfigSnapshot;

/// The key as written in `config.toml`.
pub const CREW_PERFORMANCE_TYPES_KEY: &str = "workflow.crew_performance_types";
/// The `workflow.crew_performance` key as written in `config.toml`.
pub const CREW_PERFORMANCE_KEY: &str = "workflow.crew_performance";
/// The `workflow.crew_performance_window_days` key as written in `config.toml`.
pub const CREW_PERFORMANCE_WINDOW_DAYS_KEY: &str = "workflow.crew_performance_window_days";
/// The default evidence window of a crew-performance snapshot, in days.
pub const DEFAULT_CREW_PERFORMANCE_WINDOW_DAYS: u32 = 30;
/// The `workflow.crew_performance_strength` key as written in `config.toml`.
pub const CREW_PERFORMANCE_STRENGTH_KEY: &str = "workflow.crew_performance_strength";
/// The `workflow.crew_performance_cap` key as written in `config.toml`.
pub const CREW_PERFORMANCE_CAP_KEY: &str = "workflow.crew_performance_cap";
/// `workflow.crew_performance_strength` when unset.
pub const DEFAULT_CREW_PERFORMANCE_STRENGTH: f64 = 3.0;
/// `workflow.crew_performance_cap` when unset.
pub const DEFAULT_CREW_PERFORMANCE_CAP: f64 = 4.0;

/// How crew draws use the performance snapshot (`workflow.crew_performance`).
/// `off` draws without it and also stops the hourly refresh from scanning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CrewPerformanceMode {
    /// No snapshot is built and no draw reads one.
    #[default]
    Off,
    /// Weight by each crew's absolute cell score.
    Absolute,
    /// Weight by each crew's cell score against its own tier mean.
    Comparative,
}

impl CrewPerformanceMode {
    /// Every admitted value, in the order errors name them.
    pub const CHOICES: [&'static str; 3] = ["off", "absolute", "comparative"];

    /// A mode by its configured name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "off" => Some(Self::Off),
            "absolute" => Some(Self::Absolute),
            "comparative" => Some(Self::Comparative),
            _ => None,
        }
    }

    /// The mode's configured name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Absolute => "absolute",
            Self::Comparative => "comparative",
        }
    }

    /// Whether the snapshot refresh should scan and write.
    #[must_use]
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// Admit `workflow.crew_performance`, `off` when unset.
pub(crate) fn admit_crew_performance(raw: Option<String>) -> Result<String, OrbitError> {
    let value = raw.as_deref().map_or("off", str::trim);
    CrewPerformanceMode::parse(value)
        .map(|mode| mode.as_str().to_string())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{CREW_PERFORMANCE_KEY} has invalid value '{value}'; expected one of: {}",
                CrewPerformanceMode::CHOICES.join(", ")
            ))
        })
}

/// Admit `workflow.crew_performance_strength`: a finite number of at least 0.
pub(crate) fn admit_crew_performance_strength(raw: Option<f64>) -> Result<f64, OrbitError> {
    let strength = raw.unwrap_or(DEFAULT_CREW_PERFORMANCE_STRENGTH);
    if strength.is_finite() && strength >= 0.0 {
        Ok(strength)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{CREW_PERFORMANCE_STRENGTH_KEY} must be a finite number of at least 0, got {strength}"
        )))
    }
}

/// Admit `workflow.crew_performance_cap`: a finite number of at least 1.
pub(crate) fn admit_crew_performance_cap(raw: Option<f64>) -> Result<f64, OrbitError> {
    let cap = raw.unwrap_or(DEFAULT_CREW_PERFORMANCE_CAP);
    if cap.is_finite() && cap >= 1.0 {
        Ok(cap)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{CREW_PERFORMANCE_CAP_KEY} must be a finite number of at least 1, got {cap}"
        )))
    }
}

/// How task crew draws weigh crew performance (`workflow.crew_performance`,
/// `workflow.crew_performance_strength`, `workflow.crew_performance_cap`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrewPerformancePolicy {
    /// Which advantage the factor reads, or `off`.
    pub mode: CrewPerformanceMode,
    /// `k`: the exponent's scale; `0` disables the factor.
    pub strength: f64,
    /// The factor is clamped to `[1 / cap, cap]`; at least 1.
    pub cap: f64,
}

impl Default for CrewPerformancePolicy {
    fn default() -> Self {
        Self {
            mode: CrewPerformanceMode::Off,
            strength: DEFAULT_CREW_PERFORMANCE_STRENGTH,
            cap: DEFAULT_CREW_PERFORMANCE_CAP,
        }
    }
}

impl CrewPerformancePolicy {
    /// Whether draws weigh performance: a mode other than `off` with a
    /// positive strength.
    #[must_use]
    pub fn applies(&self) -> bool {
        self.mode.is_on() && self.strength > 0.0
    }

    /// The advantage the mode reads from a cell: `score − 100` under
    /// `absolute`, `score − crew_mean` under `comparative`, `0` when off.
    #[must_use]
    pub fn advantage(&self, score: f64, crew_mean: f64) -> f64 {
        match self.mode {
            CrewPerformanceMode::Off => 0.0,
            CrewPerformanceMode::Absolute => score - 100.0,
            CrewPerformanceMode::Comparative => score - crew_mean,
        }
    }

    /// The weight factor for `advantage` points:
    /// `clamp(exp(strength × advantage / 100), 1 / cap, cap)`. A non-finite
    /// advantage counts as neutral.
    #[must_use]
    pub fn factor(&self, advantage: f64) -> f64 {
        let advantage = if advantage.is_finite() {
            advantage
        } else {
            0.0
        };
        (self.strength * advantage / 100.0)
            .exp()
            .clamp(1.0 / self.cap, self.cap)
    }
}

/// Admit `workflow.crew_performance_window_days`: 1..=90, default 30. The
/// scorecard scan reaches 90 days, so a wider window could never be served.
pub(crate) fn admit_crew_performance_window_days(raw: Option<u32>) -> Result<u32, OrbitError> {
    let days = raw.unwrap_or(DEFAULT_CREW_PERFORMANCE_WINDOW_DAYS);
    if (1..=90).contains(&days) {
        Ok(days)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{CREW_PERFORMANCE_WINDOW_DAYS_KEY} must be between 1 and 90, got {days}"
        )))
    }
}

/// A stable hash of the admitted type list, recorded on every snapshot so a
/// reader can tell a snapshot built from a different list. The serialization
/// is this function's own: entry order, then each entry's name, tags and
/// paths in order, NUL-separated.
#[must_use]
pub fn crew_performance_types_hash(types: &[CrewPerformanceType]) -> String {
    let mut canonical = String::new();
    for entry in types {
        canonical.push_str(&entry.name);
        canonical.push('\0');
        for tag in &entry.tags {
            canonical.push_str(tag);
            canonical.push('\0');
        }
        canonical.push('\0');
        for path in &entry.paths {
            canonical.push_str(path);
            canonical.push('\0');
        }
        canonical.push('\0');
    }
    orbit_common::security::release::sha256_hex(canonical.as_bytes())
}
/// The fallback type for a task no configured type matches.
pub const OTHER_TYPE: &str = "other";

/// One `[[workflow.crew_performance_types]]` entry: a name, and the tags or
/// context-file path globs that put a task in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CrewPerformanceType {
    /// The type's name; unique across the list and never `other`.
    #[serde(default)]
    pub name: String,
    /// A task carrying any of these tags takes this type.
    #[serde(default)]
    pub tags: Vec<String>,
    /// A task with any context file matching one of these globs takes this type.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// The type of a task under `types`: the first entry whose `tags` it carries
/// (any of) or whose `paths` globs match any of its `context_files` (any of),
/// else `other`. Context-file selectors are `file:`, `dir:` or `symbol:`
/// prefixed; a `symbol:` selector matches by its backing file
/// (`symbol:<path>#<name>:<kind>`), and a `dir:` selector also matches a glob
/// that only matches beneath it. `module:` and `command:` selectors name no
/// path and never match. This is the one classification the scorecard and the
/// later draw both use.
#[must_use]
pub fn classify_task_type<'a>(
    types: &'a [CrewPerformanceType],
    tags: &[String],
    context_files: &[String],
) -> &'a str {
    for entry in types {
        if entry
            .tags
            .iter()
            .any(|tag| tags.iter().any(|held| held == tag))
        {
            return &entry.name;
        }
        for selector in context_files {
            let Some((path, is_dir)) = selector_path(selector) else {
                continue;
            };
            let matched = entry.paths.iter().any(|pattern| {
                RelativePathGlob::new(pattern).is_ok_and(|glob| {
                    glob.matches(path) || (is_dir && glob.may_match_descendant(path))
                })
            });
            if matched {
                return &entry.name;
            }
        }
    }
    OTHER_TYPE
}

/// The path a context-file selector names, and whether it names a directory.
fn selector_path(selector: &str) -> Option<(&str, bool)> {
    if let Some(path) = selector.strip_prefix("file:") {
        Some((path, false))
    } else if let Some(path) = selector.strip_prefix("dir:") {
        Some((path, true))
    } else if let Some(remainder) = selector.strip_prefix("symbol:") {
        let (path, _) = remainder.split_once('#')?;
        if path.is_empty() {
            None
        } else {
            Some((path, false))
        }
    } else if selector.starts_with("module:") || selector.starts_with("command:") {
        None
    } else {
        Some((selector, false))
    }
}

/// Admit `workflow.crew_performance_types`: ordered entries with unique
/// non-empty names, `other` reserved, each holding at least one of `tags` or
/// `paths`, and every path a valid relative glob. An empty list is admitted
/// and classifies every task as `other`.
pub(crate) fn admit_crew_performance_types(
    raw: Option<Vec<CrewPerformanceType>>,
) -> Result<Vec<CrewPerformanceType>, OrbitError> {
    let invalid =
        |reason: String| OrbitError::InvalidInput(format!("{CREW_PERFORMANCE_TYPES_KEY} {reason}"));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut admitted = Vec::new();
    for (index, entry) in raw.unwrap_or_default().into_iter().enumerate() {
        let name = entry.name.trim().to_string();
        let label = if name.is_empty() {
            format!("entry {}", index + 1)
        } else {
            format!("entry `{name}`")
        };
        if name.is_empty() {
            return Err(invalid(format!("{label} has an empty name")));
        }
        if name == OTHER_TYPE {
            return Err(invalid(format!("{label} uses the reserved name `other`")));
        }
        if !seen.insert(name.clone()) {
            return Err(invalid(format!("{label} repeats the name `{name}`")));
        }
        let tags: Vec<String> = entry
            .tags
            .iter()
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty())
            .collect();
        let paths: Vec<String> = entry
            .paths
            .iter()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .collect();
        if tags.is_empty() && paths.is_empty() {
            return Err(invalid(format!(
                "{label} needs at least one of `tags` or `paths`"
            )));
        }
        for pattern in &paths {
            RelativePathGlob::new(pattern)
                .map_err(|error| invalid(format!("{label} has an invalid path glob: {error}")))?;
        }
        admitted.push(CrewPerformanceType { name, tags, paths });
    }
    Ok(admitted)
}

impl ConfigSnapshot {
    /// The admitted `workflow.crew_performance_types`, in configured order.
    #[must_use]
    pub fn crew_performance_types(&self) -> &[CrewPerformanceType] {
        &self.workflow_crew_performance_types
    }

    /// The admitted `workflow.crew_performance` mode.
    #[must_use]
    pub fn crew_performance(&self) -> CrewPerformanceMode {
        CrewPerformanceMode::parse(&self.workflow_crew_performance).unwrap_or_default()
    }

    /// The admitted `workflow.crew_performance_window_days`.
    #[must_use]
    pub fn crew_performance_window_days(&self) -> u32 {
        self.workflow_crew_performance_window_days
    }

    /// The admitted draw-weighting policy: mode, strength and cap.
    #[must_use]
    pub fn crew_performance_policy(&self) -> CrewPerformancePolicy {
        CrewPerformancePolicy {
            mode: self.crew_performance(),
            strength: self.workflow_crew_performance_strength,
            cap: self.workflow_crew_performance_cap,
        }
    }
}
