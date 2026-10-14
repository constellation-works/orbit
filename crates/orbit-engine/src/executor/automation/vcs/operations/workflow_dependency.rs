//! Static, bounded workflow dependency evidence. Expressions are never executed.
use orbit_common::OrbitError;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use super::prerequisite::unreadable;

pub(super) struct Dependency {
    pub job: String,
    pub prerequisite: String,
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
        {
            return Err(unreadable("unresolved workflow dependency"));
        }
        edges.insert(id.clone(), needs);
        names.insert(id.clone(), job_names(id, job)?);
    }
    for id in jobs.keys() {
        let mut stack = vec![(id.as_str(), BTreeSet::new())];
        while let Some((current, mut path)) = stack.pop() {
            if !path.insert(current) {
                return Err(unreadable("cyclic workflow dependency"));
            }
            for need in &edges[current] {
                stack.push((need, path.clone()));
            }
            if stack.len() > 100 {
                return Err(unreadable("workflow DAG exceeds bound"));
            }
        }
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
    Ok(Dependency {
        job: (*job).clone(),
        prerequisite: (*prerequisite).clone(),
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

/// Bind the failed API step to the prerequisite's source, not arbitrary log
/// text from an unrelated job/step. Only a single literal REST read in a
/// supported shell qualifies; this deliberately does not interpret scripts.
pub(super) fn api_step(
    source: &str,
    prerequisite: &str,
    failed_step: &str,
) -> Result<(), OrbitError> {
    let workflow: Value =
        serde_yaml::from_str(source).map_err(|_| unreadable("workflow source"))?;
    let steps = workflow["jobs"][prerequisite]["steps"]
        .as_array()
        .ok_or_else(|| unreadable("prerequisite source steps"))?;
    let matching = steps
        .iter()
        .filter(|step| step["name"].as_str() == Some(failed_step))
        .collect::<Vec<_>>();
    let [step] = matching.as_slice() else {
        return Err(unreadable("failed step has no unique source command"));
    };
    let command = step["run"]
        .as_str()
        .ok_or_else(|| unreadable("failed step has no source command"))?;
    let shell = [
        step.get("shell"),
        workflow["jobs"][prerequisite].pointer("/defaults/run/shell"),
        workflow.pointer("/defaults/run/shell"),
    ]
    .into_iter()
    .flatten()
    .next();
    if step.get("uses").is_some()
        || shell.is_some_and(|shell| !matches!(shell.as_str(), Some("bash" | "sh")))
        || !literal_api_read(command)
    {
        return Err(unreadable("failed step is not a source-bound API read"));
    }
    Ok(())
}

/// A bounded allowlist rather than shell tokenization: quoting, expansion,
/// comments, redirection and composition all hold. Unknown gh options also
/// hold, including field/input flags that silently change GET to POST.
fn literal_api_read(command: &str) -> bool {
    let command = command.trim_matches([' ', '\t', '\r', '\n']);
    if !command
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b" \t/._-{}=".contains(&byte))
    {
        return false;
    }
    let mut words = command.split_ascii_whitespace();
    if words.next() != Some("gh") || words.next() != Some("api") {
        return false;
    }
    let mut endpoint = false;
    let mut method = false;
    while let Some(word) = words.next() {
        match word {
            "--method" | "-X" if !method => {
                if words.next() != Some("GET") {
                    return false;
                }
                method = true;
            }
            "--method=GET" | "-XGET" if !method => method = true,
            _ => {
                // Repository REST paths are supported. In particular, `graphql`
                // defaults to POST even without fields or an explicit method.
                if endpoint
                    || !word.starts_with("repos/")
                    || word.len() <= "repos/".len()
                    || word.contains('=')
                {
                    return false;
                }
                endpoint = true;
            }
        }
    }
    endpoint
}
