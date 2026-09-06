use orbit_engine::CrewConfig;
use orbit_types::identity::CrewAssignment;
use orbit_types::workflow::activity_job::Provider;

/// Convert a crew assignment (string fields) into the typed [`CrewConfig`]
/// surface used by the engine resolver. An unrecognized `provider` yields
/// `None` for that field with a warn-log — silently coercing dispatch onto a
/// different runtime would defeat the point of the override.
pub(crate) fn typed_crew_config_from_assignment(raw: &CrewAssignment) -> CrewConfig {
    let provider = Some(raw.provider.as_str()).and_then(|raw_value| {
        // Canonical string→provider parsing lives on the orbit-common `Provider`
        // surface (ORB-10091); the crew path routes through it so casing/alias
        // handling cannot drift from the other layers. `resolve_name` preserves
        // the deprecation signal so a legacy alias resolves *and* warns.
        match Provider::resolve_name(raw_value) {
            Ok(identity) => {
                if let Some(deprecation) = identity.deprecation {
                    tracing::warn!(
                        target: "orbit.config.crew",
                        alias = %deprecation.alias,
                        canonical = %deprecation.canonical,
                        "[crews.<name>].provider uses a deprecated alias; resolving to the canonical id — update the config",
                    );
                }
                Some(identity.provider)
            }
            Err(_) => {
                tracing::warn!(
                    target: "orbit.config.crew",
                    raw = raw_value,
                    "[crews.<name>].provider has an unrecognized value; falling back to inline activity provider",
                );
                None
            }
        }
    });

    let model = raw.model.trim();
    let model = (!model.is_empty()).then(|| model.to_string());

    CrewConfig {
        provider,
        model,
        reasoning_effort: raw.effort,
    }
}
