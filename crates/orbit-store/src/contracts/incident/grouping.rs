//! Passes 2 to 4: cluster rows by run scope and signature, collapse
//! in-run cascades, then fold cited-run cascades; and the report built on
//! top.

use super::classify::{classify, has_tool_identity, surface_of};
use super::signature::{incident_id, signature_for};
use super::types::{
    CASCADE_WINDOW_SECS, FailureClass, FailureIncident, FailureIncidentReport, IncidentEventRef,
    PropagationLink,
};
use chrono::{DateTime, Duration, Utc};
use orbit_types::telemetry::AuditEvent;
use std::collections::{BTreeMap, BTreeSet};

/// Per-incident cap on the sampled raw events echoed back to callers. The
/// full population is always reachable through the raw Audit view; this bound
/// keeps one burst of thousands from dominating a response body.
const MAX_SAMPLE_EVENTS: usize = 20;

/// Groups already-filtered failure rows into a report. Split out from the
/// store call so the contract is testable without a database.
pub fn build_report(failures: &[AuditEvent], truncated: bool) -> FailureIncidentReport {
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

/// One `(run scope, signature)` bucket before cascade collapsing.
#[derive(Debug, Clone)]
pub(super) struct Cluster {
    signature: String,
    class: FailureClass,
    role: String,
    surface: String,
    activity_id: Option<String>,
    message: Option<String>,
    event_count: u64,
    first_ts: DateTime<Utc>,
    last_ts: DateTime<Utc>,
    run_ids: Vec<String>,
    task_ids: Vec<String>,
    sample_events: Vec<IncidentEventRef>,
    events: Vec<IncidentEventRef>,
    has_tool_identity: bool,
}

impl Cluster {
    fn absorb(&mut self, event: &AuditEvent) {
        self.event_count += 1;
        self.first_ts = self.first_ts.min(event.timestamp);
        self.last_ts = self.last_ts.max(event.timestamp);
        if self.activity_id.is_none() {
            self.activity_id.clone_from(&event.activity_id);
        }
        if self.message.is_none() {
            self.message.clone_from(&event.error_message);
        }
        push_unique(&mut self.run_ids, event.job_run_id.as_deref());
        push_unique(&mut self.task_ids, event.task_id.as_deref());
        let reference = event_ref(event);
        self.events.push(reference.clone());
        if self.sample_events.len() < MAX_SAMPLE_EVENTS {
            self.sample_events.push(reference);
        }
        self.has_tool_identity |= has_tool_identity(event);
    }

    fn into_link(mut self) -> PropagationLink {
        sort_samples(&mut self.sample_events);
        PropagationLink {
            signature: self.signature,
            surface: self.surface,
            activity_id: self.activity_id,
            event_count: self.event_count,
            first_ts: self.first_ts,
            last_ts: self.last_ts,
            message: self.message,
            sample_events: self.sample_events,
        }
    }
}

fn push_unique(target: &mut Vec<String>, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if !target.iter().any(|existing| existing == value) {
        target.push(value.to_string());
    }
}

fn event_ref(event: &AuditEvent) -> IncidentEventRef {
    IncidentEventRef {
        id: event.id,
        ts: event.timestamp,
        execution_id: event.execution_id.clone(),
        status: event.status.to_string(),
        role: event.role.clone(),
        surface: surface_of(event),
        run_id: event.job_run_id.clone(),
        task_id: event.task_id.clone(),
        activity_id: event.activity_id.clone(),
        tool_name: event
            .tool_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned),
        message: event.error_message.clone(),
    }
}

/// Newest raw evidence first, ties broken by id so a page is stable.
fn sort_samples(samples: &mut [IncidentEventRef]) {
    samples.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| b.id.cmp(&a.id)));
}

