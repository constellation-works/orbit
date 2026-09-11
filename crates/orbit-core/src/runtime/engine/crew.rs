use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::identity::{
    Crew, CrewAssignment, all_agent_families, infer_agent_family_from_model, resolve_crew,
};
use orbit_types::record::{CREW_DISCOVERY_SCHEMA_VERSION, CrewDiscoveryEntryV1, CrewDiscoveryV1};
use orbit_types::task::{Task, is_valid_orb_task_id};
use orbit_types::workflow::activity_job::ProviderSource;
use serde::Serialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::run_input::{non_empty, singular_task_id_from_input};

/// Select the crew *name* to dispatch by the Constellation provider-resolution
/// precedence (ORB-10091, contract §3): explicit > task_config >
/// workspace_default > environment_default > system_default. Empty /
/// whitespace-only values are skipped (not a selection). Returns the chosen
/// name and the tier it came from; `None` only when no tier supplies a value.
/// Pure and table-tested so the precedence cannot drift from the shared
/// contract or from the `Provider::resolve` surface it mirrors.
pub(crate) fn select_crew_name<'a>(
    explicit: Option<&'a str>,
    task_config: Option<&'a str>,
    workspace_default: Option<&'a str>,
    environment_default: Option<&'a str>,
    system_default: Option<&'a str>,
) -> Option<(&'a str, ProviderSource)> {
    [
        (explicit, ProviderSource::Explicit),
        (task_config, ProviderSource::TaskConfig),
        (workspace_default, ProviderSource::WorkspaceDefault),
        (environment_default, ProviderSource::EnvironmentDefault),
        (system_default, ProviderSource::SystemDefault),
    ]
    .into_iter()
    .find_map(|(value, source)| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| (value, source))
    })
}

/// Run-input key carrying a shipment's crew allowlist [ORB-11242].
///
/// The drain persists it on the run it submits and every pipeline it admits
/// forwards it, so the *run* input is the authority for the whole window. An
/// activity input never widens it.
pub(crate) const ALLOWED_CREWS_INPUT_KEY: &str = "allowed_crews";

/// The configured crews one submitted shipment may launch a provider as
/// [ORB-11242].
///
/// Opt-in and run-scoped: an absent or empty list is no restriction at all, so
/// every pre-existing caller keeps today's behavior. Nothing here rewrites
/// workspace configuration, remaps a task's crew, or cancels work another
/// invocation already started — it only refuses to *start* one.
///
/// Membership is checked twice over, because a crew name is a wrapper and the
/// provider process is what actually costs tokens:
///
/// - by canonical registry name, the ordinary case; and
/// - by effective configured identity — the `(provider, model)` the crew
///   resolves to — so a differently-named alias of a permitted crew (for
///   instance the `system` entry that mirrors `default_crew`) is permitted,
///   while a permitted-looking wrapper that resolves to a *different* provider
///   or model is not. Naming a run wrapper is not by itself provider usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrewAllowlist {
    names: BTreeSet<String>,
    identities: BTreeSet<(String, String)>,
}

impl CrewAllowlist {
    /// Whether `crew` — already resolved through the registry — may run.
    pub(crate) fn permits(&self, crew: &Crew) -> bool {
        self.names.contains(crew.name.trim())
            || self.identities.contains(&crew_identity(&crew.assignment))
    }

    /// The permitted names, for operator-facing messages.
    pub(crate) fn names(&self) -> Vec<&str> {
        self.names.iter().map(String::as_str).collect()
    }

    fn describe(&self) -> String {
        self.names().join(", ")
    }
}

/// Refuse a resolved crew the run's window does not permit [ORB-11242].
///
/// `origin` names where the crew came from (the task, an explicit activity
/// crew, `workflow.system_crew`) so the failure is actionable without reading
/// the pipeline. There is deliberately no fallback: a restricted drain that
/// quietly re-routed to a permitted crew would spend a budget the operator
/// scoped, against work they did not choose.
pub(crate) fn enforce_crew_allowlist(
    allowlist: Option<&CrewAllowlist>,
    crew: &Crew,
    origin: &str,
) -> Result<(), OrbitError> {
    let Some(allowlist) = allowlist else {
        return Ok(());
    };
    if allowlist.permits(crew) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "crew `{}` ({}/{}) selected by {origin} is not permitted by this run's crew allowlist [{}]; \
         reassign the task or submit a drain that permits it",
        crew.name,
        crew.assignment.provider,
        crew.assignment.model,
        allowlist.describe(),
    )))
}

/// The effective configured identity a crew dispatches as. Provider casing is
/// normalized because it is matched against typed provider enums downstream;
/// the model string is compared verbatim.
fn crew_identity(assignment: &CrewAssignment) -> (String, String) {
    (
        assignment.provider.trim().to_ascii_lowercase(),
        assignment.model.trim().to_string(),
    )
}

