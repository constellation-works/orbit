//! Consecutive exclusion of a configured pool member, independent of whether
//! any task happened to draw it in this pass.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_store::contracts::{FrictionAddParams, FrictionListFilter};
use orbit_types::identity::{resolve_crew, same_crew_family};
use orbit_types::record::FrictionStatus;
use orbit_types::workflow::{CrewExclusion, CrewPoolExclusion};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::engine::crew::CrewAllowlist;

pub(crate) fn record_pool_crew_health(
    runtime: &OrbitRuntime,
    run_id: &str,
    input: &Value,
    allowed: Option<&CrewAllowlist>,
    exclusions: &[CrewExclusion],
) {
    let result = (|| {
        let members = runtime.pool_health_crew_names(input)?;
        let recorded = runtime.read_run_state(run_id)?;
        if members.is_empty()
            && recorded
                .as_ref()
                .is_none_or(|state| state.pull_drain.crew_pool_exclusions.is_empty())
        {
            return Ok(());
        }
        let window = runtime.crew_window_from(
            run_id,
            recorded.and_then(|state| state.pull_drain.pull_crew_preflight),
        )?;
        let window_allowed = window
            .allowed
            .as_ref()
            .map(|names| runtime.crew_allowlist(names))
            .transpose()?
            .flatten();
        let allowed = allowed.or(window_allowed.as_ref());
        let exclusions = if exclusions.is_empty() {
            window.excluded.as_slice()
        } else {
            exclusions
        };
        let unset = runtime.unset_crew_env();
        let crews = runtime.context.settings().crews();
        let mut excluded = BTreeMap::new();
        for name in members {
            let reason = unset
                .iter()
                .find(|unset| same_crew_family(&unset.crew, &name))
                .map(|unset| unset.describe())
                .or_else(|| {
                    exclusions
                        .iter()
                        .find(|entry| same_crew_family(&entry.crew, &name))
                        .map(|entry| entry.reason.clone())
                })
                .or_else(|| {
                    resolve_crew(&name, crews).ok().and_then(|crew| {
                        if !crew.enabled {
                            Some("crew is disabled on this host".to_string())
                        } else if allowed.is_some_and(|allowed| !allowed.permits(&crew)) {
                            Some("crew is outside this drain's allowed window".to_string())
                        } else {
                            None
                        }
                    })
                });
            if let Some(reason) = reason {
                excluded.insert(name, reason);
            }
        }
        let now = Utc::now();
        let mut warnings = Vec::new();
        runtime
            .stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                state
                    .pull_drain
                    .crew_pool_exclusions
                    .retain(|crew, _| excluded.contains_key(crew));
                for (crew, reason) in &excluded {
                    let streak = state
                        .pull_drain
                        .crew_pool_exclusions
                        .entry(crew.clone())
                        .or_insert_with(|| CrewPoolExclusion {
                            since: now,
                            last_observed_at: now,
                            passes: 0,
                            reason: reason.clone(),
                        });
                    streak.passes = streak.passes.saturating_add(1);
                    streak.last_observed_at = now;
                    streak.reason = reason.clone();
                    if streak.sustained() {
                        warnings.push((crew.clone(), streak.clone()));
                    }
                }
                Ok(())
            })?;
        for (crew, streak) in warnings {
            if runtime.coordination_task_write_refusal().is_some() {
                tracing::warn!(target: "orbit.core.job_run", signal = "crew_pool_excluded", run_id, crew, warning = %streak.describe(&crew), "configured pool crew is excluded on every admission pass");
                continue;
            }
            let frictions = crate::runtime::friction::store_for(runtime)?;
            let key = dedupe_key(&crew, &streak.reason);
            let accepted = frictions.tags()?;
            let tags = ["automation", "other"]
                .into_iter()
                .find(|tag| accepted.iter().any(|known| known == tag))
                .map(str::to_string)
                .into_iter()
                .collect();
            #[cfg(test)]
            file_hook::run();
            // The duplicate check and the insert share one store transaction,
            // so concurrent drains cannot both file [ORB-22391].
            frictions.add_unless_duplicate(
                &open_pool_crew_filter(&crew),
                &|row| exclusion_reason_from_body(&row.record.body, &crew).as_deref() == Some(streak.reason.as_str()),
                FrictionAddParams {
                model: "system".into(),
                title: Some(format!("Pool crew `{crew}` excluded on every admission pass")),
                body: format!("{}\nInspect `orbit run show {run_id}` and the managed launcher's environment; after repairing an env source, restart the service and start a new drain.\ndedupe-key: {key}\n", streak.describe(&crew)),
                tags, status: FrictionStatus::Open, during_task: None, created_at: now,
                },
            )?;
        }
        Ok::<_, orbit_common::OrbitError>(())
    })();
    if let Err(error) = result {
        tracing::warn!(target: "orbit.core.job_run", run_id, %error, "could not record sustained pool crew exclusions");
    }
}

/// Stable identity for one crew and one unchanged exclusion cause, across
/// drains and streaks. The key is a single line so it can be written as the
/// body's `dedupe-key:` line.
fn dedupe_key(crew: &str, reason: &str) -> String {
    let sanitized: String = reason
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    format!("crew-pool-excluded:{crew}:{}", sanitized.trim())
}

/// Lists the open and triaged frictions that could record this crew excluded.
/// Matches on the crew's title, so frictions filed under the old per-drain key
/// still suppress a duplicate.
fn open_pool_crew_filter(crew: &str) -> FrictionListFilter {
    FrictionListFilter {
        statuses: vec![FrictionStatus::Open, FrictionStatus::Triaged],
        q: Some(format!(
            "Pool crew `{crew}` excluded on every admission pass"
        )),
        ..Default::default()
    }
}

/// The exclusion cause carried by a pool-crew friction body: the reason
/// suffix of its shared `describe` first line. `None` for a body that does
/// not carry this crew's line (another crew, or a different record).
fn exclusion_reason_from_body(body: &str, crew: &str) -> Option<String> {
    let first = body.lines().next()?;
    let prefix = format!("Pool crew `{crew}` excluded on every admission pass");
    if !first.starts_with(&prefix) {
        return None;
    }
    first
        .split_once("): ")
        .map(|(_, reason)| reason.to_string())
}

/// Production has no hook. A test installs one so two drains both reach the
/// filing step before either writes.
#[cfg(test)]
pub(crate) mod file_hook {
    use std::sync::{Arc, Mutex};

    static HOOK: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

    pub(crate) fn install(hook: impl Fn() + Send + Sync + 'static) {
        *HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(hook));
    }

    pub(crate) fn clear() {
        *HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    pub(super) fn run() {
        let hook = HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}
