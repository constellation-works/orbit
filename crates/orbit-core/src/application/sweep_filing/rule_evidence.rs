//! Bounded evidence reads for the generic rule-results filer.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};

use orbit_common::OrbitError;
use orbit_types::task::{TaskArtifact, media_type_for_artifact_path};
use orbit_types::workflow::activity_job::{ScriptOutputFile, ScriptOutputSet};
use orbit_types::workflow::{ActivityV2Spec, JobRunState, JobV2StepBody};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::application::job::script_outputs;

const MAX_FILES: usize = 64;
const MAX_BYTES: u64 = 1024 * 1024;

fn invalid(detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "rule evidence: {detail}; regenerate evidence with declared script outputs before filing"
    ))
}

fn producer_set(
    runtime: &OrbitRuntime,
    input: &Value,
    document: &Value,
    consumer_run_id: Option<&str>,
) -> Result<Option<ScriptOutputSet>, OrbitError> {
    let Some(producer) = input.get("evidence_step") else {
        return Ok(None);
    };
    let producer = producer
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("evidence_step must name a direct preceding script step"))?;
    let run_id = consumer_run_id
        .ok_or_else(|| invalid("evidence_step needs a host-attested consumer run"))?;
    let state = runtime
        .read_run_state(run_id)?
        .ok_or_else(|| invalid("consumer checkpoint is missing"))?;
    let (job, _) = runtime
        .read_run_definition_snapshot(run_id)?
        .ok_or_else(|| invalid("executing job definition snapshot is missing"))?;
    let matches: Vec<_> = job
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.id == producer)
        .collect();
    if matches.len() != 1
        || job
            .step_stages()
            .iter()
            .filter(|(id, _)| *id == producer)
            .count()
            != 1
    {
        return Err(invalid(
            "evidence_step must identify one unique direct script step",
        ));
    }
    let (index, step) = matches[0];
    if !matches!(&step.body, JobV2StepBody::TargetRef(target) if target.target.starts_with("script:"))
    {
        return Err(invalid("evidence producer is not a direct script step"));
    }
    let current = state.next_step_index as usize;
    let is_filer = job.steps.get(current).is_some_and(|step| match &step.body {
        JobV2StepBody::TargetRef(target) => target.target == "activity:file_rule_findings",
        JobV2StepBody::Target(target) => matches!(&target.spec, ActivityV2Spec::Deterministic(spec) if spec.action == "file_rule_findings"),
        _ => false,
    });
    if index >= current || !is_filer {
        return Err(invalid("producer must precede this direct filer step"));
    }
    let index = index as u32;
    if state.step_states.get(&index) != Some(&JobRunState::Success)
        || state.step_output(index) != Some(document)
    {
        return Err(invalid(
            "results do not match the successful producer checkpoint",
        ));
    }
    let set = state
        .script_output_sets
        .get(&index)
        .and_then(|sets| sets.get(producer))
        .ok_or_else(|| invalid("producer output set was not persisted"))?;
    if set.step_id != producer
        || set.files.len() > MAX_FILES
        || set.files.iter().any(|file| file.bytes > MAX_BYTES)
    {
        return Err(invalid(
            "producer identity or evidence budget exceeds 64 files of 1 MiB",
        ));
    }
    let supplied: Vec<ScriptOutputFile> =
        serde_json::from_value(input.get("evidence_files").cloned().unwrap_or(Value::Null))
            .map_err(|error| {
                invalid(format!(
                    "evidence_files must bind the producer files: {error}"
                ))
            })?;
    if supplied != set.files {
        return Err(invalid(
            "evidence_files do not match the bound producer output set",
        ));
    }
    script_outputs::verify_script_output_set(runtime, run_id, set).map_err(invalid)?;
    Ok(Some(set.clone()))
}

