//! Prove the validation retry that advanced the synchronized implementation.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::release;
use orbit_types::task::{ArtifactManifestFileV2, ArtifactWriter};
use orbit_types::workflow::handoff::HandoffArtifactRef;
use orbit_types::workflow::{
    JobV2, PipelineState, STEP_FAILURE_RECOVERY_ACTIVITY, TrailerKey, collect_trailers,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::resume::source_repair::{field, observation, refuse};

/// Owner binding for the validation repair proof.
pub(in crate::application::job::resume::source_repair) struct ValidationEvidence<'a> {
    pub(in crate::application::job::resume::source_repair) runtime: &'a OrbitRuntime,
    pub(in crate::application::job::resume::source_repair) run_id: &'a str,
    pub(in crate::application::job::resume::source_repair) task_id: &'a str,
    pub(in crate::application::job::resume::source_repair) base: &'a str,
    pub(in crate::application::job::resume::source_repair) branch: &'a str,
}

impl ValidationEvidence<'_> {
    /// Resolve the synchronized implementation, or authenticate its validation repair.
    pub(in crate::application::job::resume::source_repair) fn implementation_head<'a>(
        &self,
        job: &JobV2,
        state: &'a PipelineState,
        checkpoint: &impl Fn(&str, &str) -> Result<&'a Value, OrbitError>,
        (synced, pushed): (&'a str, &'a str),
    ) -> Result<&'a str, OrbitError> {
        // A before-PR correction has its own provenance. Its settlement must
        // start from the validated implementation, which may itself be a repair.
        let settle = &state.pipeline["review_gate_settle"];
        let implementation = if settle["gate"] == "passed" {
            field(settle, "implementation_head_sha")?
        } else {
            pushed
        };
        if implementation == synced {
            return Ok(synced);
        }
        let repo = &self.runtime.paths().repo_root;
        if !observation::is_exact_sha(implementation)
            || !repair_commit_matches(repo, implementation, synced, self.run_id, self.task_id)?
        {
            return Err(refuse(
                "initial publication lacks authenticated review provenance or validation repair",
            ));
        }

        let mut steps = job.steps.iter();
        if steps.find(|step| step.id == "sync_base").is_none()
            || !steps
                .by_ref()
                .take_while(|step| !matches!(step.id.as_str(), "review_gate_settle" | "push"))
                .any(|step| {
                    step.id == "validate"
                        && step.recovery_activity.as_deref() == Some(STEP_FAILURE_RECOVERY_ACTIVITY)
                })
        {
            return Err(refuse("publication lacks the validation recovery step"));
        }
        let validation = checkpoint("validate", "candidate_validate")?;
        if validation["phase"] != "validate"
            || !matches!(
                validation["decision"].as_str(),
                Some("passed" | "passed_with_known_red" | "passed_with_flaky_tests")
            )
            || validation["tested_head"] != implementation
            || validation["base_sha"] != self.base
            || validation["branch"] != self.branch
        {
            return Err(refuse("validation retry does not bind the repaired head"));
        }

        self.authenticate_retry(validation, synced, implementation)?;
        Ok(implementation)
    }

    fn authenticate_retry(
        &self,
        validation: &Value,
        synced: &str,
        implementation: &str,
    ) -> Result<(), OrbitError> {
        let manifest = self.runtime.get_task_artifact_manifest(self.task_id)?;
        let references: Vec<HandoffArtifactRef> =
            serde_json::from_value(validation["validation"].clone())
                .map_err(|_| refuse("validation retry lacks host receipts"))?;
        let prefix = format!("validation/{}/", self.run_id);
        let archive_prefix = format!("{prefix}attempts/");
        let mut commands = BTreeSet::new();
        let mut passed = Vec::new();
        let mut known_red = 0;
        let mut index = 0;
        for reference in references {
            let Some(name) = reference.path.strip_prefix(&prefix) else {
                return Err(refuse("validation retry references a foreign run"));
            };
            let candidate = name == format!("{index}.json");
            if !candidate
                && name != format!("{index}.rerun.json")
                && (index == 0 || name != format!("{}.baseline.json", index - 1))
            {
                return Err(refuse("validation retry receipts are out of order"));
            }
            // A subsequent validation in this run may have overwritten the
            // canonical receipt. The producer archives the exact old bytes.
            let archived = format!("{archive_prefix}{}.json", reference.sha256);
            let entry = manifest.iter().find(|entry| {
                entry.writer == Some(ArtifactWriter::System)
                    && entry.sha256 == reference.sha256
                    && (entry.path == reference.path || entry.path == archived)
            });
            let entry =
                entry.ok_or_else(|| refuse("validation retry receipt lacks host provenance"))?;
            if !candidate {
                continue;
            }
            let log = self.read_log(entry)?;
            if !self.binds(&log, implementation)
                || !log["missing_tool"].is_null()
                || !log["deferred_notice"].is_null()
                || !log["host_output_refusal"].is_null()
                || (validation["decision"] == "passed"
                    && (log["exit_code"] != 0 || log["timed_out"] != false))
            {
                return Err(refuse(
                    "validation retry receipt disagrees with its checkpoint",
                ));
            }
            let command = field(&log, "command")?;
            commands.insert(command.to_owned());
            if log["known_red"].is_null() {
                passed.push(command.to_owned());
            } else {
                known_red += 1;
            }
            index += 1;
        }
        if commands.is_empty() {
            return Err(refuse("validation retry lacks host receipts"));
        }
        if validation["commands"] != json!(passed)
            || validation["known_red"].as_array().map(Vec::len) != Some(known_red)
        {
            return Err(refuse("validation retry command receipts are incomplete"));
        }
        // Trailers identify a repair, but the host's preserved failure at its
        // parent is what connects the Git transition to an actual retry.
        for entry in &manifest {
            if entry.writer != Some(ArtifactWriter::System)
                || entry.path != format!("{archive_prefix}{}.json", entry.sha256)
            {
                continue;
            }
            let log = self.read_log(entry)?;
            if self.binds(&log, synced)
                && log["command"]
                    .as_str()
                    .is_some_and(|cmd| commands.contains(cmd))
                && (log["timed_out"] == true
                    || log["exit_code"].as_i64().is_some_and(|code| code != 0))
            {
                return Ok(());
            }
        }
        Err(refuse(
            "validation repair lacks a failed host attempt on its parent",
        ))
    }

    fn binds(&self, log: &Value, head: &str) -> bool {
        log["schema_version"] == 1
            && log["run_id"] == self.run_id
            && log["task_ids"] == json!([self.task_id])
            && log["base_sha"] == self.base
            && log["branch"] == self.branch
            && log["tested_head"] == head
    }

    fn read_log(&self, entry: &ArtifactManifestFileV2) -> Result<Value, OrbitError> {
        let artifact = self
            .runtime
            .observe_task_artifact(self.task_id, &entry.path)?
            .ok_or_else(|| refuse("validation repair log is missing"))?;
        if release::sha256_hex(&artifact.content) != entry.sha256 {
            return Err(refuse("validation repair log digest disagrees"));
        }
        serde_json::from_slice(&artifact.content)
            .map_err(|_| refuse("validation repair log is invalid"))
    }
}

