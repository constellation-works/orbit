//! Import bounded claimed implementation evidence before the commit boundary.
use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::canonical_artifact_path;
use orbit_types::workflow::handoff::{AlreadyLandedEvidence, NoDiffEvidence};
use orbit_types::workflow::is_reserved_review_artifact;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::input::refused;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScratchArtifact {
    path: String,
    source_path: String,
}

type PreparedArtifact = (String, Vec<u8>);

fn prepare(
    workspace: &Path,
    task: &str,
    input: &Value,
    field: &str,
) -> Result<Vec<PreparedArtifact>, OrbitError> {
    let Some(value) = input
        .get("implementation")
        .and_then(|output| output.get(field))
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let artifacts: Vec<ScratchArtifact> = serde_json::from_value(value.clone())
        .map_err(|error| refused(format!("invalid {field}: {error}")))?;
    let scratch = workspace.join(".orbit/tmp");
    let mut paths = BTreeSet::new();
    let mut prepared = Vec::new();
    for artifact in artifacts {
        let path = canonical_artifact_path(&artifact.path)
            .map_err(|error| refused(format!("{field} path '{}': {error}", artifact.path)))?;
        if is_reserved_review_artifact(&path) {
            return Err(refused(format!(
                "{field} path '{path}' is in the reserved review- namespace, which only the review gate writes"
            )));
        }
        if !paths.insert(path.clone()) {
            return Err(refused(format!("{field} contains duplicate path '{path}'")));
        }
        let payload = orbit_tools::prepare_remote_task_artifact_put(
            json!({"id": task, "path": path, "source_path": artifact.source_path}),
            Some(workspace),
            Some(&scratch),
            None,
        )
        .map_err(|error| {
            refused(format!(
                "{field} artifact '{path}' from '{}': {error}",
                artifact.source_path
            ))
        })?;
        let bytes: Vec<u8> = serde_json::from_value(payload["artifacts"][0]["content"].clone())
            .map_err(|error| refused(format!("invalid prepared artifact '{path}': {error}")))?;
        prepared.push((path, bytes));
    }
    Ok(prepared)
}

/// A sandboxed implementer returns scratch references, never owner paths.
/// Validate both lists completely before attaching any bytes through the claim.
/// Clean-tree evidence remains restricted to reports and their declared logs;
/// ordinary task artifacts cannot overwrite those reserved names. Neither list
/// may name the review gate's `review-` namespace, which the owner refuses
/// only after earlier entries are already stored.
pub(in crate::executor::automation::vcs) fn import_implementation_evidence<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    task_id: &str,
    workspace: &Path,
    input: &Value,
) -> Result<(), OrbitError> {
    let evidence = prepare(workspace, task_id, input, "no_diff_artifacts")?;
    let artifacts = prepare(workspace, task_id, input, "task_artifacts")?;
    let mut permitted = BTreeSet::new();
    for (path, bytes) in &evidence {
        let logs = match path.as_str() {
            "no-diff.json" => {
                let report: NoDiffEvidence = serde_json::from_slice(bytes)
                    .map_err(|error| refused(format!("invalid {path}: {error}")))?;
                report
                    .validation
                    .into_iter()
                    .map(|check| check.log_artifact)
                    .collect::<Vec<_>>()
            }
            "already-landed.json" => {
                let report: AlreadyLandedEvidence = serde_json::from_slice(bytes)
                    .map_err(|error| refused(format!("invalid {path}: {error}")))?;
                report
                    .validation
                    .into_iter()
                    .map(|check| check.log_artifact)
                    .collect::<Vec<_>>()
            }
            _ => continue,
        };
        permitted.insert(path.clone());
        for log in logs {
            permitted.insert(
                canonical_artifact_path(&log)
                    .map_err(|error| refused(format!("invalid {path} log '{log}': {error}")))?,
            );
        }
    }
    for (path, _) in &evidence {
        if !permitted.contains(path) {
            return Err(refused(format!(
                "no_diff_artifacts path '{path}' is not a clean-tree report or one of its declared validation logs"
            )));
        }
    }
    // These engine-owned names are written after commit, so checking only
    // the implementer's report would let later validation overwrite evidence.
    let reserved = if artifacts.is_empty() {
        None
    } else {
        let context = host.claim_execution_context()?;
        Some((
            format!("no-diff-handoff/{}.json", context.claim_id),
            format!("validation/{}/", context.claim_id),
        ))
    };
    for (path, _) in &artifacts {
        if matches!(path.as_str(), "no-diff.json" | "already-landed.json")
            || permitted.contains(path)
            || reserved
                .as_ref()
                .is_some_and(|(report, logs)| path == report || path.starts_with(logs))
        {
            return Err(refused(format!(
                "task_artifacts path '{path}' collides with a clean-tree report or its declared validation log"
            )));
        }
    }
    for (path, bytes) in evidence.into_iter().chain(artifacts) {
        host.attach_claim_validation_log(&path, bytes)?;
    }
    Ok(())
}
