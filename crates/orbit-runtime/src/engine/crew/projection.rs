use crate::RuntimeKernel;
use orbit_common::OrbitError;
use orbit_types::identity::{Crew, HumanCrew};
use orbit_types::record::{CREW_DISCOVERY_SCHEMA_VERSION, CrewDiscoveryEntryV1, CrewDiscoveryV1};
use orbit_types::task::Task;
use serde::Serialize;
use serde_json::Value;

/// Runtime crew registry projection for dashboard/API consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfiguredCrewRegistryProjection {
    pub default_crew: Option<String>,
    pub crews: Vec<ConfiguredCrewProjection>,
    /// Operator crews, listed apart: they are never a dispatch choice.
    pub human_crews: Vec<HumanCrew>,
    /// The human crew of the person viewing: the only one configured, else
    /// the one named after the OS user. `None` when neither applies.
    pub viewer_human_crew: Option<String>,
}

/// Named crew and model string from the active runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfiguredCrewProjection {
    pub name: String,
    pub model: String,
    pub provider: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub is_default: bool,
    /// Whether dispatch may run the crew (`[crews.<name>] enabled`).
    pub enabled: bool,
}

impl ConfiguredCrewProjection {
    fn from_crew(crew: &Crew, is_default: bool) -> Self {
        Self {
            name: crew.name.clone(),
            model: crew.assignment.model.clone(),
            provider: crew.assignment.provider.clone(),
            description: crew.description.clone(),
            tags: crew.tags.clone(),
            is_default,
            enabled: crew.enabled,
        }
    }
}

/// Crew/model strings to surface on a task projection.
///
/// Decouples projection consumers from the full `Crew` type so this struct can
/// also be hydrated directly from persisted run-record fields, which carry only
/// the model string (not the provider).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCrewProjection {
    pub name: String,
    pub model: String,
}

impl ResolvedCrewProjection {
    fn from_crew(crew: Crew) -> Self {
        Self {
            name: crew.name,
            model: crew.assignment.model,
        }
    }
}

/// What a *read* surface should render for a task's crew.
///
/// Read surfaces must always render the task. Crew configuration is
/// host-local: a task authored on another machine, or before a `[crews.*]`
/// table was edited, legitimately names a crew this host cannot resolve. That
/// is a configuration gap in the reader, not a corrupt task, so it downgrades
/// to [`TaskCrewRead::Unresolved`] instead of failing the readout (ORB-10968).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskCrewRead {
    /// Neither the task nor the workspace names a crew; readers omit the fields.
    Absent,
    /// The crew resolved to a concrete name/model pair.
    Resolved(ResolvedCrewProjection),
    /// A crew is named but this host cannot resolve it. Carries the resolution
    /// error so readers can surface it as a non-fatal warning.
    Unresolved { reason: String },
}

impl TaskCrewRead {
    /// Add this read to a task projection. `crew` (the stored value) is part
    /// of the record; `resolved_crew` / `crew_model` only annotate it when the
    /// crew resolved, and `crew_unresolved` carries the reason when it did not,
    /// so an unresolvable crew never fails the readout (ORB-10968).
    pub fn insert_into(self, object: &mut serde_json::Map<String, Value>) {
        match self {
            Self::Absent => {}
            Self::Resolved(projection) => {
                object.insert("resolved_crew".to_string(), Value::String(projection.name));
                object.insert("crew_model".to_string(), Value::String(projection.model));
            }
            Self::Unresolved { reason } => {
                object.insert("crew_unresolved".to_string(), Value::String(reason));
            }
        }
    }
}