/// Read the run-scoped allowlist *names* off a run/activity input.
///
/// Tolerates the shapes a job template produces: an absent key renders as the
/// empty string rather than JSON `null`, and a declared `allowed_crews: []`
/// default renders as an empty array. Both mean "unrestricted".
pub(crate) fn crew_allowlist_names_from_input(input: &Value) -> Result<Vec<String>, OrbitError> {
    let Some(raw) = input.get(ALLOWED_CREWS_INPUT_KEY) else {
        return Ok(Vec::new());
    };
    match raw {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(non_empty(text)
            .map(|name| vec![name.to_string()])
            .unwrap_or_default()),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .and_then(non_empty)
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "`{ALLOWED_CREWS_INPUT_KEY}` must list non-empty crew names, got {item}"
                        ))
                    })
            })
            .collect(),
        other => Err(OrbitError::InvalidInput(format!(
            "`{ALLOWED_CREWS_INPUT_KEY}` must be a list of crew names, got {other}"
        ))),
    }
}

/// Runtime crew registry projection for dashboard/API consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfiguredCrewRegistryProjection {
    pub default_crew: Option<String>,
    pub crews: Vec<ConfiguredCrewProjection>,
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

impl OrbitRuntime {
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
        ConfiguredCrewRegistryProjection {
            default_crew,
            crews,
        }
    }

    pub fn validate_crew_name(&self, crew: Option<&str>) -> Result<(), OrbitError> {
        self.canonical_crew_name(crew).map(|_| ())
    }

    /// Build the run-scoped allowlist for `names` [ORB-11242].
    ///
    /// Validation happens here, before any run record exists, so a typo or a
    /// crew this host does not configure fails the submission instead of
    /// silently narrowing what a live drain will admit. An empty selection is
    /// `None` — the option was omitted, and behavior is unchanged.
    pub(crate) fn crew_allowlist(
        &self,
        names: &[String],
    ) -> Result<Option<CrewAllowlist>, OrbitError> {
        let mut allowed_names = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for name in names {
            let crew = resolve_crew(name.trim(), self.context.settings().crews())?;
            identities.insert(crew_identity(&crew.assignment));
            allowed_names.insert(crew.name);
        }
        Ok((!allowed_names.is_empty()).then_some(CrewAllowlist {
            names: allowed_names,
            identities,
        }))
    }

    /// The allowlist carried by a run/activity input, or `None` when the run
    /// placed no restriction on its window.
    pub(crate) fn crew_allowlist_from_input(
        &self,
        input: &Value,
    ) -> Result<Option<CrewAllowlist>, OrbitError> {
        self.crew_allowlist(&crew_allowlist_names_from_input(input)?)
    }

    /// The crew a backlog task would actually dispatch as, or the reason it
    /// cannot be determined on this host. Used by auto-drain admission so an
    /// excluded task is reported against its *effective* crew rather than the
    /// raw string on the task.
    pub(crate) fn effective_task_crew(&self, task: &Task) -> Result<Crew, OrbitError> {
        self.resolve_crew_for_task(None, task.crew.as_deref())
    }

    /// Resolve a user-supplied crew name to the exact alias stored in the
    /// active named-crew registry. Blank optional values remain unset.
    pub(crate) fn canonical_crew_name(
        &self,
        crew: Option<&str>,
    ) -> Result<Option<String>, OrbitError> {
        let Some(crew) = crew.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        resolve_crew(crew, self.context.settings().crews())
            .map(|crew| Some(crew.name))
            .map_err(Into::into)
    }

    pub fn resolve_crew_for_task(
        &self,
        cli_override: Option<&str>,
        task_crew: Option<&str>,
    ) -> Result<Crew, OrbitError> {
        // Runtime precedence is explicit > task_config > the default projected
        // by RuntimeConfig. That projection has already resolved workspace >
        // environment > system-default precedence, so lower tiers are not
        // re-read here and cannot drift during a run.
        let selection = select_crew_name(
            cli_override,
            task_crew,
            self.context.settings().default_crew(),
            None,
            None,
        );

        let Some((selected, _source)) = selection else {
            return Err(OrbitError::InvalidInput(
                "no crew selected; set [workflow].default_crew, task.crew, or pass crew"
                    .to_string(),
            ));
        };
        resolve_crew(selected, self.context.settings().crews()).map_err(Into::into)
    }

    pub(crate) fn resolve_crew_for_run_input(&self, input: &Value) -> Result<Crew, OrbitError> {
        let cli_override = input
            .get("crew")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let task_crew = self.task_crew_from_run_input(input)?;
        self.resolve_crew_for_task(cli_override, task_crew.as_deref())
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
        if let Some(run_id) = task.job_run_id.as_deref()
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
        if !has_resolvable_name {
            return Ok(None);
        }

        self.resolve_crew_for_task(None, task.crew.as_deref())
            .map(ResolvedCrewProjection::from_crew)
            .map(Some)
    }

    /// Tolerant counterpart to [`OrbitRuntime::resolved_crew_projection`] for
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

    pub(crate) fn record_run_crew_from_input(
        &self,
        run_id: &str,
        input: &Value,
    ) -> Result<Crew, OrbitError> {
        let crew = self.resolve_crew_for_run_input(input)?;
        tracing::info!(
            run_id,
            resolved_crew = %crew.name,
            crew_model = %crew.assignment.model,
            "crew resolved for run",
        );
        self.stores().jobs().record_job_run_crew(run_id, &crew)?;
        Ok(crew)
    }

    pub(crate) fn implementer_identity_for_activity_input(
        &self,
        input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        let task = self.task_id_from_run_input(input)?;
        let input_run_id = input
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let task_run_id = task.as_ref().and_then(|task| task.job_run_id.as_deref());
        let Some(run_id) = input_run_id.or(task_run_id) else {
            return Ok((None, None));
        };
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok((None, None));
        };

        if let Some(crew_name) = run
            .resolved_crew
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && let Ok(crew) = resolve_crew(crew_name, self.context.settings().crews())
            && let Some(family) = family_from_assignment(&crew.assignment)
        {
            return Ok((Some(family.clone()), Some(family)));
        }

        if let Some(family) = run
            .crew_model
            .as_deref()
            .and_then(infer_agent_family_from_model)
        {
            return Ok((Some(family.clone()), Some(family)));
        }

        Ok((None, None))
    }

    pub(crate) fn resolve_and_log_crew_for_task_start(
        &self,
        task_id: &str,
        crew_override: Option<&str>,
        task_crew: Option<&str>,
    ) -> Result<Crew, OrbitError> {
        let crew = self.resolve_crew_for_task(crew_override, task_crew)?;
        tracing::info!(
            task_id,
            resolved_crew = %crew.name,
            crew_model = %crew.assignment.model,
            "crew resolved for task start",
        );
        Ok(crew)
    }

    /// Unanimous `task.crew` across every stored task named by the run/activity
    /// input. Missing fixture ids are skipped so fake implementer ids do not
    /// fail resolution. Distinct crews — including a mix of set and unset —
    /// fail closed instead of inheriting `workflow.default_crew`.
    fn task_crew_from_run_input(&self, input: &Value) -> Result<Option<String>, OrbitError> {
        let mut agreed: Option<Option<String>> = None;
        for task_id in task_ids_for_crew_resolution(input) {
            if !is_valid_orb_task_id(&task_id) {
                continue;
            }
            let Some(task) = self.stores().tasks().get_task(&task_id)? else {
                continue;
            };
            let crew = normalized_task_crew(task.crew.as_deref());
            match &agreed {
                None => agreed = Some(crew),
                Some(existing) if existing == &crew => {}
                Some(existing) => {
                    return Err(OrbitError::InvalidInput(mixed_bundle_crew_error(
                        existing.as_deref(),
                        crew.as_deref(),
                    )));
                }
            }
        }
        Ok(agreed.flatten())
    }

    fn task_id_from_run_input(&self, input: &Value) -> Result<Option<Task>, OrbitError> {
        if let Some(task_id) = singular_task_id_from_input(input)
            && is_valid_orb_task_id(task_id)
            && let Some(task) = self.stores().tasks().get_task(task_id)?
        {
            return Ok(Some(task));
        }

        for key in ["task_id", "taskId", "id"] {
            let Some(task_id) = input.get(key).and_then(Value::as_str).and_then(non_empty) else {
                continue;
            };
            if is_valid_orb_task_id(task_id)
                && let Some(task) = self.stores().tasks().get_task(task_id)?
            {
                return Ok(Some(task));
            }
        }
        Ok(None)
    }
}

