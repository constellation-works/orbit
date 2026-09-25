//! Import of a migration archive into the local registry: staging and
//! validation, conflict resolution (renumber, skip, fail, owner-wins), the
//! write phase and its rollback guard.

use crate::driver::file::task_bundle::{
    TaskBundleV2, bundle_lock_target, read_bundle_at, replace_bundle_at, write_bundle_at,
    write_bundle_with_artifacts_at,
};
use crate::driver::sqlite::task_registry::{
    RegisterWorkspaceParams, TaskBundleBinding, TaskRegistryStore, parse_orb_task_number,
};
use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_types::task::{task_id_prefix, validate_orb_task_id};
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

/// A validated, staged bundle read out of an archive before any state mutation.
struct StagedBundle {
    source_id: String,
    bundle: TaskBundleV2,
    /// Extracted bundle directory under the import staging tempdir; the source
    /// of truth for `artifacts/files/**` blobs copied into the canonical
    /// bundle during the write phase.
    staging_dir: PathBuf,
}

/// A colliding mirror whose owner is authoritative: the staged bundle replaces
/// the local copy at the directory the registry already binds it to.
struct MirrorReplacement {
    staged: StagedBundle,
    bundle_dir: PathBuf,
    /// The canonical directory existed without its registry binding, so the
    /// successful refresh must restore that binding.
    register_binding: bool,
}

/// Resolved import target after workspace resolution.
struct ImportTarget {
    workspace_id: String,
    /// Set when import must register a new logical workspace record.
    register: Option<RegisterWorkspaceParams>,
}