impl RuntimeKernel {
    /// Project the selected runtime's effective crew configuration for clients.
    ///
    /// This reads the already-open runtime rather than loading configuration a
    /// second time in an outer transport adapter.
    pub fn crew_discovery(
        &self,
        workspace_id: &str,
        owner_machine_id: Option<String>,
    ) -> Result<CrewDiscoveryV1, OrbitError> {
        let crews = self
            .context
            .settings()
            .crews()
            .values()
            .map(CrewDiscoveryEntryV1::from_crew)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CrewDiscoveryV1 {
            schema_version: CREW_DISCOVERY_SCHEMA_VERSION,
            workspace_id: workspace_id.to_string(),
            owner_machine_id,
            default_crew: self
                .context
                .settings()
                .default_crew()
                .map(ToOwned::to_owned),
            crews,
        })
    }

    pub fn configured_crew_registry_projection(&self) -> ConfiguredCrewRegistryProjection {
        let default_crew = self
            .context
            .settings()
            .default_crew()
            .map(ToString::to_string);
        let mut crews = self
            .context
            .settings()
            .crews()
            .values()
            .map(|crew| {
                ConfiguredCrewProjection::from_crew(
                    crew,
                    default_crew.as_deref() == Some(crew.name.as_str()),
                )
            })
            .collect::<Vec<_>>();
        crews.sort_by(|left, right| left.name.cmp(&right.name));
        let human_crews = self
            .context
            .settings()
            .human_crews()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let viewer_human_crew = match human_crews.as_slice() {
            [only] => Some(only.name.clone()),
            _ => orbit_config::os_user_human_crew_name()
                .filter(|name| human_crews.iter().any(|human| human.name == *name)),
        };
        ConfiguredCrewRegistryProjection {
            default_crew,
            crews,
            human_crews,
            viewer_human_crew,
        }
    }

    /// Resolve a crew/role-model projection for `orbit.task.show` consumers.
    ///
    /// Selection truth comes first: when the task points at a run record that
    /// persisted the resolved crew, those two strings win — they reflect what
    /// was selected for routing, even if the workspace registry has been edited
    /// since. "Who actually ran?" projections read invocation records instead.
    ///
    /// Best-effort otherwise: if neither the task nor the workspace can name a
    /// crew, `Ok(None)` so readers (CLI, MCP) can omit the fields instead of
    /// failing the entire task readout. Genuine misconfigurations (stale crew
    /// name in `task.crew` or `default_crew`) still surface as `Err`.
    pub fn resolved_crew_projection(
        &self,
        task: &Task,
    ) -> Result<Option<ResolvedCrewProjection>, OrbitError> {
        if self.task_run_is_local(task)
            && let Some(run_id) = task.job_run_id.as_deref()
            && let Some(run) = self.get_job_run_backend(run_id)?
            && let (Some(resolved_crew), Some(model)) = (run.resolved_crew, run.crew_model)
        {
            return Ok(Some(ResolvedCrewProjection {
                name: resolved_crew,
                model,
            }));
        }

        let has_resolvable_name =
            task.crew.is_some() || self.context.settings().default_crew().is_some();
        // A human crew has no agent assignment to project.
        if !has_resolvable_name || self.is_human_crew(task.crew.as_deref()) {
            return Ok(None);
        }

        self.lookup_crew_for_task(None, task.crew.as_deref())
            .map(ResolvedCrewProjection::from_crew)
            .map(Some)
    }

    /// Tolerant counterpart to [`RuntimeKernel::resolved_crew_projection`] for
    /// read surfaces (`orbit task show`, `orbit.task.*` tool responses).
    ///
    /// This is the single owner of the "a task stays readable when its crew is
    /// unavailable here" rule, so the CLI, the tool host, and MCP cannot drift
    /// apart on it. Paths that actually need a crew — `start`, dispatch — keep
    /// resolving strictly and still fail with the crew-validation error.
    pub fn task_crew_read(&self, task: &Task) -> TaskCrewRead {
        match self.resolved_crew_projection(task) {
            Ok(Some(projection)) => TaskCrewRead::Resolved(projection),
            Ok(None) => TaskCrewRead::Absent,
            Err(error) => {
                let reason = error.to_string();
                tracing::warn!(
                    task_id = %task.id,
                    stored_crew = task.crew.as_deref().unwrap_or("<unset>"),
                    reason = %reason,
                    "task crew could not be resolved on this host; rendering it unresolved",
                );
                TaskCrewRead::Unresolved { reason }
            }
        }
    }

    /// Project one crew enrichment key for a `task show` field selector.
    ///
    /// The unprojected readout carries `resolved_crew` / `crew_model` /
    /// `crew_unresolved` only in the case each describes; a projection asked
    /// for by name always answers, with `null` where that case does not apply.
    /// Selecting a key the full readout shows is therefore never an error
    /// [ORB-12113]. Any other name yields `None`, so a caller keeps its own
    /// unknown-field handling.
    pub fn task_crew_field_json(&self, task: &Task, field: &str) -> Option<Value> {
        if !matches!(field, "resolved_crew" | "crew_model" | "crew_unresolved") {
            return None;
        }

        let value = match (self.task_crew_read(task), field) {
            (TaskCrewRead::Resolved(projection), "resolved_crew") => Value::String(projection.name),
            (TaskCrewRead::Resolved(projection), "crew_model") => Value::String(projection.model),
            (TaskCrewRead::Unresolved { reason }, "crew_unresolved") => Value::String(reason),
            _ => Value::Null,
        };
        Some(value)
    }
}
