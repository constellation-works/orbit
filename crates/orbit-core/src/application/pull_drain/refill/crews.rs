//! The crews a pull drain's window can run [ORB-13941].

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::{CrewExclusion, CrewExclusionSource, PullCrewPreflight};

use crate::OrbitRuntime;
use crate::application::distributed::PullCrewWindow;

/// The drain's crew window, taking and persisting its preflight on the first
/// pass of the window. A preflight that cannot be persisted is still used for
/// this pass, and taken again on the next. The owner's local drain takes its
/// window the same way, for the census row it declares [ORB-17304].
pub(crate) fn crew_window(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<PullCrewWindow, OrbitError> {
    let stored = runtime
        .read_run_state(run_id)?
        .and_then(|state| state.pull_drain.pull_crew_preflight);
    let preflight = match stored {
        Some(preflight) => preflight,
        None => {
            let preflight = crew_preflight(runtime);
            let mut pending = Some(preflight.clone());
            if let Err(failure) =
                runtime
                    .stores()
                    .jobs()
                    .update_run_state(run_id, &mut |_, state| {
                        if state.pull_drain.pull_crew_preflight.is_none() {
                            state.pull_drain.pull_crew_preflight = pending.take();
                        }
                        Ok(())
                    })
            {
                tracing::warn!(
                    target: "orbit.core.pull",
                    run_id,
                    %failure,
                    "pull drain could not record its crew preflight; the next pass takes it again",
                );
            }
            preflight
        }
    };
    runtime.crew_window_from(run_id, Some(preflight))
}

/// The window's provider preflight [ORB-13941]: every configured crew this
/// host could dispatch now, resolved the way dispatch resolves it — enabled,
/// every `[crews.<name>] env` source set, its provider's executor resolvable,
/// and that executor's CLI found where a leaf would launch it. Cheap: no
/// provider process is started. An unauthenticated CLI passes here and is
/// caught by its first claimed leaf. Only auth-excluded providers with a
/// declared probe may recover later.
fn crew_preflight(runtime: &OrbitRuntime) -> PullCrewPreflight {
    let registry = runtime.configured_crew_registry_projection();
    let unset_env = runtime.unset_crew_env();
    let mut runnable = Vec::new();
    let mut excluded = Vec::new();
    for crew in &registry.crews {
        let unset = unset_env.iter().find(|unset| unset.crew == crew.name);
        let unusable = if let Some(unset) = unset {
            // A crew whose credential source is unset would run on the
            // provider's default login instead; dispatch refuses it.
            Some(format!("{} is unset on this host", unset.describe()))
        } else if crew.enabled {
            match RuntimeHost::resolve_cli_executor(runtime, &crew.provider) {
                Ok(executor) => runtime
                    .locate_provider_launcher(&executor.command)
                    .is_none()
                    .then(|| {
                        format!(
                            "provider `{}` CLI `{}` was not found on this host",
                            crew.provider, executor.command
                        )
                    }),
                Err(failure) => Some(format!("provider `{}`: {failure}", crew.provider)),
            }
        } else {
            Some(format!(
                "disabled here (`[crews.{}] enabled = false`)",
                crew.name
            ))
        };
        match unusable {
            None => runnable.push(crew.name.clone()),
            Some(reason) => excluded.push(CrewExclusion {
                crew: crew.name.clone(),
                source: CrewExclusionSource::Preflight,
                reason,
                until: None,
            }),
        }
    }
    PullCrewPreflight {
        checked_at: chrono::Utc::now(),
        runnable,
        default_crew: registry.default_crew,
        excluded,
    }
}
