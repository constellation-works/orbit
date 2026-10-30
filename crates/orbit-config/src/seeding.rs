//! `seeding.routines` and `seeding.auto_tasks`: which of Orbit's shipped
//! default routines and auto-tasks a workspace carries (design
//! `docs/design/distributions/1_scope.md`).
//!
//! Each key takes `"all"` (the default), `"none"`, or a list of shipped
//! default names. Admission refuses a name this Orbit has never shipped, so a
//! misspelling fails at config load instead of silently seeding nothing. A
//! name a later release retired still admits: the workspace that listed it
//! stays loadable, and the retired default is simply no longer seeded.
//!
//! The shipped names live here because this crate admits the keys and cannot
//! see the embedded assets; `orbit-core` asserts at compile time that its
//! asset tables carry exactly these names.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize, Serializer};

use crate::ConfigRoots;
use crate::registry::read_optional;

/// The `seeding.routines` key as written in `config.toml`.
pub const SEEDING_ROUTINES_KEY: &str = "seeding.routines";
/// The `seeding.auto_tasks` key as written in `config.toml`.
pub const SEEDING_AUTO_TASKS_KEY: &str = "seeding.auto_tasks";

/// File stems of the default routines this Orbit ships, in seeding order.
pub const SHIPPED_DEFAULT_ROUTINES: &[&str] = &[
    "ci_failure_sweep",
    "dependabot_alert_sweep",
    "task_pilot",
    "finding_verification",
    "pilot_warning_triage",
    "blocked_task_triage",
    "worktree_gc",
    "task_publication",
    "store_gc",
    "crew_performance_refresh",
];

/// Default routines a prior release shipped and this one no longer seeds.
pub const RETIRED_DEFAULT_ROUTINES: &[&str] = &["auto_task_scheduler", "task_triage", "ship_sweep"];

/// Names of the default auto-tasks this Orbit ships, in seeding order.
pub const SHIPPED_DEFAULT_AUTO_TASKS: &[&str] = &[
    "agent-guidance-audit",
    "auto-task-yield-review",
    "architecture-audit",
    "backlog-hygiene",
    "ci-budget-watch",
    "code-health-sweep",
    "delivery-code-review",
    "dependency-freshness",
    "code-review",
    "comment-hygiene",
    "design-sync",
    "doc-duties",
    "escape-to-guard",
    "flaky-test-hunt",
    "friction-curation",
    "full-code-review",
    "path-risk-review",
    "qa-full-sweep",
    "qa-sweep",
    "run-failure-patterns",
    "security-review",
    "structure-sweep",
    "suppression-ratchet",
    "workflow-health-audit",
];

/// Default auto-tasks a prior release shipped and this one no longer seeds.
pub const RETIRED_DEFAULT_AUTO_TASKS: &[&str] = &["blocked-task-triage"];

/// Which shipped defaults of one kind a workspace carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SeedingPolicy {
    /// Every shipped default, including ones a later release adds.
    #[default]
    All,
    /// No shipped default.
    None,
    /// Exactly the named shipped defaults; a later release's additions are
    /// not seeded.
    Only(BTreeSet<String>),
}

impl SeedingPolicy {
    /// Whether the shipped default `name` is seeded under this policy.
    #[must_use]
    pub fn includes(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Only(names) => names.contains(name),
        }
    }

    /// Whether this is the default policy, which seeds what Orbit always has.
    #[must_use]
    pub fn is_all(&self) -> bool {
        *self == Self::All
    }

    /// The policy as `config.toml` and JSON surfaces write it: `"all"`,
    /// `"none"`, or the sorted list of names.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::All => serde_json::Value::from("all"),
            Self::None => serde_json::Value::from("none"),
            Self::Only(names) => serde_json::Value::from(names.iter().cloned().collect::<Vec<_>>()),
        }
    }
}

impl Serialize for SeedingPolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

/// A seeding key as written: a keyword or a list of names.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawSeedingPolicy {
    Keyword(String),
    Names(Vec<String>),
}

/// The two seeding policies one workspace resolves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WorkspaceSeedingPolicies {
    /// `seeding.routines`.
    pub routines: SeedingPolicy,
    /// `seeding.auto_tasks`.
    pub auto_tasks: SeedingPolicy,
}

impl WorkspaceSeedingPolicies {
    /// Whether both policies are the default `all`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.routines.is_all() && self.auto_tasks.is_all()
    }
}

