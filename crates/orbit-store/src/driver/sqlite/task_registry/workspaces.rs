//! Workspace and checkout bindings: binding, rebinding, registration,
//! repository fingerprints, lookups and retirement.

use super::partition_id::{next_partition_id_candidate, sanitize_slug, validate_partition_id};
use super::queries::{
    decode_workspace_checkout_binding, workspace_by_id, workspace_by_orbit_dir,
    workspace_checkout_by_id, workspace_checkout_by_paths,
};
use super::store::TaskRegistryStore;
use super::util::{now_string, path_to_string};
use crate::contracts::{
    BindWorkspaceParams, RegisterWorkspaceParams, WorkspaceBinding, WorkspaceCheckoutBinding,
};
use crate::fs::path_safety::normalize_path;
use orbit_common::{NotFoundKind, OrbitError};
use rusqlite::{TransactionBehavior, params};
use std::collections::BTreeSet;
use std::path::Path;

impl TaskRegistryStore {
    /// Bind one checkout to its task-store partition, minting the partition id
    /// when the caller supplies none.
    ///
    /// The id this returns names the directory under
    /// [`task_workspaces_dir`](super::task_workspaces_dir) that holds the
    /// checkout's task bundles, and is stored in
    /// `workspace_bindings.workspace_id`. It is *not* a workspace-registry id:
    /// a caller that passes `params.partition_id` decides which namespace the
    /// partition is named in, and a caller that passes `None` gets a minted
    /// `<slug>-<hash>` id that no workspace registry knows. See
    /// [`task_workspaces_dir`](super::task_workspaces_dir) for the two id
    /// spaces in full.
    pub fn bind_workspace(
        &self,
        params: BindWorkspaceParams,
    ) -> Result<WorkspaceCheckoutBinding, OrbitError> {
        let repo_root = normalize_path(&params.repo_root);
        let workspace_path = normalize_path(&params.workspace_path);
        let orbit_dir = normalize_path(&params.orbit_dir);
        let slug = sanitize_slug(&params.slug);
        let requested_partition_id = params
            .partition_id
            .as_deref()
            .map(validate_partition_id)
            .transpose()?;

        // Runtime construction asks for the same binding on every command.
        // Satisfy that observational fast path without opening a write
        // transaction; a read-only mount must only fail when a real rebind is
        // required.
        {
            let conn = self.read()?;
            if let Some(existing) = workspace_by_orbit_dir(&conn, &orbit_dir)? {
                if let Some(requested) = &requested_partition_id
                    && requested != &existing.partition_id
                {
                    return Err(OrbitError::InvalidInput(format!(
                        "orbit dir '{}' is already bound to workspace '{}', not '{}'",
                        orbit_dir.display(),
                        existing.partition_id,
                        requested
                    )));
                }
                return Ok(existing);
            }
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if let Some(existing) = workspace_by_orbit_dir(&tx, &orbit_dir)? {
            if let Some(requested) = &requested_partition_id
                && requested != &existing.partition_id
            {
                return Err(OrbitError::InvalidInput(format!(
                    "orbit dir '{}' is already bound to workspace '{}', not '{}'",
                    orbit_dir.display(),
                    existing.partition_id,
                    requested
                )));
            }
            tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
            return Ok(existing);
        }

        let partition_id = match requested_partition_id {
            Some(id) => id,
            // A checkout is identified by its repo root and workspace path, not
            // by the orbit dir the caller happens to be running with. Reusing
            // the id already bound to those paths keeps a repeat bind from
            // minting a second logical workspace for the same checkout.
            None => match workspace_checkout_by_paths(&tx, &repo_root, &workspace_path)? {
                Some(existing) => existing.partition_id,
                None => next_partition_id_candidate(&tx, &slug, &workspace_path)?,
            },
        };
        let now = now_string();
        if let Some(existing) = workspace_checkout_by_id(&tx, &partition_id)? {
            // The logical workspace already has a checkout, and it is bound to
            // a different orbit dir (a matching one returned above). When the
            // checkout paths are unchanged this is the same checkout whose
            // orbit dir moved — the shape short-lived CLI invocations produce
            // when they generate an ephemeral orbit dir per call — so move the
            // binding instead of failing (ORB-10507). A checkout at genuinely
            // different paths still conflicts: a real id clash, not a rebind.
            if normalize_path(&existing.repo_root) != repo_root
                || normalize_path(&existing.workspace_path) != workspace_path
            {
                return Err(OrbitError::Store(format!(
                    "workspace id '{partition_id}' already has a local checkout at '{}'",
                    existing.orbit_dir.display()
                )));
            }
            tx.execute(
                "UPDATE workspace_checkout_bindings
                 SET orbit_dir = ?2, updated_at = ?3
                 WHERE workspace_id = ?1",
                params![partition_id, path_to_string(&orbit_dir), now],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
            let binding = workspace_checkout_by_id(&tx, &partition_id)?.ok_or_else(|| {
                OrbitError::Store("failed to read rebound workspace checkout binding".into())
            })?;
            tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
            return Ok(binding);
        }

        if workspace_by_id(&tx, &partition_id)?.is_none() {
            tx.execute(
                "INSERT INTO workspace_bindings (
                    workspace_id, slug, repo_fingerprint, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![partition_id, slug, params.repo_fingerprint, now],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        }
        tx.execute(
            "INSERT INTO workspace_checkout_bindings (
                workspace_id, repo_root, workspace_path, orbit_dir, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![
                partition_id,
                path_to_string(&repo_root),
                path_to_string(&workspace_path),
                path_to_string(&orbit_dir),
                now,
            ],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;

        let binding = workspace_checkout_by_id(&tx, &partition_id)?.ok_or_else(|| {
            OrbitError::Store("failed to read inserted workspace checkout binding".into())
        })?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(binding)
    }

    /// Move `orbit_dir` onto `params.partition_id`, replacing any checkout
    /// currently bound to that directory.
    ///
    /// `bind_workspace` fails closed when the orbit dir already belongs to a
    /// different workspace. Workspace `--force` reconciliation uses this to
    /// finish a split-brain bind: a read-only command that minted a synthetic
    /// checkout for `parent(data-dir)`, then `workspace init --force` claiming
    /// the same data dir for a real git checkout.
    pub fn rebind_checkout(
        &self,
        params: BindWorkspaceParams,
    ) -> Result<WorkspaceCheckoutBinding, OrbitError> {
        let repo_root = normalize_path(&params.repo_root);
        let workspace_path = normalize_path(&params.workspace_path);
        let orbit_dir = normalize_path(&params.orbit_dir);
        let slug = sanitize_slug(&params.slug);
        let partition_id =
            validate_partition_id(params.partition_id.as_deref().ok_or_else(|| {
                OrbitError::InvalidInput("rebind_checkout requires an explicit workspace id".into())
            })?)?;

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let now = now_string();

        if workspace_by_id(&tx, &partition_id)?.is_none() {
            tx.execute(
                "INSERT INTO workspace_bindings (
                    workspace_id, slug, repo_fingerprint, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![partition_id, slug, params.repo_fingerprint, now],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        }

        if let Some(existing) = workspace_by_orbit_dir(&tx, &orbit_dir)?
            && existing.partition_id != partition_id
        {
            tx.execute(
                "DELETE FROM workspace_checkout_bindings WHERE orbit_dir = ?1",
                [path_to_string(&orbit_dir)],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        }

        if workspace_checkout_by_id(&tx, &partition_id)?.is_some() {
            tx.execute(
                "UPDATE workspace_checkout_bindings
                 SET repo_root = ?2, workspace_path = ?3, orbit_dir = ?4, updated_at = ?5
                 WHERE workspace_id = ?1",
                params![
                    partition_id,
                    path_to_string(&repo_root),
                    path_to_string(&workspace_path),
                    path_to_string(&orbit_dir),
                    now,
                ],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        } else {
            tx.execute(
                "INSERT INTO workspace_checkout_bindings (
                    workspace_id, repo_root, workspace_path, orbit_dir, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    partition_id,
                    path_to_string(&repo_root),
                    path_to_string(&workspace_path),
                    path_to_string(&orbit_dir),
                    now,
                ],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        }

        let binding = workspace_checkout_by_id(&tx, &partition_id)?.ok_or_else(|| {
            OrbitError::Store("failed to read rebound workspace checkout binding".into())
        })?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(binding)
    }

    /// Register a logical workspace in the coordination registry without
    /// inventing a machine-local checkout path.
    pub fn register_workspace(
        &self,
        params: RegisterWorkspaceParams,
    ) -> Result<WorkspaceBinding, OrbitError> {
        let partition_id = validate_partition_id(&params.partition_id)?;
        let slug = sanitize_slug(&params.slug);
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        if let Some(existing) = workspace_by_id(&tx, &partition_id)? {
            if existing.slug != slug || existing.repo_fingerprint != params.repo_fingerprint {
                return Err(OrbitError::InvalidInput(format!(
                    "logical workspace '{partition_id}' is already registered with different metadata"
                )));
            }
            tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
            return Ok(existing);
        }

        let now = now_string();
        tx.execute(
            "INSERT INTO workspace_bindings(
                workspace_id, slug, repo_fingerprint, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![partition_id, slug, params.repo_fingerprint, now],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        let binding = workspace_by_id(&tx, &partition_id)?.ok_or_else(|| {
            OrbitError::Store("failed to read inserted logical workspace binding".into())
        })?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(binding)
    }

    /// Record the portable source-repository identity when an existing
    /// workspace first enables a workflow that requires it.
    ///
    /// Ordinary task-runtime bootstrap deliberately does not infer remote
    /// identity. Explicit publication binding supplies the registry-validated
    /// value instead. Once recorded, a different value is a pairing conflict,
    /// never an implicit source move.
    pub fn record_workspace_repo_fingerprint(
        &self,
        partition_id: &str,
        repo_fingerprint: &str,
    ) -> Result<WorkspaceBinding, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if repo_fingerprint.trim() != repo_fingerprint || repo_fingerprint.is_empty() {
            return Err(OrbitError::InvalidInput(
                "workspace repository fingerprint must be non-empty and trimmed".to_string(),
            ));
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let existing = workspace_by_id(&tx, &partition_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, partition_id.clone()))?;
        match existing.repo_fingerprint.as_deref() {
            Some(current) if current == repo_fingerprint => {}
            Some(_) => {
                return Err(OrbitError::InvalidInput(format!(
                    "workspace '{partition_id}' is registered with a different source-repository fingerprint"
                )));
            }
            None => {
                tx.execute(
                    "UPDATE workspace_bindings
                     SET repo_fingerprint = ?2, updated_at = ?3
                     WHERE workspace_id = ?1",
                    params![partition_id, repo_fingerprint, now_string()],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            }
        }
        let binding = workspace_by_id(&tx, &partition_id)?.ok_or_else(|| {
            OrbitError::Store("failed to read fingerprinted workspace binding".to_string())
        })?;
        tx.commit()
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        Ok(binding)
    }

    pub fn find_rebind_candidates(
        &self,
        repo_root: &Path,
        workspace_path: &Path,
        orbit_dir: &Path,
    ) -> Result<Vec<WorkspaceCheckoutBinding>, OrbitError> {
        let repo_root = normalize_path(repo_root);
        let workspace_path = normalize_path(workspace_path);
        let orbit_dir = normalize_path(orbit_dir);
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT workspace_id, repo_root, workspace_path, orbit_dir, created_at, updated_at
                 FROM workspace_checkout_bindings
                 WHERE repo_root = ?1 OR workspace_path = ?2 OR orbit_dir = ?3
                 ORDER BY updated_at DESC, workspace_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    path_to_string(&repo_root),
                    path_to_string(&workspace_path),
                    path_to_string(&orbit_dir),
                ],
                decode_workspace_checkout_binding,
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Every task-store partition id the registry binds.
    ///
    /// This is the id space the on-disk partitions under
    /// `<global>/tasks/workspaces/` are named after, so a caller deciding
    /// whether a partition directory is still claimed asks here rather than
    /// inferring an owner from the workspace catalog, whose `ws_*` ids are a
    /// different namespace [ORB-12119].
    pub fn partition_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached("SELECT workspace_id FROM workspace_bindings")
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<BTreeSet<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Retire one workspace's registry rows: its checkout binding, every task
    /// bundle bound to it, and the logical workspace itself. Returns whether a
    /// binding existed.
    ///
    /// Paired with deleting the workspace's bundle partition, so that no
    /// binding survives pointing at a directory that is gone [ORB-12119]. The
    /// dependent rows are deleted explicitly rather than left to
    /// `ON DELETE CASCADE`, which a connection without `foreign_keys=ON` would
    /// silently skip.
    pub fn unbind_workspace(&self, partition_id: &str) -> Result<bool, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        for statement in [
            // Relations are retired from both ends, like `unregister_task_bundle`:
            // an edge another workspace points at these tasks would otherwise
            // outlive them.
            "DELETE FROM task_bundle_relations
             WHERE workspace_id = ?1
                OR target_task_id IN (
                    SELECT task_id FROM task_bundle_bindings WHERE workspace_id = ?1
                )",
            "DELETE FROM task_bundle_tags WHERE workspace_id = ?1",
            "DELETE FROM task_bundle_index WHERE workspace_id = ?1",
            "DELETE FROM task_bundle_bindings WHERE workspace_id = ?1",
            "DELETE FROM workspace_checkout_bindings WHERE workspace_id = ?1",
        ] {
            tx.execute(statement, [&partition_id])
                .map_err(|e| OrbitError::Store(e.to_string()))?;
        }
        let deleted = tx
            .execute(
                "DELETE FROM workspace_bindings WHERE workspace_id = ?1",
                [&partition_id],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(deleted > 0)
    }

    /// Look up a logical workspace by id. Public wrapper over the internal query
    /// so migration tooling can resolve a target workspace without opening the
    /// SQLite connection directly.
    pub fn find_workspace_binding(
        &self,
        partition_id: &str,
    ) -> Result<Option<WorkspaceBinding>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        workspace_by_id(&conn, &partition_id)
    }

    /// Look up the machine-local checkout for a logical workspace, if this
    /// machine has one.
    pub fn find_workspace_checkout(
        &self,
        partition_id: &str,
    ) -> Result<Option<WorkspaceCheckoutBinding>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        workspace_checkout_by_id(&conn, &partition_id)
    }

    /// Look up the checkout bound to an orbit dir, if one is bound.
    ///
    /// `orbit_dir` is UNIQUE in `workspace_checkout_bindings`, so this answers
    /// "which partition does task state under this directory already live in?"
    /// without attempting a bind.
    pub fn find_checkout_by_orbit_dir(
        &self,
        orbit_dir: &Path,
    ) -> Result<Option<WorkspaceCheckoutBinding>, OrbitError> {
        let orbit_dir = normalize_path(orbit_dir);
        let conn = self.read()?;
        workspace_by_orbit_dir(&conn, &orbit_dir)
    }

    /// Resolve a checkout before a task operation touches checkout-local files.
    pub fn require_workspace_checkout(
        &self,
        partition_id: &str,
    ) -> Result<WorkspaceCheckoutBinding, OrbitError> {
        self.find_workspace_checkout(partition_id)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "workspace '{partition_id}' has no local checkout binding; link or initialize a checkout before running this file operation"
            ))
        })
    }
}