/// Groups failed audit rows into incidents. Pure and order-independent: the
/// same rows in any input order produce the same incidents.
pub fn group_failure_incidents(failures: &[AuditEvent]) -> Vec<FailureIncident> {
    // Pass 1+2 — cluster by (run scope, signature).
    let mut clusters: BTreeMap<(String, String), Cluster> = BTreeMap::new();
    for event in failures {
        let signature = signature_for(event);
        let run_scope = event
            .job_run_id
            .clone()
            .filter(|run| !run.is_empty())
            .unwrap_or_default();
        clusters
            .entry((run_scope, signature.clone()))
            .and_modify(|cluster| cluster.absorb(event))
            .or_insert_with(|| {
                let mut cluster = Cluster {
                    signature,
                    class: classify(event),
                    role: event.role.clone(),
                    surface: surface_of(event),
                    activity_id: None,
                    message: None,
                    event_count: 0,
                    first_ts: event.timestamp,
                    last_ts: event.timestamp,
                    run_ids: Vec::new(),
                    task_ids: Vec::new(),
                    sample_events: Vec::new(),
                    events: Vec::new(),
                    has_tool_identity: false,
                };
                cluster.absorb(event);
                cluster
            });
    }

    // Pass 3 — collapse same-run, same-class cascades onto their earliest
    // cluster. Clusters with no run id cannot be attributed to a pipeline, so
    // each stays its own incident (still deduplicated by signature above).
    let mut by_scope: BTreeMap<String, Vec<Cluster>> = BTreeMap::new();
    let mut incidents = Vec::new();
    for ((run_scope, _), cluster) in clusters {
        if run_scope.is_empty() {
            incidents.push(incident_from(&run_scope, cluster, Vec::new()));
        } else {
            by_scope.entry(run_scope).or_default().push(cluster);
        }
    }

    let cascade_window = Duration::seconds(CASCADE_WINDOW_SECS);
    for (run_scope, scope_clusters) in by_scope {
        for (_, mut class_clusters) in partition_by_class(scope_clusters) {
            class_clusters.sort_by(|a, b| {
                a.first_ts
                    .cmp(&b.first_ts)
                    .then_with(|| a.signature.cmp(&b.signature))
            });
            let mut iter = class_clusters.into_iter();
            let Some(mut root) = iter.next() else {
                continue;
            };
            let mut chain: Vec<Cluster> = Vec::new();
            let mut chain_end = root.last_ts;
            for cluster in iter {
                if cluster.first_ts - chain_end <= cascade_window {
                    chain_end = chain_end.max(cluster.last_ts);
                    chain.push(cluster);
                    continue;
                }
                incidents.push(incident_from(&run_scope, root, std::mem::take(&mut chain)));
                chain_end = cluster.last_ts;
                root = cluster;
            }
            incidents.push(incident_from(&run_scope, root, chain));
        }
    }

    // Pass 4 — collapse parent/child propagation across job runs. A later
    // incident whose raw message cites another incident's `job_run_id` is the
    // same root cause (a guard copying a leaf failure), not a second incident.
    incidents = collapse_cited_run_cascades(incidents);

    // Newest incident first; the deterministic id breaks ties so two incidents
    // sharing a last-seen timestamp never swap places between renders.
    incidents.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then_with(|| a.incident_id.cmp(&b.incident_id))
    });
    incidents
}

fn partition_by_class(clusters: Vec<Cluster>) -> BTreeMap<FailureClass, Vec<Cluster>> {
    let mut out: BTreeMap<FailureClass, Vec<Cluster>> = BTreeMap::new();
    for cluster in clusters {
        out.entry(cluster.class).or_default().push(cluster);
    }
    out
}

fn incident_from(run_scope: &str, root: Cluster, chain: Vec<Cluster>) -> FailureIncident {
    let mut root = root;
    sort_samples(&mut root.sample_events);
    sort_samples(&mut root.events);
    let mut event_count = root.event_count;
    let mut last_ts = root.last_ts;
    let mut run_ids = root.run_ids.clone();
    let mut task_ids = root.task_ids.clone();
    let mut events = root.events.clone();
    let has_tool_identity = root.has_tool_identity;

    let mut propagation: Vec<PropagationLink> = Vec::with_capacity(chain.len());
    for cluster in chain {
        event_count += cluster.event_count;
        last_ts = last_ts.max(cluster.last_ts);
        for run_id in &cluster.run_ids {
            push_unique(&mut run_ids, Some(run_id));
        }
        for task_id in &cluster.task_ids {
            push_unique(&mut task_ids, Some(task_id));
        }
        events.extend(cluster.events.iter().cloned());
        propagation.push(cluster.into_link());
    }
    propagation.sort_by(|a, b| {
        a.first_ts
            .cmp(&b.first_ts)
            .then_with(|| a.signature.cmp(&b.signature))
    });
    sort_samples(&mut events);

    FailureIncident {
        incident_id: incident_id(run_scope, &root.signature, root.first_ts),
        signature: root.signature,
        class: root.class,
        role: root.role,
        surface: root.surface,
        activity_id: root.activity_id,
        message: root.message,
        event_count,
        root_event_count: root.event_count,
        first_ts: root.first_ts,
        last_ts,
        run_ids,
        task_ids,
        sample_events: root.sample_events,
        events,
        has_tool_identity,
        propagation,
    }
}