/// Read all named evidence before the filer mutates tasks. Paths are names,
/// never authority to read host files. A script binding takes precedence over
/// the legacy workspace-confined evidence directory.
pub(super) fn load_rule_evidence(
    runtime: &OrbitRuntime,
    input: &Value,
    document: &Value,
    evidence_dir: Option<&Path>,
    consumer_run_id: Option<&str>,
) -> Result<BTreeMap<String, TaskArtifact>, OrbitError> {
    let set = producer_set(runtime, input, document, consumer_run_id)?;
    let mut artifacts = BTreeMap::new();
    for result in document
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(evidence) = result.get("evidence") else {
            continue;
        };
        let evidence = evidence
            .as_array()
            .ok_or_else(|| invalid("evidence must be an array of relative file names"))?;
        for name in evidence {
            let name = name
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid("evidence contains a malformed file name"))?;
            if artifacts.contains_key(name) {
                continue;
            }
            if artifacts.len() >= MAX_FILES {
                return Err(invalid("evidence exceeds 64 unique files; split the run"));
            }
            let relative = Path::new(name);
            if relative.is_absolute()
                || name.contains('\\')
                || relative
                    .components()
                    .any(|p| !matches!(p, Component::Normal(_)))
            {
                return Err(invalid(format!("unsafe evidence name {name:?}")));
            }
            let (file, expected) = if let Some(set) = &set {
                let member = set
                    .files
                    .iter()
                    .find(|file| file.name == name)
                    .ok_or_else(|| {
                        invalid(format!("{name:?} is not in the producer output set"))
                    })?;
                let root = &runtime.paths().state_dir;
                let relative = Path::new(&member.path)
                    .strip_prefix(root)
                    .map_err(invalid)?;
                (open_rule_evidence_beneath(root, relative)?, Some(member))
            } else {
                let dir = evidence_dir
                    .ok_or_else(|| invalid(format!("{name:?} has no evidence source")))?;
                let root = runtime.paths().repo_root.canonicalize().map_err(invalid)?;
                let dir = dir.canonicalize().map_err(invalid)?;
                let relative = dir
                    .strip_prefix(&root)
                    .map_err(|_| invalid("evidence directory escapes the workspace"))?
                    .join(relative);
                (open_rule_evidence_beneath(&root, &relative)?, None)
            };
            let metadata = file.metadata().map_err(invalid)?;
            if !metadata.is_file() || metadata.len() > MAX_BYTES {
                return Err(invalid(format!(
                    "{name:?} must be a regular file at most 1 MiB"
                )));
            }
            let mut content = Vec::new();
            file.take(MAX_BYTES + 1)
                .read_to_end(&mut content)
                .map_err(invalid)?;
            if content.len() as u64 > MAX_BYTES {
                return Err(invalid(format!("{name:?} exceeds 1 MiB")));
            }
            if let Some(expected) = expected {
                let digest = orbit_types::identity::lower_hex(Sha256::digest(&content));
                if content.len() as u64 != expected.bytes || digest != expected.sha256 {
                    return Err(invalid(format!(
                        "{name:?} changed after producer verification"
                    )));
                }
            }
            let mut artifact = TaskArtifact {
                path: name.to_string(),
                content,
                media_type: media_type_for_artifact_path(name).to_string(),
                created_by: Some("system".to_string()),
            };
            crate::application::task::admit_agent_artifact(&mut artifact)?;
            artifacts.insert(name.to_string(), artifact);
        }
    }
    Ok(artifacts)
}

pub(super) fn artifacts_for_result(
    evidence: &BTreeMap<String, TaskArtifact>,
    result: &Value,
) -> Vec<TaskArtifact> {
    result
        .get("evidence")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().and_then(|name| evidence.get(name)).cloned())
        .collect()
}

// Pin each directory before opening the next component. A pathname check
// followed by a final-component O_NOFOLLOW open would race parent symlinks.
#[cfg(unix)]
fn open_rule_evidence_beneath(root: &Path, relative: &Path) -> Result<std::fs::File, OrbitError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root)
        .map_err(invalid)?;
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Err(invalid("evidence needs a file name"));
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(invalid("unsafe evidence component"));
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(invalid)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if index + 1 < components.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: directory is an owned live descriptor, name is NUL-terminated,
        // and openat borrows both for this call, returning a new owned descriptor.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(invalid(format!(
                "cannot safely open evidence (symlinks are refused): {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: openat succeeded and ownership of its new descriptor transfers here.
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open_rule_evidence_beneath(_root: &Path, _relative: &Path) -> Result<std::fs::File, OrbitError> {
    Err(invalid(
        "safe directory-relative evidence reads are unavailable on this platform; use a Unix host",
    ))
}
