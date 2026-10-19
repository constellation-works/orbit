//! One-file co-change, reservation overlap and undeclared landed partners.
use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::facts::{CodebaseFacts, TaskFacts};
use super::sections::{LARGE_FOOTPRINT_CAP, Params, rate_fields};

fn declares(task: &TaskFacts, path: &str) -> bool {
    task.footprint.iter().any(|anchor| {
        anchor.path == path
            || (anchor.dir
                && path
                    .strip_prefix(&anchor.path)
                    .is_some_and(|rest| rest.starts_with('/')))
    })
}

fn task_value(task: &TaskFacts) -> Value {
    json!({"task_id": task.id, "title": task.title, "commit": task.landed.as_ref().map(|landed| &landed.commit)})
}

pub(super) fn related(
    facts: &CodebaseFacts,
    params: &Params,
    path: &str,
    top: usize,
) -> Result<Value, OrbitError> {
    let path = path.trim_start_matches("./");
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return Err(OrbitError::InvalidInput(
            "path must be a repository-relative file path".into(),
        ));
    }
    if !(1..=100).contains(&top) {
        return Err(OrbitError::InvalidInput(
            "top must be between 1 and 100".into(),
        ));
    }
    let mut excluded_large = 0;
    let mut unavailable = 0;
    let mut singles = BTreeMap::<&str, usize>::new();
    let mut both = BTreeMap::<&str, Vec<&TaskFacts>>::new();
    let mut misses = BTreeMap::<&str, Vec<&TaskFacts>>::new();
    for task in &facts.tasks {
        let Some(landed) = &task.landed else {
            unavailable += usize::from(task.delivery_at.is_some_and(|at| params.within(at)));
            continue;
        };
        if !params.within(landed.at) {
            continue;
        }
        if landed.paths.len() > LARGE_FOOTPRINT_CAP {
            excluded_large += 1;
            continue;
        }
        for partner in &landed.paths {
            *singles.entry(partner).or_default() += 1;
            if partner == path {
                continue;
            }
            if landed.paths.contains(path) {
                both.entry(partner).or_default().push(task);
            }
            if declares(task, path) && !declares(task, partner) {
                misses.entry(partner).or_default().push(task);
            }
        }
    }
    let mut reserved = BTreeMap::<String, usize>::new();
    let mut reserved_both = BTreeMap::<String, (usize, BTreeSet<String>)>::new();
    for reservation in &facts.reservations {
        if !params.within(reservation.at) || reservation.files.len() > LARGE_FOOTPRINT_CAP {
            continue;
        }
        let keys: BTreeSet<_> = reservation
            .files
            .iter()
            .map(|a| {
                if a.dir {
                    format!("{}/", a.path)
                } else {
                    a.path.clone()
                }
            })
            .collect();
        for key in &keys {
            *reserved.entry(key.clone()).or_default() += 1;
            if key != path && keys.contains(path) {
                let entry = reserved_both.entry(key.clone()).or_default();
                entry.0 += 1;
                entry.1.extend(reservation.requesters.iter().cloned());
            }
        }
    }
    let path_count = singles.get(path).copied().unwrap_or(0);
    let mut rows = Vec::new();
    for partner in both.keys().copied() {
        let tasks = both.get(partner).cloned().unwrap_or_default();
        let missed = misses.get(partner).cloned().unwrap_or_default();
        // Support is shared landed tasks, as in the existing coupling ranking.
        if tasks.len() < params.min_support {
            continue;
        }
        let partner_count = singles.get(partner).copied().unwrap_or(0);
        let either = path_count + partner_count - tasks.len();
        // Count reservations, not requesting identities (one reservation may
        // name multiple tasks). The requesters are evidence only.
        let reservation = match reserved_both.get(partner) {
            None => Value::Null,
            Some((count, requesters)) => {
                let mut value = rate_fields(
                    *count,
                    reserved.get(path).copied().unwrap_or(0)
                        + reserved.get(partner).copied().unwrap_or(0)
                        - count,
                );
                value["requesters"] = json!(requesters);
                value["tasks"] = json!(
                    facts
                        .tasks
                        .iter()
                        .filter(|task| requesters.contains(&task.id))
                        .map(task_value)
                        .collect::<Vec<_>>()
                );
                value
            }
        };
        rows.push(json!({
            "path": partner,
            "jaccard": rate_fields(tasks.len(), either)["rate"],
            "conditional_rate": rate_fields(tasks.len(), path_count)["rate"],
            "support": {"both": tasks.len(), "path": path_count, "partner": partner_count, "either": either},
            "reservation_coupling": reservation,
            "footprint_miss_count": missed.len(),
            "tasks": tasks.into_iter().map(task_value).collect::<Vec<_>>(),
            "footprint_miss_tasks": missed.into_iter().map(task_value).collect::<Vec<_>>(),
        }));
    }
    rows.sort_by(|a, b| {
        b["jaccard"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["jaccard"].as_f64().unwrap_or(0.0))
            .then_with(|| {
                b["support"]["both"]
                    .as_u64()
                    .cmp(&a["support"]["both"].as_u64())
            })
            .then_with(|| a["path"].as_str().cmp(&b["path"].as_str()))
    });
    rows.truncate(top);
    Ok(
        json!({"path": path, "rows": rows, "top": top, "row_limit": top, "rollup": "file", "touched": "landed_diff", "path_support": path_count, "excluded_large": excluded_large, "unavailable_landed_tasks": unavailable, "large_footprint_cap": LARGE_FOOTPRINT_CAP}),
    )
}

pub(super) fn known_paths(facts: &CodebaseFacts) -> BTreeSet<String> {
    facts
        .tasks
        .iter()
        .flat_map(|task| {
            task.footprint
                .iter()
                .filter(|a| !a.dir)
                .map(|a| a.path.clone())
                .chain(task.landed.iter().flat_map(|l| l.paths.iter().cloned()))
        })
        .chain(
            facts
                .reservations
                .iter()
                .flat_map(|r| r.files.iter().filter(|a| !a.dir).map(|a| a.path.clone())),
        )
        .collect()
}
