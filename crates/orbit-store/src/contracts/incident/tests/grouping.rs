// Admitted unit test: combinatorial pure grouping logic. The frozen merger
// is an oracle for fold precedence, ambiguous ownership, and moving windows.
use super::*;
use orbit_types::telemetry::AuditEventStatus;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Fixture {
    name: String,
    expected_incidents: usize,
    // run, second offset, message, role, denied
    rows: Vec<(String, i64, String, String, bool)>,
}

fn event(id: i64, run: &str, second: i64, message: &str, role: &str, denied: bool) -> AuditEvent {
    serde_json::from_value(serde_json::json!({
        "id":id, "execution_id":format!("execution-{id}"),
        "timestamp":DateTime::from_timestamp(1_780_000_000 + second, 0).unwrap(),
        "command":"tool", "subcommand":"run", "tool_name":"orbit.fixture.run",
        "target_type":null, "target_id":null, "role":role,
        "status":if denied { AuditEventStatus::Denied } else { AuditEventStatus::Failure },
        "exit_code":1, "duration_ms":1, "working_directory":".",
        "arguments_json":null, "stdout_truncated":null, "stderr_truncated":null,
        "error_message":message, "host":null, "pid":1, "session_id":null,
        "job_run_id":run,
    }))
    .unwrap()
}

fn assert_equivalent(rows: &[AuditEvent], label: &str) {
    let before = group_run_cascades(rows);
    assert_eq!(
        collapse_cited_run_cascades(before.clone()),
        legacy_collapse(before),
        "{label}"
    );
}

#[test]
fn cached_citations_preserve_recorded_folds_and_combinatorial_precedence() {
    let fixtures: Vec<Fixture> = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/incidents/cited-run-cascades.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for fixture in fixtures {
        let rows: Vec<_> = fixture
            .rows
            .iter()
            .enumerate()
            .map(|(i, (run, ts, msg, role, denied))| event(i as i64, run, *ts, msg, role, *denied))
            .collect();
        assert_eq!(
            group_failure_incidents(&rows).len(),
            fixture.expected_incidents,
            "fixture {} must exercise its recorded cascade shape",
            fixture.name
        );
        assert_equivalent(&rows, &fixture.name);
        let reversed: Vec<_> = rows.into_iter().rev().collect();
        assert_equivalent(&reversed, &format!("{} reversed", fixture.name));
    }
    // Deterministic generated corpora cover citation order vs timestamp order,
    // equal timestamps, unknown/self citations, mixed classes, repeat run ids,
    // citations present only in raw evidence beyond the sample cap, and folds
    // whose extension of a target's last_ts unlocks a previously ineligible edge.
    let mut seed = 41_u64;
    for case in 0..160 {
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            seed >> 32
        };
        let rows: Vec<_> = (0..48)
            .map(|id| {
                let run = format!("run-{}", next() % 12);
                let second = (next() % 240) as i64;
                let role = if next().is_multiple_of(3) {
                    "claude"
                } else {
                    "codex"
                };
                let denied = next().is_multiple_of(5);
                let message = format!(
                    "failure kind{} citing (`run-{}`), run-{} unknown",
                    next() % 7,
                    next() % 12,
                    next() % 12
                );
                event(id, &run, second, &message, role, denied)
            })
            .collect();
        assert_equivalent(&rows, &format!("generated case {case}"));
    }
}

fn legacy_collapse(mut incidents: Vec<FailureIncident>) -> Vec<FailureIncident> {
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
