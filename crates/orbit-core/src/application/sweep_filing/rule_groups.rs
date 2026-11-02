//! Producer-declared defect groups. Target and cause policy stays in the producer.
use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskArtifact};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::is_open_ci_owner_status;
use super::rule_findings::{field, severity_rank, string_list};
use crate::OrbitRuntime;

pub(super) const STATE_PATH: &str = "rule-findings/state.json";
const STATE_MARKER: &str = "\n<!-- ux-rules:group-state -->\n";
const MAX_STATE_BYTES: usize = 1024 * 1024;
/// The owner context is script step input; keep it far below stdout budgets.
const MAX_OWNER_CONTEXT_BYTES: usize = 256 * 1024;

pub(super) struct PreparedGroups {
    pub results: Vec<Value>,
    pub states: BTreeMap<String, Value>,
    pub duplicates: Vec<Value>,
    pub historical: Vec<Value>,
    pub group_count: usize,
}

/// The defect groups open owners track, read without the filing lock and
/// without writing. Entries are opaque producer identities; a concurrent
/// owner created after this read is simply absent, so its members are never
/// reported by omission and gain no pass credit until actually observed.
pub(super) fn owner_context(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    let mut tasks = runtime.list_tasks_by_tags(&["ux-rules".to_string()])?;
    tasks.retain(|task| is_open_ci_owner_status(task.status));
    let mut groups: BTreeMap<String, Value> = BTreeMap::new();
    for task in &tasks {
        let Some(state) = read_state(runtime, task)? else {
            continue;
        };
        let group = field(&state, "group_fingerprint");
        let first = state
            .get("observations")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        let defect = state.get("defect").cloned().unwrap_or(Value::Null);
        if group.is_empty()
            || first.is_none()
            || defect.get("version").and_then(Value::as_u64).is_none()
            || defect.get("key").and_then(Value::as_str).is_none()
        {
            return Err(OrbitError::InvalidInput(format!(
                "rule group state on {} has no group fingerprint, defect or observations",
                task.id
            )));
        }
        let first = first.unwrap_or(&Value::Null);
        groups.entry(group.to_string()).or_insert_with(|| {
            json!({
                "group_fingerprint": group,
                "task_id": task.id,
                "surface": field(first, "surface"),
                "rule": field(first, "rule"),
                "defect": defect,
            })
        });
    }
    let context = json!({"schema_version": 1, "groups": groups.into_values().collect::<Vec<_>>()});
    if context.to_string().len() > MAX_OWNER_CONTEXT_BYTES {
        return Err(OrbitError::InvalidInput(
            "rule owner context exceeds 256 KiB; dispose of duplicate or obsolete ux-rules owners"
                .to_string(),
        ));
    }
    Ok(context)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn identity(value: &Value) -> String {
    orbit_types::identity::lower_hex(Sha256::digest(canonical(value).to_string().as_bytes()))
}

fn observation_id(value: &Value) -> String {
    identity(&json!([value.get("fingerprint"), value.get("target")]))
}

fn read_state(runtime: &OrbitRuntime, task: &Task) -> Result<Option<Value>, OrbitError> {
    if let Some(artifact) = runtime.get_task_artifact(&task.id, STATE_PATH)? {
        return serde_json::from_slice(&artifact.content)
            .map(Some)
            .map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "rule group state on {} is invalid: {error}",
                    task.id
                ))
            });
    }
    // Creation records the complete registry in the description before the
    // separately audited artifact update. A replay can repair interrupted attachments.
    Ok(task
        .description
        .split_once(STATE_MARKER)
        .and_then(|(_, tail)| tail.lines().next())
        .and_then(|line| serde_json::from_str(line).ok()))
}

pub(super) fn state_description(state: &Value) -> String {
    format!("{STATE_MARKER}{}\n", canonical(state))
}

pub(super) fn state_artifact(state: &Value) -> Result<TaskArtifact, OrbitError> {
    let content = serde_json::to_vec(&canonical(state))
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    if content.len() > MAX_STATE_BYTES {
        return Err(OrbitError::InvalidInput(
            "rule group observations exceed the 1 MiB task artifact budget; split the run"
                .to_string(),
        ));
    }
    Ok(TaskArtifact {
        path: STATE_PATH.to_string(),
        content,
        media_type: "application/json".to_string(),
        created_by: Some("system".to_string()),
    })
}