/// Import tasks from a tar.zst archive into the local registry.
///
/// `target_workspace_id` overrides the destination; otherwise the archive's
/// source workspace is used (registering it if unknown). Conflicts on
/// already-used ids are resolved by `policy`.
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
    let target = resolve_target(registry, &manifest, target_workspace_id)?;

    // The prefix this host mints under decides which colliding ids are mirrors
    // of another host's tasks and which are locally owned.
    let local_prefix = registry.local_task_prefix()?;

    // Classify each staged bundle against the current registry.
    let mut kept: Vec<StagedBundle> = Vec::new();
    let mut to_renumber: Vec<StagedBundle> = Vec::new();
    let mut to_overwrite: Vec<MirrorReplacement> = Vec::new();
    let mut records: Vec<ImportedTask> = Vec::new();
    for staged in staged {
        match registry.find_task_binding(&staged.source_id)? {
            None => {
                // An owner-wins sync repairs a canonical bundle the registry
                // has lost track of, but authority still follows the prefix:
                // a local-prefix bundle is ours no matter what the index says.
                let orphan_dir = (policy == ImportConflictPolicy::OwnerWins)
                    .then(|| {
                        registry.canonical_task_bundle_path(&target.workspace_id, &staged.source_id)
                    })
                    .transpose()?
                    .filter(|bundle_dir| bundle_dir.is_dir());

                match orphan_dir {
                    Some(_) if task_id_prefix(&staged.source_id) == Some(local_prefix.as_str()) => {
                        records.push(ImportedTask {
                            source_id: staged.source_id.clone(),
                            final_id: staged.source_id,
                            action: ImportAction::SkippedLocalOwned,
                        });
                    }
                    Some(bundle_dir) => to_overwrite.push(MirrorReplacement {
                        staged,
                        bundle_dir,
                        register_binding: true,
                    }),
                    None => kept.push(staged),
                }
            }
            Some(existing) => {
                let identical = existing.partition_id == target.workspace_id
                    && read_bundle_at(&existing.canonical_path)
                        .ok()
                        .is_some_and(|current| current == staged.bundle);
                if identical {
                    records.push(ImportedTask {
                        source_id: staged.source_id.clone(),
                        final_id: staged.source_id,
                        action: ImportAction::AlreadyPresent,
                    });
                } else {
                    match policy {
                        ImportConflictPolicy::Fail => {
                            return Err(OrbitError::InvalidInput(format!(
                                "task id '{}' already exists locally; import aborted (--on-conflict=fail)",
                                staged.source_id
                            )));
                        }
                        ImportConflictPolicy::Skip => records.push(ImportedTask {
                            source_id: staged.source_id.clone(),
                            final_id: staged.source_id,
                            action: ImportAction::SkippedConflict,
                        }),
                        // Note: renumber is not idempotent across re-runs — the
                        // source id still collides with the original local task
                        // (unchanged), so a second run mints another fresh id.
                        ImportConflictPolicy::Renumber => to_renumber.push(staged),
                        ImportConflictPolicy::OwnerWins => {
                            match owner_wins_verdict(
                                &staged.source_id,
                                &existing,
                                &target.workspace_id,
                                &local_prefix,
                            )? {
                                MirrorVerdict::LocalOwned => records.push(ImportedTask {
                                    source_id: staged.source_id.clone(),
                                    final_id: staged.source_id,
                                    action: ImportAction::SkippedLocalOwned,
                                }),
                                MirrorVerdict::Replace(bundle_dir) => {
                                    to_overwrite.push(MirrorReplacement {
                                        staged,
                                        bundle_dir,
                                        register_binding: false,
                                    })
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Nothing new to write (all ids were free-and-identical, or all skipped).
    if kept.is_empty() && to_renumber.is_empty() && to_overwrite.is_empty() {
        return Ok(ImportOutcome {
            workspace_id: target.workspace_id,
            registered_workspace: false,
            tasks: records,
            id_remap: BTreeMap::new(),
            id_map_path: None,
        });
    }

    // ---- Phase 2: mutate. Track writes for best-effort rollback. ----
    // Note: the monotonic allocator bumps below are intentionally never rolled
    // back — the counter only moves forward and holes are expected. A newly
    // created workspace binding is tracked and retired on rollback, while
    // pre-existing workspace bindings are never touched.
    let mut guard = WriteGuard::new(registry);

    let registered_workspace = if let Some(params) = &target.register {
        registry.register_workspace(params.clone())?;
        guard.registered_workspace = Some(target.workspace_id.clone());
        true
    } else {
        false
    };

    // Reserve headroom so renumber allocations never collide with kept ids.
    // Only renumbering mints, so no other policy moves the counter here.
    if !to_renumber.is_empty() {
        let kept_max = kept
            .iter()
            .filter_map(|staged| parse_orb_task_number(&staged.source_id))
            .max();
        let existing_max = registry.max_registered_task_number()?;
        let floor = [kept_max, existing_max]
            .into_iter()
            .flatten()
            .max()
            .map(|value| value + 1)
            .unwrap_or(0);
        registry.bump_allocator_to_at_least(floor)?;
    }

    // Allocate new ids for collisions (deterministic order by source id). The
    // whole run is reserved with a single counter bump, so a renumber of N
    // tasks costs one commit rather than N.
    to_renumber.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    let new_ids = registry.allocate_task_ids(&target.workspace_id, to_renumber.len())?;
    let id_remap: BTreeMap<String, String> = to_renumber
        .iter()
        .map(|staged| staged.source_id.clone())
        .zip(new_ids)
        .collect();

    // Write kept + renumbered bundles, rewriting relation targets in the set,
    // then replace the mirrors their owner has changed.
    let write_result = (|| -> Result<Vec<u32>, OrbitError> {
        let mut landed_numbers = Vec::new();
        // Every binding this import lands is collected and registered in one
        // transaction below: the bundles are already durable on disk, and one
        // commit for the set replaces one WAL fsync per task.
        let mut pending_bindings: Vec<(String, PathBuf)> = Vec::new();
        for staged in kept.iter().chain(to_renumber.iter()) {
            let final_id = id_remap
                .get(&staged.source_id)
                .cloned()
                .unwrap_or_else(|| staged.source_id.clone());
            let action = if id_remap.contains_key(&staged.source_id) {
                ImportAction::Renumbered
            } else {
                ImportAction::Kept
            };
            let bundle = remap_bundle(&staged.bundle, &final_id, &id_remap);
            let dir = registry.canonical_task_bundle_path(&target.workspace_id, &final_id)?;
            if bundle
                .artifact_manifest
                .as_ref()
                .is_some_and(|manifest| !manifest.files.is_empty())
            {
                write_bundle_with_artifacts_at(&dir, &bundle, &staged.staging_dir)?;
            } else {
                write_bundle_at(&dir, &bundle)?;
            }
            guard.written_dirs.push(dir.clone());
            pending_bindings.push((final_id.clone(), dir));
            if let Some(number) = local_task_number(&final_id, &local_prefix) {
                landed_numbers.push(number);
            }
            records.push(ImportedTask {
                source_id: staged.source_id.clone(),
                final_id,
                action,
            });
        }

        // Owner-wins replacements land under their own (foreign) ids, so they
        // are already registered and never touch the local allocator.
        for replacement in &to_overwrite {
            let staged = &replacement.staged;
            // Readers and writers of this bundle coordinate on the same sibling
            // lock file, which outlives the directory swap.
            with_exclusive_file_lock(
                &bundle_lock_target(&replacement.bundle_dir),
                "task migration import",
                || replace_bundle_at(&replacement.bundle_dir, &staged.bundle, &staged.staging_dir),
            )?;
            if replacement.register_binding {
                pending_bindings.push((staged.source_id.clone(), replacement.bundle_dir.clone()));
            }
            records.push(ImportedTask {
                source_id: staged.source_id.clone(),
                final_id: staged.source_id.clone(),
                action: ImportAction::Updated,
            });
        }

        registry.register_task_bundles(&target.workspace_id, &pending_bindings)?;
        guard
            .registered_ids
            .extend(pending_bindings.into_iter().map(|(task_id, _)| task_id));
        Ok(landed_numbers)
    })();

    let landed_numbers = match write_result {
        Ok(numbers) => numbers,
        Err(err) => {
            guard.rollback();
            return Err(err);
        }
    };

    // Rebuild the whole workspace index from disk (pre-existing + imported).
    if let Err(err) = rebuild_index_from_disk(registry, &target.workspace_id) {
        guard.rollback();
        return Err(err);
    }

    // Bump the allocator past the highest landed id so future creates don't collide.
    if let Some(max) = landed_numbers.iter().copied().max() {
        registry.bump_allocator_to_at_least(max + 1)?;
    }

    // Persist and surface the old→new mapping.
    let id_map_path = if id_remap.is_empty() {
        None
    } else {
        Some(write_id_map(archive_path, &id_remap)?)
    };

    guard.disarm();
    Ok(ImportOutcome {
        workspace_id: target.workspace_id,
        registered_workspace,
        tasks: records,
        id_remap,
        id_map_path,
    })
}

/// What owner-wins does with one colliding id.
enum MirrorVerdict {
    /// The id is under the local prefix, so the local copy is authoritative.
    LocalOwned,
    /// The owner's copy supersedes the local mirror at this bundle directory.
    Replace(PathBuf),
}

/// Decide a colliding id from its prefix alone: this host's own ids are never
/// overwritten, and any other host's id is refreshed in place.
fn owner_wins_verdict(
    source_id: &str,
    existing: &TaskBundleBinding,
    target_workspace_id: &str,
    local_prefix: &str,
) -> Result<MirrorVerdict, OrbitError> {
    if task_id_prefix(source_id) == Some(local_prefix) {
        return Ok(MirrorVerdict::LocalOwned);
    }
    // Replacing in place keeps the mirror where the registry already binds it;
    // moving a task between workspaces is a reconciliation this rule cannot
    // decide from the id.
    if existing.partition_id != target_workspace_id {
        return Err(OrbitError::InvalidInput(format!(
            "task id '{source_id}' is registered to workspace '{}' locally but this archive lands in '{target_workspace_id}'; resolve the workspace before syncing",
            existing.partition_id
        )));
    }
    Ok(MirrorVerdict::Replace(existing.canonical_path.clone()))
}

/// Numeric suffix of `task_id`, but only for ids this host mints. A mirror
/// carries its owner's numbering, which must not advance the local allocator.
fn local_task_number(task_id: &str, local_prefix: &str) -> Option<u32> {
    (task_id_prefix(task_id) == Some(local_prefix))
        .then(|| parse_orb_task_number(task_id))
        .flatten()
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

/// Resolve where imported tasks should land, and whether a new logical workspace
/// must be registered.
fn resolve_target(
    registry: &TaskRegistryStore,
    manifest: &TaskMigrationManifest,
    target_workspace_id: Option<&str>,
) -> Result<ImportTarget, OrbitError> {
    if let Some(requested) = target_workspace_id {
        let binding = registry.find_workspace_binding(requested)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "target workspace '{requested}' is not registered in the coordination registry; register it first or omit --task-workspace to register the source workspace"
            ))
        })?;
        return Ok(ImportTarget {
            workspace_id: binding.partition_id,
            register: None,
        });
    }

    if let Some(binding) = registry.find_workspace_binding(&manifest.source_workspace_id)? {
        return Ok(ImportTarget {
            workspace_id: binding.partition_id,
            register: None,
        });
    }

    // Source workspace is unknown locally and no target was named: register
    // only its logical coordination identity. A later checkout link may add a
    // checkout binding; migration must never fabricate checkout paths.
    let params = RegisterWorkspaceParams {
        partition_id: manifest.source_workspace_id.clone(),
        slug: manifest.source_workspace_slug.clone(),
        repo_fingerprint: None,
    };
    Ok(ImportTarget {
        workspace_id: manifest.source_workspace_id.clone(),
        register: Some(params),
    })
}

/// Clone `bundle`, setting the envelope id to `final_id` and rewriting every
/// relation target that is being renumbered within the imported set.
fn remap_bundle(
    bundle: &TaskBundleV2,
    final_id: &str,
    id_remap: &BTreeMap<String, String>,
) -> TaskBundleV2 {
    let mut out = bundle.clone();
    out.envelope.id = final_id.to_string();
    for relation in &mut out.envelope.relations {
        // Rewrite whenever the target is being renumbered, regardless of relation
        // type. A `ChildOf` target is the task's parent, so this covers parent
        // rewrites; `Produces`/`Resolves` may point at F-/L-/ADR- ids that are
        // never in the renumber set, so they are left untouched.
        if let Some(mapped) = id_remap.get(&relation.target) {
            relation.target = mapped.clone();
        }
    }
    out
}

/// Rebuild the target workspace's index rows from the bundles currently on disk.
fn rebuild_index_from_disk(
    registry: &TaskRegistryStore,
    workspace_id: &str,
) -> Result<(), OrbitError> {
    let bindings = registry.tasks_for_workspace(workspace_id)?;
    let mut envelopes = Vec::with_capacity(bindings.len());
    for binding in bindings {
        envelopes.push(read_bundle_at(&binding.canonical_path)?.envelope);
    }
    registry.replace_workspace_task_indexes(workspace_id, &envelopes)
}

fn write_id_map(
    archive_path: &Path,
    id_remap: &BTreeMap<String, String>,
) -> Result<PathBuf, OrbitError> {
    let mut os = archive_path.as_os_str().to_owned();
    os.push(".idmap.json");
    let path = PathBuf::from(os);
    let json = serde_json::to_vec_pretty(id_remap)
        .map_err(|e| OrbitError::Store(format!("failed to encode id map: {e}")))?;
    std::fs::write(&path, json).map_err(|e| OrbitError::Io(e.to_string()))?;
    Ok(path)
}

/// Tracks filesystem/registry writes so a mid-import failure can be rolled back.
struct WriteGuard<'a> {
    registry: &'a TaskRegistryStore,
    written_dirs: Vec<PathBuf>,
    registered_ids: Vec<String>,
    registered_workspace: Option<String>,
    armed: bool,
}

impl<'a> WriteGuard<'a> {
    fn new(registry: &'a TaskRegistryStore) -> Self {
        Self {
            registry,
            written_dirs: Vec::new(),
            registered_ids: Vec::new(),
            registered_workspace: None,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Best-effort undo of everything written so far. Registry rows are removed
    /// first (they reference the dirs), then the bundle directories. Failures
    /// are logged, not propagated — the caller is already returning the original
    /// error.
    ///
    /// Owner-wins replacements are deliberately *not* undone: the bundle they
    /// overwrote was a mirror, the copy now on disk is the owner's current one,
    /// and restoring the stale mirror would be the wrong direction. Their index
    /// rows can be left behind the bundle until the next successful sync (or
    /// `orbit task reindex`) rebuilds them.
    fn rollback(&mut self) {
        if let Some(workspace_id) = self.registered_workspace.take() {
            let _ = self.registry.unbind_workspace(&workspace_id);
        }
        for id in self.registered_ids.drain(..) {
            // The partition id is not needed to look up the (global) binding,
            // but the API takes it; recover it from the binding.
            if let Ok(Some(binding)) = self.registry.find_task_binding(&id) {
                let _ = self
                    .registry
                    .unregister_task_bundle(&id, &binding.partition_id);
            }
        }
        for dir in self.written_dirs.drain(..) {
            let _ = std::fs::remove_dir_all(&dir);
        }
        self.armed = false;
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.rollback();
        }
    }
}