fn repair_commit_matches(
    repo: &Path,
    repaired: &str,
    parent: &str,
    run_id: &str,
    task_id: &str,
) -> Result<bool, OrbitError> {
    // Read the actual object and parents by OID, without Git replacements.
    let repo = git2::Repository::open(repo).map_err(|_| refuse("source repository unavailable"))?;
    let commit = git2::Oid::from_str(repaired)
        .and_then(|oid| repo.find_commit(oid))
        .map_err(|_| refuse("publication commit object unavailable"))?;
    if !commit.parent_ids().map(|oid| oid.to_string()).eq([parent]) {
        return Ok(false);
    }
    let Ok(message) = commit.message_raw() else {
        return Ok(false);
    };
    let trailers = collect_trailers(message);
    Ok([
        (TrailerKey::OrbitTask, task_id),
        (TrailerKey::OrbitRecoveryRun, run_id),
        (TrailerKey::OrbitRecoveryStep, "validate"),
        (
            TrailerKey::OrbitRecoveryActivity,
            STEP_FAILURE_RECOVERY_ACTIVITY,
        ),
    ]
    .into_iter()
    .all(|(key, expected)| {
        trailers
            .iter()
            .filter(|trailer| trailer.key == key)
            .map(|trailer| trailer.value.as_str())
            .eq([expected])
    }))
}