/// Partition and merge before the filing cap. The caller holds the workspace
/// filer lock across this read and all task updates, including evidence replay.
pub(super) fn prepare_groups(
    runtime: &OrbitRuntime,
    results: &[Value],
    surface: &str,
) -> Result<PreparedGroups, OrbitError> {
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut output = Vec::new();
    for result in results {
        let Some(defect) = result.get("defect") else {
            output.push(result.clone());
            continue;
        };
        let version = defect
            .get("version")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0);
        let key = defect
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.trim().is_empty() && key.len() <= 4096);
        if version.is_none()
            || key.is_none()
            || field(result, "fingerprint").is_empty()
            || field(result, "rule").is_empty()
        {
            return Err(OrbitError::InvalidInput("rule defect requires a positive version, bounded nonempty key, rule and observation fingerprint".to_string()));
        }
        if field(result, "status") == "error" {
            let mut error = result.clone();
            if let Some(object) = error.as_object_mut() {
                object.remove("defect");
            }
            output.push(error);
        }
        let scope = json!([
            result
                .get("surface")
                .and_then(Value::as_str)
                .unwrap_or(surface),
            field(result, "rule"),
            version,
            key
        ]);
        let fingerprint = format!("defect|{}", identity(&scope));
        groups.entry(fingerprint).or_default().push(result.clone());
    }
    let group_count = groups.len();
    if groups.is_empty() {
        return Ok(PreparedGroups {
            results: output,
            states: BTreeMap::new(),
            duplicates: Vec::new(),
            historical: Vec::new(),
            group_count,
        });
    }
    let mut tasks = runtime.list_tasks_by_tags(&["ux-rules".to_string()])?;
    tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    // Terminal matches are listed for operator disposition only: never an
    // owner, never reopened, never a reason to withhold a new task.
    let (tasks, terminal): (Vec<Task>, Vec<Task>) = tasks
        .into_iter()
        .partition(|task| is_open_ci_owner_status(task.status));
    let mut historical = Vec::new();
    let mut owner_states = BTreeMap::new();
    for task in &tasks {
        if let Some(state) = read_state(runtime, task)? {
            owner_states.insert(task.id.clone(), state);
        }
    }
    let mut states = BTreeMap::new();
    let mut duplicates = Vec::new();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for (group_key, entries) in groups {
        let aliases: BTreeSet<_> = entries
            .iter()
            .flat_map(|entry| {
                [
                    field(entry, "fingerprint"),
                    field(entry, "original_fingerprint"),
                ]
            })
            .filter(|s| !s.is_empty())
            .collect();
        let candidates: Vec<_> = tasks
            .iter()
            .filter(|task| {
                if claimed.contains(&task.id) {
                    return false;
                }
                if let Some(state) = owner_states.get(&task.id) {
                    return field(state, "group_fingerprint") == group_key;
                }
                task.rule_fingerprint
                    .as_deref()
                    .is_some_and(|fp| fp == group_key || aliases.contains(fp))
            })
            .collect();
        for task in &terminal {
            if task
                .rule_fingerprint
                .as_deref()
                .is_some_and(|fp| fp == group_key || aliases.contains(fp))
            {
                historical.push(json!({"group_fingerprint":group_key,"task_id":task.id,"status":task.status.to_string()}));
            }
        }
        let owner = candidates.first().copied();
        let fingerprint = owner
            .and_then(|task| task.rule_fingerprint.clone())
            .unwrap_or_else(|| group_key.clone());
        if let Some(owner) = owner {
            claimed.insert(owner.id.clone());
            for duplicate in candidates.iter().skip(1) {
                duplicates.push(json!({"group_fingerprint":group_key,"canonical_task_id":owner.id,"duplicate_task_id":duplicate.id}));
            }
        }
        let previous = owner.and_then(|owner| owner_states.get(&owner.id));
        let mut members: BTreeMap<String, Value> = previous
            .and_then(|state| state.get("observations"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|entry| (observation_id(entry), entry.clone()))
            .collect();
        let mut current: BTreeMap<String, Value> = BTreeMap::new();
        for entry in entries {
            let id = observation_id(&entry);
            // Contradictory repetitions never gain pass credit. Stable ordering
            // also makes a reordered document produce the identical state digest.
            let rank = |value: &Value| match field(value, "status") {
                "fail" => 4,
                "error" => 3,
                "skipped" => 2,
                "pass" => 1,
                _ => 0,
            };
            match current.get(&id) {
                Some(old)
                    if rank(old) > rank(&entry)
                        || (rank(old) == rank(&entry)
                            && canonical(old)
                                .to_string()
                                .cmp(&canonical(&entry).to_string())
                                .is_ge()) => {}
                _ => {
                    current.insert(id, entry);
                }
            }
        }
        let all_pass = !current.is_empty()
            && members.keys().all(|key| {
                current
                    .get(key)
                    .is_some_and(|entry| field(entry, "status") == "pass")
            })
            && current
                .values()
                .all(|entry| field(entry, "status") == "pass");
        let has_fail = current
            .values()
            .any(|entry| field(entry, "status") == "fail");
        for (id, mut entry) in current {
            // Keep the evidence mapping after a successful observation omits
            // screenshots; attached bytes remain historical evidence.
            let mut evidence: BTreeSet<String> = members
                .get(&id)
                .map(|old| string_list(old.get("evidence")))
                .unwrap_or_default()
                .into_iter()
                .collect();
            evidence.extend(string_list(entry.get("evidence")));
            entry["evidence"] = json!(evidence);
            members.insert(id, canonical(&entry));
        }
        let observations: Vec<_> = members.into_values().collect();
        let mut representative = observations
            .iter()
            .max_by(|a, b| {
                severity_rank(field(a, "severity"))
                    .cmp(&severity_rank(field(b, "severity")))
                    .then(canonical(a).to_string().cmp(&canonical(b).to_string()))
            })
            .cloned()
            .ok_or_else(|| OrbitError::InvalidInput("empty rule defect group".to_string()))?;
        let state = json!({"schema_version":1,"group_fingerprint":group_key,"defect":representative["defect"],"observations":observations});
        // Validate the complete state budget before any task can be mutated.
        let _ = state_artifact(&state)?;
        let evidence: BTreeSet<String> = observations
            .iter()
            .flat_map(|entry| string_list(entry.get("evidence")))
            .collect();
        for name in ["context_files", "source_paths", "tags"] {
            let values: BTreeSet<String> = observations
                .iter()
                .flat_map(|entry| string_list(entry.get(name)))
                .collect();
            if !values.is_empty() {
                representative[name] = json!(values);
            }
        }
        representative["fingerprint"] = json!(fingerprint);
        representative["status"] = json!(if has_fail {
            "fail"
        } else if all_pass {
            "pass"
        } else {
            "skipped"
        });
        representative["measurements"] =
            json!({"defect":state["defect"],"observations":observations});
        representative["evidence"] = json!(evidence);
        states.insert(fingerprint, state);
        output.push(representative);
    }
    Ok(PreparedGroups {
        results: output,
        states,
        duplicates,
        historical,
        group_count,
    })
}
