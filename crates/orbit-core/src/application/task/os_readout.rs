//! Host-local effective OS readouts, derived without changing task records.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{ExecutorOsLimit, HostOs, Task, TaskOsRequirement};
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;

/// A request's configured crew constraints. Reuse it across list rows so
/// executor assets are loaded once, rather than once per task.
pub struct TaskOsReadout {
    crews: BTreeMap<String, Result<String, String>>,
    limits: BTreeMap<String, ExecutorOsLimit>,
}

impl TaskOsReadout {
    /// Capture the current host's crew registry and executor OS declarations.
    pub fn new(runtime: &OrbitRuntime) -> Result<Self, OrbitError> {
        let crews = runtime
            .configured_crew_registry_projection()
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
            limits: runtime.executor_host_os_limits()?,
        })
    }

    /// Effective requirement from tags and the stored crew. An unset crew
    /// leaves automatic routing undecided; this readout does not predict draws
    /// or substitute the crew recorded by an earlier run.
    pub fn requirement(&self, task: &Task) -> Value {
        let tags = TaskOsRequirement::from_tags(&task.tags);
        let mut source = json!({"tags": task.tags.iter()
            .filter(|tag| tag.trim().to_ascii_lowercase().starts_with("os:"))
            .collect::<Vec<_>>()});
        let crew = task
            .crew
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let mut effective = tags.clone();
        if let Some(name) = crew {
            let canonical = match self.crews.get(&name.to_ascii_lowercase()) {
                Some(Ok(name)) => name,
                unresolved => {
                    source["crew"] = json!(name);
                    let reason = match unresolved {
                        Some(Err(reason)) => reason.clone(),
                        _ => format!("crew `{name}` is not configured on this host"),
                    };
                    return json!({"any_of": [], "invalid": tags.invalid,
                        "state": "unresolved", "source": source, "reason": reason,
                        "describe": format!("unresolved (crew {name}): {reason}")});
                }
            };
            if let Some(limit) = self.limits.get(canonical) {
                source["crew"] = json!(canonical);
                source["executor"] = json!(limit.executor);
                match tags.intersect_executor(canonical, &limit.executor, Some(&limit.hosts)) {
                    Ok(requirement) => effective = requirement,
                    Err(reason) => {
                        return json!({"any_of": [], "invalid": [],
                        "state": "conflict", "source": source, "reason": reason,
                        "describe": format!("no host: {reason}")});
                    }
                }
            }
        }
        if effective.is_unrestricted() {
            return Value::Null;
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
        let mut origins = source["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if effective.crew.is_some() {
            origins.push(format!("crew {}", crew.unwrap_or_default()));
        }
        let (state, describe) = if effective.invalid.is_empty() {
            (
                "restricted",
                format!("{hosts} hosts ({})", origins.join(", ")),
            )
        } else {
            (
                "invalid",
                format!("no host until retagged ({})", effective.invalid.join(", ")),
            )
        };
        json!({"any_of": effective.any_of, "invalid": effective.invalid,
            "state": state, "describe": describe, "source": source})
    }

    /// Whether a known effective requirement admits `host`. Unknown crews,
    /// invalid tags and conflicting constraints admit no host in this filter.
    pub fn admits(&self, task: &Task, host: HostOs) -> bool {
        let requirement = self.requirement(task);
        requirement.is_null()
            || (requirement["state"] == "restricted"
                && requirement["any_of"].as_array().is_some_and(|hosts| {
                    hosts
                        .iter()
                        .any(|value| value.as_str() == Some(host.as_str()))
                }))
    }

    /// Replace any tags-only base enrichment with this host's derived value.
    pub fn insert_into(&self, task: &Task, object: &mut Map<String, Value>) {
        let requirement = self.requirement(task);
        if requirement.is_null() {
            object.remove("os_requirement");
        } else {
            object.insert("os_requirement".to_string(), requirement);
        }
    }
}
