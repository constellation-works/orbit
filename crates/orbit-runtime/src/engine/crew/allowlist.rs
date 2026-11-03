use crate::RuntimeKernel;
use crate::run_input::non_empty;
use orbit_common::OrbitError;
use orbit_types::identity::{Crew, resolve_crew};
use serde_json::Value;
use std::collections::BTreeSet;

/// Run-input key carrying a shipment's crew allowlist [ORB-11242].
///
/// The drain persists it on the run it submits and every pipeline it admits
/// forwards it, so the *run* input is the authority for the whole window. An
/// activity input never widens it.
pub const ALLOWED_CREWS_INPUT_KEY: &str = "allowed_crews";

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
///   resolves to, and [ORB-17288] the provider account it runs on — so a
///   differently-named alias of a permitted crew (for instance the `system`
///   entry that mirrors `default_crew`) is permitted, while a
///   permitted-looking wrapper that resolves to a *different* provider, model
///   or account is not. A crew that sets `env` is its own account, so
///   permitting the subscription's crew never permits a credits crew on the
///   same model. Naming a run wrapper is not by itself provider usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewAllowlist {
    names: BTreeSet<String>,
    identities: BTreeSet<CrewIdentity>,
}

/// A crew's provider, model and provider account.
type CrewIdentity = (String, String, Option<String>);

impl CrewAllowlist {
    /// Whether `crew` — already resolved through the registry — may run.
    pub fn permits(&self, crew: &Crew) -> bool {
        self.names.contains(crew.name.trim()) || self.identities.contains(&crew_identity(crew))
    }

    /// The permitted names, for operator-facing messages.
    pub fn names(&self) -> Vec<&str> {
        self.names.iter().map(String::as_str).collect()
    }

    fn describe(&self) -> String {
        self.names().join(", ")
    }
}

/// Refuse a resolved crew the run's window does not permit [ORB-11242].
///
/// `origin` names where the crew came from (the task, an explicit activity
/// crew, a complexity pool) so the failure is actionable without reading
/// the pipeline. There is deliberately no fallback: a restricted drain that
/// quietly re-routed to a permitted crew would spend a budget the operator
/// scoped, against work they did not choose.
pub fn enforce_crew_allowlist(
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
fn crew_identity(crew: &Crew) -> CrewIdentity {
    (
        crew.assignment.provider.trim().to_ascii_lowercase(),
        crew.assignment.model.trim().to_string(),
        crew.account().map(ToOwned::to_owned),
    )
}

/// Read the run-scoped allowlist *names* off a run/activity input.
///
/// Tolerates the shapes a job template produces: an absent key renders as the
/// empty string rather than JSON `null`, and a declared `allowed_crews: []`
/// default renders as an empty array. Both mean "unrestricted".
pub fn crew_allowlist_names_from_input(input: &Value) -> Result<Vec<String>, OrbitError> {
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

impl RuntimeKernel {
    /// Build the run-scoped allowlist for `names` [ORB-11242].
    ///
    /// Validation happens here, before any run record exists, so a typo or a
    /// crew this host does not configure fails the submission instead of
    /// silently narrowing what a live drain will admit. An empty selection is
    /// `None` — the option was omitted, and behavior is unchanged.
    pub fn crew_allowlist(&self, names: &[String]) -> Result<Option<CrewAllowlist>, OrbitError> {
        let mut allowed_names = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for name in names {
            // A human crew permits nothing: its name joins the list as named,
            // with no agent identity, so its tasks report `human_crew` rather
            // than an unknown crew. Names cannot collide with agent crews.
            if let Some(human) = self.human_crew(Some(name)) {
                allowed_names.insert(human.name.clone());
                continue;
            }
            let crew = resolve_crew(name.trim(), self.context.settings().crews())?;
            identities.insert(crew_identity(&crew));
            allowed_names.insert(crew.name);
        }
        Ok((!allowed_names.is_empty()).then_some(CrewAllowlist {
            names: allowed_names,
            identities,
        }))
    }

    /// Whether the configured crew `name` is one `allowlist` permits, by name
    /// or by the identity it resolves to. A name that no longer resolves is
    /// not permitted.
    pub fn crew_allowlist_permits(&self, allowlist: &CrewAllowlist, name: &str) -> bool {
        resolve_crew(name.trim(), self.context.settings().crews())
            .is_ok_and(|crew| allowlist.permits(&crew))
    }

    /// The allowlist carried by a run/activity input, or `None` when the run
    /// placed no restriction on its window.
    pub fn crew_allowlist_from_input(
        &self,
        input: &Value,
    ) -> Result<Option<CrewAllowlist>, OrbitError> {
        self.crew_allowlist(&crew_allowlist_names_from_input(input)?)
    }
}
