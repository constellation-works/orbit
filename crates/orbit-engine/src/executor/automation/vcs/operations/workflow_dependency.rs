//! Static, bounded workflow dependency evidence. Expressions are never executed.
use orbit_common::OrbitError;
use orbit_types::workflow::RequiredGate;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use super::prerequisite::unreadable;

pub(super) struct Dependency {
    pub job: String,
    pub prerequisite: String,
    pub(super) workflow: Value,
    edges: BTreeMap<String, Vec<String>>,
    names: BTreeMap<String, Vec<String>>,
}

/// Match a literal job name (including a bounded, single-axis literal matrix)
/// and a direct needs edge. Dynamic names, reusable jobs and ambiguous matches
/// authorize nothing. Validate the entire DAG, including unrelated edges.
pub(super) fn dependency(
    source: &str,
    context: &str,
    failed_name: &str,
) -> Result<Dependency, OrbitError> {
    let workflow: Value = serde_yaml::from_str(source)
        .map_err(|error| unreadable(&format!("workflow source: {error}")))?;
    let jobs = workflow["jobs"]
        .as_object()
        .filter(|jobs| !jobs.is_empty() && jobs.len() <= 100)
        .ok_or_else(|| unreadable("workflow jobs"))?;
    let mut edges = BTreeMap::new();
    let mut names = BTreeMap::new();
    for (id, job) in jobs {
        if job.get("uses").is_some() {
            return Err(unreadable("reusable workflow dependency"));
        }
        let needs = match job.get("needs") {
            None => Vec::new(),
            Some(Value::String(need)) => vec![need.clone()],
            Some(Value::Array(needs)) => needs
                .iter()
                .map(|need| {
                    need.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| unreadable("dynamic needs"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(unreadable("dynamic needs")),
        };
        if needs
            .iter()
            .any(|need| !jobs.contains_key(need) || need == id)
            || needs.len() > jobs.len()
            || needs.iter().collect::<BTreeSet<_>>().len() != needs.len()
        {
            return Err(unreadable("unresolved workflow dependency"));
        }
        edges.insert(id.clone(), needs);
        names.insert(id.clone(), job_names(id, job)?);
    }
    // Kahn's global check consumes each accepted node and edge once. Shared
    // prerequisites never cause root-to-leaf path enumeration.
    let mut pending = edges
        .iter()
        .map(|(id, needs)| (id, needs.len()))
        .collect::<BTreeMap<_, _>>();
    let mut dependents: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
    for (id, needs) in &edges {
        for need in needs {
            super::super::completion_deadline::probe("workflow edge", 0);
            dependents.entry(need).or_default().push(id);
        }
    }
    let mut ready = pending
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        super::super::completion_deadline::probe("workflow node", 0);
        visited += 1;
        for dependent in dependents.get(id).into_iter().flatten() {
            super::super::completion_deadline::probe("workflow edge", 0);
            if let Some(count) = pending.get_mut(dependent) {
                *count -= 1;
                if *count == 0 {
                    ready.push(dependent);
                }
            }
        }
    }
    if visited != jobs.len() {
        return Err(unreadable("cyclic workflow dependency"));
    }
    let matches = names
        .iter()
        .filter(|(_, names)| names.iter().any(|name| name == context))
        .collect::<Vec<_>>();
    let [(job, _)] = matches.as_slice() else {
        return Err(unreadable("missing check has no unique source job"));
    };
    let prerequisites = edges[*job]
        .iter()
        .filter(|id| names[*id].iter().any(|name| name == failed_name))
        .collect::<Vec<_>>();
    let [prerequisite] = prerequisites.as_slice() else {
        return Err(unreadable(
            "missing check has no unique failed prerequisite",
        ));
    };
    if names[*prerequisite].len() != 1 || !edges[*prerequisite].is_empty() {
        return Err(unreadable("ambiguous prerequisite job"));
    }
    if let Some(condition) = jobs[*job].get("if") {
        let condition = condition
            .as_str()
            .ok_or_else(|| unreadable("dynamic job condition"))?;
        if !condition.contains(&format!("needs.{prerequisite}.outputs."))
            || condition.contains("always()")
        {
            return Err(unreadable(
                "job condition does not depend on the failed prerequisite",
            ));
        }
    }
    let job = (*job).clone();
    let prerequisite = (*prerequisite).clone();
    Ok(Dependency {
        job,
        prerequisite,
        workflow,
        edges,
        names,
    })
}

fn job_names(id: &str, job: &Value) -> Result<Vec<String>, OrbitError> {
    let name = match job.get("name") {
        None => id,
        Some(Value::String(name)) => name,
        _ => return Err(unreadable("dynamic job name")),
    };
    let Some(strategy) = job.get("strategy") else {
        if name.contains("${{") {
            return Err(unreadable("dynamic job name"));
        }
        return Ok(vec![name.into()]);
    };
    let matrix = strategy["matrix"]
        .as_object()
        .filter(|matrix| matrix.len() == 1)
        .ok_or_else(|| unreadable("unsupported workflow matrix"))?;
    let (axis, values) = matrix
        .iter()
        .next()
        .ok_or_else(|| unreadable("empty workflow matrix"))?;
    if matches!(axis.as_str(), "include" | "exclude") {
        return Err(unreadable("unsupported workflow matrix"));
    }
    let values = values
        .as_array()
        .filter(|values| !values.is_empty() && values.len() <= 16)
        .ok_or_else(|| unreadable("dynamic workflow matrix"))?;
    let token = format!("${{{{ matrix.{axis} }}}}");
    if !name.contains(&token) {
        return Err(unreadable("matrix check name is ambiguous"));
    }
    values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .filter(|value| !value.contains("${{"))
                .ok_or_else(|| unreadable("dynamic matrix value"))?;
            let name = name.replace(&token, value);
            if name.contains("${{") {
                return Err(unreadable("dynamic matrix check name"));
            }
            Ok(name)
        })
        .collect()
}