/// Fold incidents whose raw messages cite another incident's `job_run_id`
/// onto that cited incident. Parent/child pipeline guards are the motivating
/// case; matching is by durable columns only (message tokens ∩ known run ids).
fn collapse_cited_run_cascades(mut incidents: Vec<FailureIncident>) -> Vec<FailureIncident> {
    if incidents.len() < 2 {
        return incidents;
    }
    let cascade_window = Duration::seconds(CASCADE_WINDOW_SECS);
    loop {
        let known_runs = unique_run_index(&incidents);
        if known_runs.is_empty() {
            break;
        }
        let known_ids: BTreeMap<String, ()> = known_runs
            .keys()
            .cloned()
            .map(|run_id| (run_id, ()))
            .collect();
        let mut merge: Option<(usize, usize)> = None;
        for (from_idx, incident) in incidents.iter().enumerate() {
            for cited in cited_known_run_ids_from_incident(incident, &known_ids) {
                let Some(&onto_idx) = known_runs.get(&cited) else {
                    continue;
                };
                if onto_idx == usize::MAX || onto_idx == from_idx {
                    continue;
                }
                let onto = &incidents[onto_idx];
                if onto.class != incident.class {
                    continue;
                }
                if incident.first_ts < onto.first_ts {
                    continue;
                }
                if incident.first_ts - onto.last_ts > cascade_window {
                    continue;
                }
                merge = Some((from_idx, onto_idx));
                break;
            }
            if merge.is_some() {
                break;
            }
        }
        let Some((from_idx, onto_idx)) = merge else {
            break;
        };
        let from = incidents.remove(from_idx);
        let onto_idx = if onto_idx > from_idx {
            onto_idx - 1
        } else {
            onto_idx
        };
        merge_incident_into(&mut incidents[onto_idx], from);
    }
    incidents
}

fn unique_run_index(incidents: &[FailureIncident]) -> BTreeMap<String, usize> {
    let mut known_runs: BTreeMap<String, usize> = BTreeMap::new();
    for (idx, incident) in incidents.iter().enumerate() {
        for run_id in &incident.run_ids {
            known_runs
                .entry(run_id.clone())
                .and_modify(|existing| *existing = usize::MAX)
                .or_insert(idx);
        }
    }
    known_runs
}

fn cited_known_run_ids_from_incident(
    incident: &FailureIncident,
    known: &BTreeMap<String, ()>,
) -> Vec<String> {
    let mut found = Vec::new();
    let mut consider = |message: Option<&str>| {
        if let Some(message) = message {
            for run_id in cited_known_run_ids(message, known) {
                if !found.iter().any(|existing| existing == &run_id) {
                    found.push(run_id);
                }
            }
        }
    };
    consider(incident.message.as_deref());
    for event in incident.events.iter().chain(incident.sample_events.iter()) {
        consider(event.message.as_deref());
    }
    for link in &incident.propagation {
        consider(link.message.as_deref());
        for event in &link.sample_events {
            consider(event.message.as_deref());
        }
    }
    found
}

fn cited_known_run_ids(message: &str, known: &BTreeMap<String, ()>) -> Vec<String> {
    if known.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for token in message.split_whitespace() {
        let trimmed = token.trim_matches(['"', '\'', '`', '(', ')', ',', ':', ';']);
        if known.contains_key(trimmed) && !found.iter().any(|existing| existing == trimmed) {
            found.push(trimmed.to_string());
        }
    }
    found
}

fn merge_incident_into(root: &mut FailureIncident, child: FailureIncident) {
    root.event_count += child.event_count;
    root.last_ts = root.last_ts.max(child.last_ts);
    for run_id in &child.run_ids {
        push_unique(&mut root.run_ids, Some(run_id));
    }
    for task_id in &child.task_ids {
        push_unique(&mut root.task_ids, Some(task_id));
    }
    root.events.extend(child.events.iter().cloned());
    sort_samples(&mut root.events);
    let child_root_last = child
        .sample_events
        .iter()
        .map(|event| event.ts)
        .max()
        .unwrap_or(child.last_ts);
    root.propagation.push(PropagationLink {
        signature: child.signature,
        surface: child.surface,
        activity_id: child.activity_id,
        event_count: child.root_event_count,
        first_ts: child.first_ts,
        last_ts: child_root_last,
        message: child.message,
        sample_events: child.sample_events,
    });
    root.propagation.extend(child.propagation);
    root.propagation.sort_by(|a, b| {
        a.first_ts
            .cmp(&b.first_ts)
            .then_with(|| a.signature.cmp(&b.signature))
    });
}