/// Collect `task_id` / `task.id` / `task_ids` entries. Generic `id` is left to
/// [`OrbitRuntime::task_id_from_run_input`] so a non-task `id` cannot pollute
/// mixed-crew detection.
fn task_ids_for_crew_resolution(input: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let mut push = |raw: Option<&str>| {
        if let Some(id) = raw.and_then(non_empty)
            && !ids.iter().any(|existing| existing == id)
        {
            ids.push(id.to_string());
        }
    };
    push(input.get("task_id").and_then(Value::as_str));
    push(
        input
            .get("task")
            .and_then(|task| task.get("id"))
            .and_then(Value::as_str),
    );
    if let Some(items) = input.get("task_ids").and_then(Value::as_array) {
        for item in items {
            push(item.as_str());
        }
    }
    ids
}

fn normalized_task_crew(crew: Option<&str>) -> Option<String> {
    crew.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn mixed_bundle_crew_error(left: Option<&str>, right: Option<&str>) -> String {
    format!(
        "task bundle mixes crews {} and {}; split the bundle or assign one crew instead of inheriting workflow.default_crew",
        format_bundle_crew(left),
        format_bundle_crew(right),
    )
}

fn format_bundle_crew(crew: Option<&str>) -> String {
    match crew {
        Some(name) => format!("`{name}`"),
        None => "the workspace default (no task.crew)".to_string(),
    }
}

fn family_from_assignment(assignment: &CrewAssignment) -> Option<String> {
    let provider = assignment.provider.trim().to_ascii_lowercase();
    if all_agent_families()
        .iter()
        .any(|family| *family == provider)
    {
        return Some(provider);
    }

    infer_agent_family_from_model(&assignment.model)
}
