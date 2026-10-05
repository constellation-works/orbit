//! The operating system a failing job ran on, and the `os:` tag it routes the
//! filed repair to.
//!
//! A macOS-only failure filed into a Linux owner's backlog blocks: the Linux
//! worker cannot reproduce it. So the sweep records the failing job's runner as
//! evidence and tags the repair `os:macos` or `os:linux`, which admission then
//! honours. The runner's labels from the jobs API are the evidence; when the
//! snapshot has none, the workflow's own `runs-on` in this checkout stands in.
//! A Windows or unrecognised runner gets no tag: there is no Windows host to
//! hand the task to, and a task no host can claim would be worse than today.

use std::path::Path;

use orbit_types::task::HostOs;
use serde_json::{Value, json};

use super::fields::value_string;

/// Where a run's runner OS came from.
const SOURCE_JOB_LABELS: &str = "job_labels";
const SOURCE_WORKFLOW_RUNS_ON: &str = "workflow_runs_on";
const SOURCE_UNKNOWN: &str = "unknown";

/// The runner OS of one failure row, as recorded evidence:
/// `{"os", "source", "labels"}`. `os` is null when neither source names one OS.
pub(super) fn failure_runner_os(failure: &Value, repo_root: &Path) -> Value {
    let job = &failure["failed_jobs"][0];
    let labels = string_list(&job["runner_labels"]);
    if !labels.is_empty() {
        return json!({
            "os": os_from_labels(&labels),
            "source": SOURCE_JOB_LABELS,
            "labels": labels,
        });
    }
    let workflow = value_string(failure, "workflow");
    let job_name = value_string(job, "name");
    if let Some(labels) = workflow_runs_on(repo_root, &workflow, &job_name) {
        return json!({
            "os": os_from_labels(&labels),
            "source": SOURCE_WORKFLOW_RUNS_ON,
            "labels": labels,
        });
    }
    json!({"os": null, "source": SOURCE_UNKNOWN, "labels": []})
}

/// The `os:` tags for a repair whose runs ran on `runners`: the union of their
/// OSes when every run's is Linux or macOS, and none otherwise. One run on
/// Windows or an unknown runner leaves the repair untagged, admissible
/// anywhere, rather than routed away from the platform that failed.
pub(super) fn os_tags(runners: &[Value]) -> Vec<String> {
    let mut oses = std::collections::BTreeSet::new();
    for runner in runners {
        match runner["os"].as_str().and_then(HostOs::parse) {
            Some(os @ (HostOs::Linux | HostOs::Macos)) => {
                oses.insert(os);
            }
            _ => return Vec::new(),
        }
    }
    oses.into_iter().map(HostOs::tag).collect()
}

/// The one OS a runner's labels name: `macos-*` is macOS, `ubuntu-*` or
/// `linux` is Linux, `windows-*` is Windows. Labels naming none, or naming two,
/// say nothing.
fn os_from_labels(labels: &[String]) -> Option<&'static str> {
    let mut found = labels.iter().filter_map(|label| label_os(label));
    let first = found.next()?;
    found.all(|other| other == first).then_some(first.as_str())
}

fn label_os(label: &str) -> Option<HostOs> {
    let label = label.trim().to_ascii_lowercase();
    let family = |name: &str| label == name || label.starts_with(&format!("{name}-"));
    if family("macos") {
        Some(HostOs::Macos)
    } else if family("ubuntu") || family("linux") {
        Some(HostOs::Linux)
    } else if family("windows") {
        Some(HostOs::Windows)
    } else {
        None
    }
}

/// The literal `runs-on` labels of `job` in `workflow`, read from this
/// checkout's `.github/workflows`. A workflow matches by its `name` (GitHub
/// shows the file path for an unnamed one), a job by its `name` or key. An
/// expression such as `${{ matrix.os }}` says nothing until it is evaluated,
/// so it yields `None`.
fn workflow_runs_on(repo_root: &Path, workflow: &str, job: &str) -> Option<Vec<String>> {
    if workflow.is_empty() || job.is_empty() {
        return None;
    }
    let directory = repo_root.join(".github/workflows");
    let mut entries = std::fs::read_dir(&directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yml" || extension == "yaml")
        })
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(document) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        let relative = path
            .strip_prefix(repo_root)
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = document["name"].as_str().unwrap_or(&relative);
        if name != workflow {
            continue;
        }
        let jobs = document["jobs"].as_mapping()?;
        let spec = jobs.iter().find_map(|(key, spec)| {
            let display = spec["name"].as_str().or_else(|| key.as_str())?;
            (display == job || key.as_str() == Some(job)).then_some(spec)
        })?;
        let runs_on = &spec["runs-on"];
        let labels = match runs_on {
            serde_yaml::Value::String(label) => vec![label.clone()],
            serde_yaml::Value::Sequence(_) => yaml_strings(runs_on),
            serde_yaml::Value::Mapping(_) => yaml_strings(&runs_on["labels"]),
            _ => Vec::new(),
        };
        return (!labels.is_empty() && labels.iter().all(|label| !label.contains("${{")))
            .then_some(labels);
    }
    None
}

fn yaml_strings(value: &serde_yaml::Value) -> Vec<String> {
    match value {
        serde_yaml::Value::String(label) => vec![label.clone()],
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}
