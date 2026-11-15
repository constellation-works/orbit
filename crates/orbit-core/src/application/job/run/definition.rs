//! The job definition a persisted run executes: where a submission pins its
//! own copy, and how a later worker, resume or observation reads it back.
//!
//! The run owns its definition reads, so `job/run` sits below `job/pipeline`.
//! Pipeline's submission writes the snapshot ([`OrbitRuntime::write_run_definition_snapshot`])
//! at the path [`run_definition_snapshot_path`] names.

use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::open_read_only_no_follow;
use orbit_engine::activity_job::load_job_asset;
use orbit_types::workflow::{JobRun, JobV2};

use crate::OrbitRuntime;

/// Turn a persisted run ID into a filename only after rejecting path syntax.
///
/// Run IDs normally come from the store, but worker entry points also accept
/// an ID from a process argument. Keeping this check at the shared filename
/// boundary prevents either source from steering pipeline artifacts outside
/// their owning directory.
pub(in crate::application::job) fn pipeline_worker_file_name(
    run_id: &str,
    suffix: &str,
) -> Result<String, OrbitError> {
    let safe = !run_id.is_empty()
        && run_id != "."
        && run_id != ".."
        && !run_id.contains(['/', '\\'])
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !safe {
        return Err(OrbitError::InvalidInput(format!(
            "job run id must be a safe filename stem: {run_id}"
        )));
    }

    Ok(format!("{run_id}{suffix}"))
}

/// Where a submitted run's pinned job definition lives.
pub(crate) fn run_definition_snapshot_path(
    job_runs_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    Ok(job_runs_dir.join(pipeline_worker_file_name(run_id, ".job.yaml")?))
}

impl OrbitRuntime {
    /// The YAML pinned beside `run_id`, when this run has one.
    ///
    /// Direct-path submission writes the snapshot so a later worker — and a
    /// later resume — executes that definition. A missing file means the run
    /// is catalog-backed. A symlink, non-regular file, or present file that
    /// cannot be read or parsed is an error: resume must not silently
    /// substitute a catalog asset of the same name.
    pub(crate) fn read_run_definition_snapshot(
        &self,
        run_id: &str,
    ) -> Result<Option<(JobV2, String)>, OrbitError> {
        let snapshot = run_definition_snapshot_path(&self.paths().job_runs_dir, run_id)?;
        // The shared filename validator confines the leaf to job_runs_dir.
        // Resolve its parent and open it without following the final component;
        // all checks and the read then use that same descriptor.
        let mut file = match open_read_only_no_follow(&snapshot) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(OrbitError::InvalidInput(format!(
                    "open job run definition snapshot '{}': {error}",
                    snapshot.display()
                )));
            }
        };
        let metadata = file.metadata().map_err(|error| {
            OrbitError::InvalidInput(format!("inspect {}: {error}", snapshot.display()))
        })?;
        if !metadata.is_file() {
            return Err(OrbitError::InvalidInput(format!(
                "job run definition snapshot must be a regular file: {}",
                snapshot.display()
            )));
        }
        let mut yaml = String::new();
        file.read_to_string(&mut yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("read {}: {error}", snapshot.display()))
        })?;
        let asset = load_job_asset(&yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("load {}: {error}", snapshot.display()))
        })?;
        Ok(Some((asset.spec, yaml)))
    }
    /// The definition a persisted run must execute: its own snapshot when the
    /// submission pinned one, otherwise the catalog asset named by the run.
    pub(crate) fn resolve_run_definition(
        &self,
        run: &JobRun,
    ) -> Result<(PathBuf, JobV2), OrbitError> {
        let snapshot = run_definition_snapshot_path(&self.paths().job_runs_dir, &run.run_id)?;
        if let Some((spec, _)) = self.read_run_definition_snapshot(&run.run_id)? {
            return Ok((snapshot, spec));
        }
        self.load_v2_job_asset_by_name(&run.job_id)
    }
}
