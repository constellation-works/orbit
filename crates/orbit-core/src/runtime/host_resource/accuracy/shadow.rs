//! Aggregate shadow and enforce decisions by drain.

use std::collections::{BTreeMap, BTreeSet};

use orbit_store::contracts::AdmissionForecastRecord;

use super::DrainShadowDecisions;

const SHADOW: &str = "shadow";

pub(super) fn shadow_decisions(records: &[AdmissionForecastRecord]) -> Vec<DrainShadowDecisions> {
    #[derive(Default)]
    struct Drain {
        shadow_passes: u64,
        enforce_passes: u64,
        admitted: BTreeSet<String>,
        would_hold: BTreeSet<String>,
        would_add: BTreeSet<String>,
        enforce_admitted: BTreeSet<String>,
        gap_held: BTreeSet<String>,
        pull_requests: u64,
        pull_sent: u64,
        pull_would_hold: u64,
        pull_held: u64,
    }
    let mut drains: BTreeMap<(&str, &str), Drain> = BTreeMap::new();
    for record in records {
        let drain = drains
            .entry((record.workspace_id.as_str(), record.run_id.as_str()))
            .or_default();
        if let Some(pull) = &record.pull {
            drain.pull_requests += 1;
            drain.pull_sent += u64::from(pull.sent);
            if record.mode == SHADOW {
                drain.shadow_passes += 1;
                drain.admitted.extend(record.admitted.iter().cloned());
                drain.pull_would_hold += u64::from(pull.forecast_no_headroom);
            } else {
                drain.enforce_passes += 1;
                drain.pull_held += u64::from(pull.forecast_no_headroom);
            }
            continue;
        }
        if record.mode != SHADOW {
            drain.enforce_passes += 1;
            drain
                .enforce_admitted
                .extend(record.admitted.iter().cloned());
            drain.gap_held.extend(record.fit_skipped.iter().cloned());
            continue;
        }
        drain.shadow_passes += 1;
        let fit: BTreeSet<&String> = record.fit_admitted.iter().collect();
        let admitted: BTreeSet<&String> = record.admitted.iter().collect();
        drain
            .admitted
            .extend(admitted.iter().map(|id| (*id).clone()));
        drain
            .would_hold
            .extend(admitted.difference(&fit).map(|id| (*id).clone()));
        drain
            .would_add
            .extend(fit.difference(&admitted).map(|id| (*id).clone()));
    }
    drains
        .into_iter()
        .map(|((workspace_id, run_id), drain)| DrainShadowDecisions {
            workspace_id: workspace_id.to_string(),
            run_id: run_id.to_string(),
            shadow_passes: drain.shadow_passes,
            enforce_passes: drain.enforce_passes,
            admitted: drain.admitted.len() as u64,
            would_hold: drain.would_hold.into_iter().collect(),
            would_add: drain.would_add.into_iter().collect(),
            enforce_admitted: drain.enforce_admitted.len() as u64,
            gap_held: drain.gap_held.into_iter().collect(),
            pull_requests: drain.pull_requests,
            pull_sent: drain.pull_sent,
            pull_would_hold: drain.pull_would_hold,
            pull_held: drain.pull_held,
        })
        .collect()
}
