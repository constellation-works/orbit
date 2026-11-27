//! The one crew name the task detail's **Handled by** row shows for a hand.
//!
//! A hand is shown as a bare crew name (`opus`, `muse-spark`, `sol`): never
//! an `@effort`, a provider or a full model id. A name comes only from a
//! record of that occurrence:
//!
//! - a recorded crew reference, minus its `@effort`;
//! - else a recorded model id that exactly one configured crew runs.
//!
//! Anything else — a provider-level label such as `claude`, a model several
//! crews share, the task's current assignment — names no crew, and the row
//! shows the action alone rather than a guess.

use orbit_types::identity::{all_agent_families, crew_reference_base};
use serde_json::{Map, Value};

use crate::OrbitRuntime;
use crate::Task;
use crate::runtime::engine::ConfiguredCrewRegistryProjection;

/// The configured crews a recorded model id is matched against.
pub(super) struct CrewNames {
    /// `(name, model)` of every configured agent crew.
    agents: Vec<(String, String)>,
    humans: Vec<String>,
}

impl CrewNames {
    pub(super) fn load(runtime: &OrbitRuntime) -> Self {
        Self::from_registry(&runtime.configured_crew_registry_projection())
    }

    fn from_registry(registry: &ConfiguredCrewRegistryProjection) -> Self {
        Self {
            agents: registry
                .crews
                .iter()
                .map(|crew| (crew.name.clone(), crew.model.clone()))
                .collect(),
            humans: registry
                .human_crews
                .iter()
                .map(|crew| crew.name.clone())
                .collect(),
        }
    }

    /// The crew a hand's own record names: its recorded crew reference
    /// without the effort, else the one crew that runs its recorded model.
    pub(super) fn recorded(&self, crew: Option<&str>, model: Option<&str>) -> Option<String> {
        crew.map(crew_reference_base)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| model.and_then(|model| self.by_model(model)))
    }

    /// The crew a stored attribution label names: a registered crew
    /// reference, else the one crew that runs the label as its model. A
    /// provider-level label (`claude`) is never a crew.
    fn by_label(&self, label: &str) -> Option<String> {
        let base = crew_reference_base(label);
        if base.is_empty() || all_agent_families().contains(&base.to_ascii_lowercase().as_str()) {
            return None;
        }
        let registered = self.agents.iter().any(|(name, _)| name == base)
            || self.humans.iter().any(|name| name == base);
        if registered {
            Some(base.to_string())
        } else {
            self.by_model(label)
        }
    }

    /// The crew running `model`, when exactly one configured crew does.
    fn by_model(&self, model: &str) -> Option<String> {
        let model = model.trim();
        let mut matches = self
            .agents
            .iter()
            .filter(|(_, crew_model)| !model.is_empty() && crew_model == model);
        let (name, _) = matches.next()?;
        matches.next().is_none().then(|| name.clone())
    }
}

impl OrbitRuntime {
    /// The crew each stored attribution field of `task` names, as the task
    /// detail's `recorded_crews`: `created_by`, `planned_by`,
    /// `implemented_by` and `orchestrator`, each present only when the
    /// stored record establishes a crew. The task's current `crew` is never
    /// consulted, so a reassignment cannot rewrite who did earlier work.
    pub fn task_recorded_crews(&self, task: &Task) -> Map<String, Value> {
        let names = CrewNames::load(self);
        let mut crews = Map::new();
        let mut put = |field: &str, crew: Option<String>| {
            if let Some(crew) = crew {
                crews.insert(field.to_string(), Value::String(crew));
            }
        };
        let label = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .and_then(|value| names.by_label(value))
        };
        put(
            "created_by",
            label(&task.created_by_model).or_else(|| label(&task.created_by)),
        );
        put("planned_by", label(&task.planned_by));
        put("implemented_by", label(&task.implemented_by));
        // The orchestrator is a validated crew name when it is recorded.
        put(
            "orchestrator",
            task.orchestrator
                .as_deref()
                .map(crew_reference_base)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        );
        crews
    }
}