/// Certify the actual --failed scope, not just the failure diagnosed above.
/// The complete provider table must cover the source, including skipped
/// unexpanded matrices. Every failed job and every transitive dependent is
/// selected regardless of its previous success or its runtime condition.
pub(super) fn certify_rerun(
    dependency: &Dependency,
    provider: &[Value],
    required: &[RequiredGate],
    app_id: u64,
) -> Result<(), OrbitError> {
    let jobs = dependency.workflow["jobs"]
        .as_object()
        .ok_or_else(|| unreadable("workflow jobs"))?;
    let mut bound: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    let mut provider_names = BTreeSet::new();
    for observed in provider {
        let name = observed["name"]
            .as_str()
            .ok_or_else(|| unreadable("provider job name"))?;
        if !provider_names.insert(name) {
            return Err(unreadable("duplicate provider job"));
        }
        let matches = jobs
            .iter()
            .filter(|(id, job)| {
                dependency.names[*id]
                    .iter()
                    .any(|candidate| candidate == name)
                    || (observed["conclusion"] == "skipped"
                        && observed["steps"].as_array().is_some_and(Vec::is_empty)
                        && job.get("strategy").is_some()
                        && job["name"].as_str() == Some(name))
            })
            .collect::<Vec<_>>();
        let [(id, _)] = matches.as_slice() else {
            return Err(unreadable("provider job has no unique source"));
        };
        bound.entry(id.as_str()).or_default().push(observed);
    }
    for (id, job) in jobs {
        let rows = bound
            .get(id.as_str())
            .ok_or_else(|| unreadable("incomplete source job coverage"))?;
        let unexpanded = rows.len() == 1
            && job.get("strategy").is_some()
            && rows[0]["conclusion"] == "skipped"
            && rows[0]["steps"].as_array().is_some_and(Vec::is_empty)
            && rows[0]["name"] == job["name"];
        if !unexpanded && rows.len() != dependency.names[id].len() {
            return Err(unreadable("incomplete source matrix coverage"));
        }
    }
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, needs) in &dependency.edges {
        for need in needs {
            super::super::completion_deadline::probe("selection edge", 0);
            dependents.entry(need).or_default().push(id);
        }
    }
    let mut selected = BTreeSet::new();
    let mut pending = Vec::new();
    for (id, rows) in &bound {
        if rows.iter().any(|row| row["conclusion"] == "failure") && selected.insert(*id) {
            pending.push(*id);
        }
    }
    while let Some(id) = pending.pop() {
        super::super::completion_deadline::probe("selection node", 0);
        for next in dependents.get(id).into_iter().flatten() {
            super::super::completion_deadline::probe("selection edge", 0);
            if selected.insert(*next) {
                pending.push(*next);
            }
        }
    }
    for id in selected {
        let job = &jobs[id];
        let rows = &bound[id];
        if rows.iter().all(|row| {
            row["conclusion"] == "skipped" && row["steps"].as_array().is_some_and(Vec::is_empty)
        }) {
            // First execution can use the unchanged protected CI job's
            // captured check authority, including result publication and
            // action pre/post effects. A pin/name alone never grants it.
            if required.iter().any(|gate| {
                gate.app_id.is_none_or(|app| app == app_id)
                    && dependency.names[id].contains(&gate.context)
            }) {
                fresh_check(&dependency.workflow, job)?;
            } else {
                certify_read_job(&dependency.workflow, id, job)?;
            }
        } else {
            certify_read_job(&dependency.workflow, id, job)?;
            for row in rows {
                certify_steps(job, row)?;
            }
        }
    }
    Ok(())
}

