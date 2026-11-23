//! Host-local effective OS readouts, derived without changing task records.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{ExecutorOsLimit, HostOs, Task, TaskOsRequirement};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::application::host_labels::{FleetLabelHost, label_placement_wait};

#[derive(Serialize)]
pub(crate) struct TaskOsRequirementSource {
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    crew: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    executor: Option<String>,
}

pub(crate) enum EffectiveTaskOsRequirement {
    Unrestricted,
    Restricted {
        requirement: TaskOsRequirement,
        source: TaskOsRequirementSource,
        describe: String,
    },
    Invalid {
        requirement: TaskOsRequirement,
        source: TaskOsRequirementSource,
        describe: String,
    },
    Unresolved {
        invalid: Vec<String>,
        source: TaskOsRequirementSource,
        reason: String,
        describe: String,
    },
    Conflict {
        source: TaskOsRequirementSource,
        reason: String,
        describe: String,
    },
}

impl EffectiveTaskOsRequirement {
    fn into_json(self) -> Value {
        match self {
            Self::Unrestricted => Value::Null,
            Self::Restricted {
                requirement,
                source,
                describe,
            } => json!({
                "any_of": requirement.any_of, "invalid": requirement.invalid,
                "state": "restricted", "describe": describe, "source": source
            }),
            Self::Invalid {
                requirement,
                source,
                describe,
            } => json!({
                "any_of": requirement.any_of, "invalid": requirement.invalid,
                "state": "invalid", "describe": describe, "source": source
            }),
            Self::Unresolved {
                invalid,
                source,
                reason,
                describe,
            } => json!({
                "any_of": [], "invalid": invalid, "state": "unresolved",
                "source": source, "reason": reason, "describe": describe
            }),
            Self::Conflict {
                source,
                reason,
                describe,
            } => json!({
                "any_of": [], "invalid": [], "state": "conflict",
                "source": source, "reason": reason, "describe": describe
            }),
        }
    }
}

/// A request's configured crew constraints and the fleet's capability
/// labels. Reuse it across list rows so executor assets and the executor
/// census are read once, rather than once per task.
pub struct TaskOsReadout {
    crews: BTreeMap<String, Result<String, String>>,
    /// Human crews (`kind = "human"`): operator work with no executor, so no
    /// executor OS constraint applies to a task that names one.
    human_crews: BTreeSet<String>,
    limits: BTreeMap<String, ExecutorOsLimit>,
    fleet: Vec<FleetLabelHost>,
    tag_label_requirements: orbit_types::task::TagLabelRequirements,
}

impl TaskOsReadout {
    /// Capture the current host's crew registry and executor OS declarations.
    pub fn new(runtime: &OrbitRuntime) -> Result<Self, OrbitError> {
        let registry = runtime.configured_crew_registry_projection();
        let human_crews = registry
            .human_crews
            .iter()
            .map(|crew| crew.name.clone())
            .collect();
        let crews = registry
            .crews
            .into_iter()
            .map(|crew| {
                let resolved = runtime
                    .resolve_crew_for_task(None, Some(&crew.name))
                    .map(|crew| crew.name)
                    .map_err(|error| error.to_string());
                (crew.name.to_ascii_lowercase(), resolved)
            })
            .collect();
        Ok(Self {
            crews,
            human_crews,
            limits: runtime.executor_host_os_limits()?,
            fleet: runtime.fleet_label_hosts(),
            tag_label_requirements: runtime.tag_label_requirements().clone(),
        })
    }

    /// Effective requirement from tags and the stored crew. An unset crew
    /// leaves automatic routing undecided; this readout does not predict draws
    /// or substitute the crew recorded by an earlier run.
    pub fn requirement(&self, task: &Task) -> Value {
        self.effective_requirement(task).into_json()
    }