/// Admit `seeding.routines`.
pub(crate) fn admit_routine_seeding(
    raw: Option<RawSeedingPolicy>,
) -> Result<SeedingPolicy, OrbitError> {
    admit_seeding_policy(
        raw,
        SEEDING_ROUTINES_KEY,
        "routine",
        SHIPPED_DEFAULT_ROUTINES,
        RETIRED_DEFAULT_ROUTINES,
    )
}

/// Admit `seeding.auto_tasks`.
pub(crate) fn admit_auto_task_seeding(
    raw: Option<RawSeedingPolicy>,
) -> Result<SeedingPolicy, OrbitError> {
    admit_seeding_policy(
        raw,
        SEEDING_AUTO_TASKS_KEY,
        "auto-task",
        SHIPPED_DEFAULT_AUTO_TASKS,
        RETIRED_DEFAULT_AUTO_TASKS,
    )
}

fn admit_seeding_policy(
    raw: Option<RawSeedingPolicy>,
    key: &str,
    noun: &str,
    shipped: &[&str],
    retired: &[&str],
) -> Result<SeedingPolicy, OrbitError> {
    match raw {
        None => Ok(SeedingPolicy::All),
        Some(RawSeedingPolicy::Keyword(keyword)) => match keyword.trim() {
            "all" => Ok(SeedingPolicy::All),
            "none" => Ok(SeedingPolicy::None),
            other => Err(OrbitError::InvalidInput(format!(
                "{key} must be \"all\", \"none\" or a list of shipped default {noun} names, \
                 not '{other}'; a list is a TOML array, e.g. '[\"{}\"]'",
                shipped.first().copied().unwrap_or_default()
            ))),
        },
        Some(RawSeedingPolicy::Names(names)) => {
            let mut admitted = BTreeSet::new();
            for name in names {
                let name = name.trim().to_string();
                if !shipped.contains(&name.as_str()) && !retired.contains(&name.as_str()) {
                    return Err(OrbitError::InvalidInput(format!(
                        "{key} names '{name}', which is not a default {noun} Orbit ships; \
                         choose from {}",
                        shipped.join(", ")
                    )));
                }
                admitted.insert(name);
            }
            Ok(SeedingPolicy::Only(admitted))
        }
    }
}

/// Resolve both seeding policies for the workspace whose `.orbit` directory is
/// `roots.workspace()`, layering it over the global file as every key does.
///
/// Only the two seeding keys are read and admitted, so a fault elsewhere in
/// either file does not stop the convergence paths that seed defaults; a full
/// config load still reports it.
pub fn load_workspace_seeding_policies(
    roots: &ConfigRoots,
) -> Result<WorkspaceSeedingPolicies, OrbitError> {
    let mut layers = vec![roots.global().join("config.toml")];
    if roots.has_workspace_layer() {
        layers.push(roots.workspace().join("config.toml"));
    }
    let mut routines: Option<RawSeedingPolicy> = None;
    let mut auto_tasks: Option<RawSeedingPolicy> = None;
    let mut sources = [None, None];
    for path in &layers {
        let Some(raw) = crate::store::read_config_file(path)? else {
            continue;
        };
        let document: toml::Value = toml::from_str(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': {error}",
                orbit_common::security::redaction::redact_home_dir(&path.display().to_string())
            ))
        })?;
        if let Some(value) = read_optional(&document, SEEDING_ROUTINES_KEY, path)? {
            routines = Some(value);
            sources[0] = Some(path.clone());
        }
        if let Some(value) = read_optional(&document, SEEDING_AUTO_TASKS_KEY, path)? {
            auto_tasks = Some(value);
            sources[1] = Some(path.clone());
        }
    }
    let at = |result: Result<SeedingPolicy, OrbitError>, source: &Option<std::path::PathBuf>| {
        result.map_err(|error| match (error, source) {
            (OrbitError::InvalidInput(message), Some(path)) => {
                OrbitError::InvalidInput(format!("{}: {message}", display_path(path)))
            }
            (other, _) => other,
        })
    };
    Ok(WorkspaceSeedingPolicies {
        routines: at(admit_routine_seeding(routines), &sources[0])?,
        auto_tasks: at(admit_auto_task_seeding(auto_tasks), &sources[1])?,
    })
}

fn display_path(path: &Path) -> String {
    orbit_common::security::redaction::redact_home_dir(&path.display().to_string())
}