fn certify_read_job(workflow: &Value, id: &str, job: &Value) -> Result<(), OrbitError> {
    let name = job["steps"]
        .as_array()
        .and_then(|steps| steps.first())
        .and_then(|step| step["name"].as_str())
        .ok_or_else(|| unreadable("replayed source step identity"))?;
    super::workflow_read::api_step(workflow, id, name)
}

fn certify_steps(job: &Value, observed: &Value) -> Result<(), OrbitError> {
    let source = job["steps"]
        .as_array()
        .filter(|steps| !steps.is_empty() && steps.len() <= 100)
        .ok_or_else(|| unreadable("source step bound"))?;
    let steps = observed["steps"]
        .as_array()
        .filter(|steps| steps.len() <= 102)
        .ok_or_else(|| unreadable("provider step bound"))?;
    let mut index = 0;
    let mut number = 0;
    let mut failed = 0;
    let mut setup = false;
    let mut complete = false;
    for step in steps {
        let next = step["number"]
            .as_u64()
            .ok_or_else(|| unreadable("provider step number"))?;
        if next != number + 1
            || step["status"] != "completed"
            || !matches!(
                step["conclusion"].as_str(),
                Some("success" | "failure" | "skipped")
            )
        {
            return Err(unreadable("unknown provider step outcome"));
        }
        number = next;
        if step["name"] == "Set up job" && index == 0 && next == 1
            || step["name"] == "Complete job" && index == source.len()
        {
            if step["conclusion"] != "success" {
                return Err(unreadable("unsafe runner lifecycle"));
            }
            if step["name"] == "Set up job" {
                if setup {
                    return Err(unreadable("duplicate runner setup"));
                }
                setup = true;
            } else {
                if complete {
                    return Err(unreadable("duplicate runner completion"));
                }
                complete = true;
            }
            continue;
        }
        if source.get(index).and_then(|step| step["name"].as_str()) != step["name"].as_str()
            || step["name"].as_str().is_none()
        {
            return Err(unreadable("unknown source/action/pre/post step"));
        }
        index += 1;
        failed += usize::from(step["conclusion"] == "failure");
    }
    if index != source.len() || failed != usize::from(observed["conclusion"] == "failure") {
        return Err(unreadable("incomplete provider source steps"));
    }
    Ok(())
}

fn fresh_check(workflow: &Value, job: &Value) -> Result<(), OrbitError> {
    for env in [workflow.get("env"), job.get("env")].into_iter().flatten() {
        super::workflow_read::safe_environment(env)?;
    }
    if job.get("environment").is_some()
        || job.get("container").is_some()
        || job.get("services").is_some()
        || job.get("uses").is_some()
        || !matches!(
            job["runs-on"].as_str(),
            Some(
                "ubuntu-latest"
                    | "ubuntu-22.04"
                    | "ubuntu-24.04"
                    | "macos-latest"
                    | "windows-latest"
            )
        )
    {
        return Err(unreadable("unsupported fresh check environment"));
    }
    let permissions = job
        .get("permissions")
        .or_else(|| workflow.get("permissions"))
        .and_then(Value::as_object)
        .ok_or_else(|| unreadable("uncaptured check credentials"))?;
    if permissions.iter().any(|(scope, permission)| {
        !matches!(permission.as_str(), Some("read" | "none"))
            && !(matches!(scope.as_str(), "checks" | "statuses" | "security-events")
                && permission == "write")
    }) {
        return Err(unreadable("fresh check has non-check write authority"));
    }
    // Repository secrets beyond the original check token can authorize an
    // unrelated deployment/issue effect. The exact immutable source authorizes
    // first execution only under captured read/check-result token privileges.
    fn check_values(value: &Value) -> bool {
        match value {
            Value::String(value) => {
                let value = value.replace("${{ secrets.GITHUB_TOKEN }}", "");
                !value.contains("secrets")
            }
            Value::Array(values) => values.iter().all(check_values),
            Value::Object(values) => values.values().all(check_values),
            _ => true,
        }
    }
    if !check_values(job) || workflow.get("env").is_some_and(|env| !check_values(env)) {
        return Err(unreadable("uncaptured fresh check secret"));
    }
    let steps = job["steps"]
        .as_array()
        .filter(|steps| !steps.is_empty() && steps.len() <= 100)
        .ok_or_else(|| unreadable("fresh check source steps"))?;
    for step in steps {
        if let Some(env) = step.get("env") {
            super::workflow_read::safe_environment(env)?;
        }
        match (step["run"].as_str(), step["uses"].as_str()) {
            (Some(_), None) => {}
            (None, Some(action))
                if action.rsplit_once('@').is_some_and(|(name, revision)| {
                    !name.contains("${{")
                        && revision.len() == 40
                        && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) => {}
            _ => return Err(unreadable("unsupported fresh check action")),
        }
    }
    Ok(())
}
