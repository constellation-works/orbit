//! Validate a migration archive before publishing its bundles and index.

use crate::boundary::import::{StagedBundle, commit_import};
use crate::driver::file::task_bundle::read_bundle_at;
use crate::repository::task_index::TaskRegistryStore;
use orbit_common::OrbitError;
use orbit_types::task::validate_orb_task_id;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::archive;
use super::manifest::{TaskMigrationManifest, read_manifest, validate_manifest};

/// How to resolve an imported task id that already exists locally (with
/// non-identical content).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportConflictPolicy {
    /// Allocate a fresh local id for the colliding task and rewrite references.
    Renumber,
    /// Leave the local task untouched and drop the incoming one.
    Skip,
    /// Abort the whole import on the first collision.
    Fail,
    /// Prefix authority: the host that minted a task id is its sole writer.
    /// A colliding id under a *foreign* prefix is a stale mirror, so the
    /// incoming bundle replaces it; a colliding id under the *local* prefix is
    /// locally owned and is never touched. Nothing is ever renumbered, which
    /// makes a repeated sync of the same archive a no-op.
    OwnerWins,
}

/// What happened to a single task during import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportAction {
    /// Landed under its original id (was free locally).
    Kept,
    /// Collided; landed under a freshly allocated id.
    Renumbered,
    /// Already present locally with identical content — skipped (idempotent).
    AlreadyPresent,
    /// Collided and `--on-conflict=skip` dropped it.
    SkippedConflict,
    /// Owner-wins: a foreign-prefix mirror was replaced by the owner's copy.
    Updated,
    /// Owner-wins: the id is under the local prefix, so the local copy is
    /// authoritative and the incoming one was dropped.
    SkippedLocalOwned,
}

/// Per-task import record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedTask {
    /// Id in the source archive.
    pub source_id: String,
    /// Id the task landed under locally (equals `source_id` unless renumbered).
    pub final_id: String,
    /// Outcome for this task.
    pub action: ImportAction,
}

/// Result of [`import_tasks`].
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    /// Target workspace the tasks landed in.
    pub workspace_id: String,
    /// True if import registered a new (previously unknown) logical workspace.
    pub registered_workspace: bool,
    /// Per-task records.
    pub tasks: Vec<ImportedTask>,
    /// Old→new id map for renumbered tasks (empty if none renumbered).
    pub id_remap: BTreeMap<String, String>,
    /// Path of the written old→new mapping file, if any renumbering occurred.
    pub id_map_path: Option<PathBuf>,
}

/// Import tasks from a tar.zst archive into the local registry.
///
/// `target_workspace_id` overrides the destination; otherwise the archive's
/// source workspace is used (registering it if unknown). Conflicts on
/// already-used ids are resolved by `policy`.
///
/// After that selection, and before any workspace registration, canonical
/// bundle write, or task binding, a relation target under a prefix this
/// registry knows — or under a prefix of an id this import will register —
/// must already be registered or be one of the final ids this import will
/// land. A miss names the source task, the relation type and the missing
/// target and leaves the destination unchanged. A prefix this import does
/// not register, and that the registry has never issued, may still dangle.
/// Cycles and other failures that surface only while the index is rebuilt
/// still roll back fresh bundles and a workspace registration this import
/// created.
///
/// Import is a restore of existing records, not a create-time assessment:
/// a source task with no complexity stays unlabeled (`None`) so re-import
/// stays byte-identical and historical gaps are not rewritten. New work
/// from auto-task mint uses [`orbit_types::task::TaskComplexity::Unassessed`].
pub fn import_tasks(
    registry: &TaskRegistryStore,
    archive_path: &Path,
    target_workspace_id: Option<&str>,
    policy: ImportConflictPolicy,
) -> Result<ImportOutcome, OrbitError> {
    // ---- Phase 1: validate everything before touching any state. ----
    let staging = tempfile::Builder::new()
        .prefix("orbit-task-import-")
        .tempdir()
        .map_err(|e| OrbitError::Io(e.to_string()))?;
    archive::extract_archive(archive_path, staging.path())?;

    let manifest = read_manifest(staging.path())?;
    validate_manifest(&manifest)?;

    let staged = stage_bundles(staging.path(), &manifest)?;
    commit_import(
        registry,
        archive_path,
        target_workspace_id,
        policy,
        &manifest,
        staged,
    )
}

/// Read + validate every bundle referenced by the manifest into memory. Any
/// integrity failure aborts before state is touched.
fn stage_bundles(
    staging: &Path,
    manifest: &TaskMigrationManifest,
) -> Result<Vec<StagedBundle>, OrbitError> {
    let mut staged = Vec::with_capacity(manifest.task_ids.len());
    let mut seen = BTreeSet::new();
    for id in &manifest.task_ids {
        validate_orb_task_id(id)?;
        if !seen.insert(id.clone()) {
            return Err(OrbitError::InvalidInput(format!(
                "archive manifest lists task '{id}' more than once"
            )));
        }
        let dir = staging.join(archive::BUNDLES_DIR).join(id);
        if !dir.is_dir() {
            return Err(OrbitError::Store(format!(
                "archive manifest references '{id}' but its bundle is missing"
            )));
        }
        // read_bundle_at validates the envelope, jsonl rows, and artifact hashes.
        let bundle = read_bundle_at(&dir)?;
        if bundle.envelope.id != *id {
            return Err(OrbitError::Store(format!(
                "archive bundle '{id}' contains mismatched envelope id '{}'",
                bundle.envelope.id
            )));
        }
        staged.push(StagedBundle {
            source_id: id.clone(),
            bundle,
            staging_dir: dir,
        });
    }
    Ok(staged)
}
