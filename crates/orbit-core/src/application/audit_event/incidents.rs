//! Report counts and denominators for grouped audit failure evidence.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::telemetry::{
    AuditEvent,
    incident::{
        FailureClass, FailureIncidentReport, classify, group_failure_incidents, has_tool_identity,
    },
};

/// Groups the scanned rows and keeps the raw denominators in the report.
pub(super) fn build_report(failures: &[AuditEvent], truncated: bool) -> FailureIncidentReport {
    let incidents = group_failure_incidents(failures);

    let mut raw_events_by_class: BTreeMap<String, u64> = BTreeMap::new();
    let mut job_run_lifecycle_events: u64 = 0;
    for event in failures {
        *raw_events_by_class
            .entry(classify(event).as_str().to_string())
            .or_insert(0) += 1;
        if !has_tool_identity(event) {
            job_run_lifecycle_events += 1;
        }
    }
    let mut incidents_by_class: BTreeMap<String, u64> = BTreeMap::new();
    let mut run_ids: BTreeSet<String> = BTreeSet::new();
    let mut run_ids_by_class: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut job_run_lifecycle_incidents: u64 = 0;
    for incident in &incidents {
        *incidents_by_class
            .entry(incident.class.as_str().to_string())
            .or_insert(0) += 1;
        if !incident.has_tool_identity {
            job_run_lifecycle_incidents += 1;
        }
        for run_id in &incident.run_ids {
            run_ids.insert(run_id.clone());
            run_ids_by_class
                .entry(incident.class.as_str().to_string())
                .or_default()
                .insert(run_id.clone());
        }
    }
    let affected_runs_by_class: BTreeMap<String, u64> = run_ids_by_class
        .into_iter()
        .map(|(class, ids)| (class, ids.len() as u64))
        .collect();
    let lifecycle_diagnostic_events = raw_events_by_class
        .get(FailureClass::Diagnostic.as_str())
        .copied()
        .unwrap_or(0);
    let lifecycle_diagnostic_incidents = incidents_by_class
        .get(FailureClass::Diagnostic.as_str())
        .copied()
        .unwrap_or(0);
    let lifecycle_diagnostic_affected_run_count = affected_runs_by_class
        .get(FailureClass::Diagnostic.as_str())
        .copied()
        .unwrap_or(0);

    FailureIncidentReport {
        raw_failed_events: failures.len() as u64,
        raw_events_by_class,
        incidents_by_class,
        affected_runs_by_class,
        incidents,
        affected_run_count: run_ids.len() as u64,
        job_run_lifecycle_events,
        job_run_lifecycle_incidents,
        lifecycle_diagnostic_events,
        lifecycle_diagnostic_incidents,
        lifecycle_diagnostic_affected_run_count,
        truncated,
    }
}
