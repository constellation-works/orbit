//! Consecutive exclusion of a configured pool member, independent of whether
//! any task happened to draw it in this pass.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_store::contracts::FrictionAddParams;
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
                .find(|unset| unset.crew == name)
                .map(|unset| unset.describe())
                .or_else(|| {
                    exclusions
                        .iter()
                        .find(|entry| entry.crew == name)
                        .map(|entry| entry.reason.clone())
                })
                .or_else(|| {
                    crews.get(&name).and_then(|crew| {
                        if !crew.enabled {
                            Some("crew is disabled on this host".to_string())
                        } else if allowed.is_some_and(|allowed| !allowed.permits(crew)) {
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
            let key = format!(
                "crew-pool-excluded:{run_id}:{crew}:{}",
                streak.since.to_rfc3339()
            );
            let frictions = crate::runtime::friction::store_for(runtime)?;
            let accepted = frictions.tags()?;
            let tags = ["automation", "other"]
                .into_iter()
                .find(|tag| accepted.iter().any(|known| known == tag))
                .map(str::to_string)
                .into_iter()
                .collect();
            frictions.add_or_reuse(&key, FrictionAddParams {
                model: "system".into(),
                title: Some(format!("Pool crew `{crew}` excluded on every admission pass")),
                body: format!("{}\nInspect `orbit run show {run_id}` and the managed launcher's environment; after repairing an env source, restart the service and start a new drain.\ndedupe-key: {key}\n", streak.describe(&crew)),
                tags, status: FrictionStatus::Open, during_task: None, created_at: now,
            })?;
        }
        Ok::<_, orbit_common::OrbitError>(())
    })();
    if let Err(error) = result {
        tracing::warn!(target: "orbit.core.job_run", run_id, %error, "could not record sustained pool crew exclusions");
    }
}