    fn effective_requirement(&self, task: &Task) -> EffectiveTaskOsRequirement {
        let tags = TaskOsRequirement::from_tags(&task.tags);
        let mut source = TaskOsRequirementSource {
            tags: task
                .tags
                .iter()
                .filter(|tag| tag.trim().to_ascii_lowercase().starts_with("os:"))
                .cloned()
                .collect(),
            crew: None,
            executor: None,
        };
        let crew = task
            .crew
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let mut effective = tags.clone();
        // A human crew has no executor, so only the task's own OS tags bind it.
        let agent_crew = crew.filter(|name| !self.human_crews.contains(*name));
        if let Some(name) = agent_crew {
            let base = orbit_types::identity::crew_reference_base(name).to_ascii_lowercase();
            let canonical = match self.crews.get(&base) {
                Some(Ok(name)) => name,
                unresolved => {
                    source.crew = Some(name.to_string());
                    let reason = match unresolved {
                        Some(Err(reason)) => reason.clone(),
                        _ => format!("crew `{name}` is not configured on this host"),
                    };
                    let describe = format!("unresolved (crew {name}): {reason}");
                    return EffectiveTaskOsRequirement::Unresolved {
                        invalid: tags.invalid,
                        source,
                        reason,
                        describe,
                    };
                }
            };
            if let Some(limit) = self.limits.get(canonical) {
                source.crew = Some(canonical.clone());
                source.executor = Some(limit.executor.clone());
                match tags.intersect_executor(canonical, &limit.executor, Some(&limit.hosts)) {
                    Ok(requirement) => effective = requirement,
                    Err(reason) => {
                        let describe = format!("no host: {reason}");
                        return EffectiveTaskOsRequirement::Conflict {
                            source,
                            reason,
                            describe,
                        };
                    }
                }
            }
        }
        if effective.is_unrestricted() {
            return EffectiveTaskOsRequirement::Unrestricted;
        }
        if !effective.invalid.is_empty() {
            let describe = format!("no host until retagged ({})", effective.invalid.join(", "));
            return EffectiveTaskOsRequirement::Invalid {
                requirement: effective,
                source,
                describe,
            };
        }
        let hosts = effective
            .any_of
            .iter()
            .map(|os| match os {
                HostOs::Linux => "Linux",
                HostOs::Macos => "macOS",
                HostOs::Windows => "Windows",
            })
            .collect::<Vec<_>>()
            .join(" or ");
        let mut origins = source.tags.clone();
        if effective.crew.is_some() {
            origins.push(format!("crew {}", crew.unwrap_or_default()));
        }
        let describe = format!("{hosts} hosts ({})", origins.join(", "));
        EffectiveTaskOsRequirement::Restricted {
            requirement: effective,
            source,
            describe,
        }
    }

    /// Whether a known effective requirement admits `host`. Unknown crews,
    /// invalid tags and conflicting constraints admit no host in this filter.
    pub fn admits(&self, task: &Task, host: HostOs) -> bool {
        match self.effective_requirement(task) {
            EffectiveTaskOsRequirement::Unrestricted => true,
            EffectiveTaskOsRequirement::Restricted { requirement, .. } => {
                requirement.any_of.contains(&host)
            }
            EffectiveTaskOsRequirement::Invalid { .. }
            | EffectiveTaskOsRequirement::Unresolved { .. }
            | EffectiveTaskOsRequirement::Conflict { .. } => false,
        }
    }

    /// Why `task` waits for a host carrying its `requires_labels`, while no
    /// live host carries them all, or `None`.
    pub fn placement_wait(&self, task: &Task) -> Option<String> {
        let required = self
            .tag_label_requirements
            .required_labels(&task.tags, &task.requires_labels);
        label_placement_wait(task, &required, &self.fleet)
    }

    /// Replace any tags-only base enrichment with this host's derived value,
    /// and name the label placement wait when there is one.
    pub fn insert_into(&self, task: &Task, object: &mut Map<String, Value>) {
        match self.effective_requirement(task) {
            EffectiveTaskOsRequirement::Unrestricted => {
                object.remove("os_requirement");
            }
            requirement => {
                object.insert("os_requirement".to_string(), requirement.into_json());
            }
        }
        match self.placement_wait(task) {
            Some(wait) => {
                object.insert("placement_wait".to_string(), Value::String(wait));
            }
            None => {
                object.remove("placement_wait");
            }
        }
    }
}
