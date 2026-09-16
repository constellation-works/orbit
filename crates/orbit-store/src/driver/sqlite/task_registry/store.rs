use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    CYCLIC_RELATION_TYPES, ORB_TASK_ID_MAX, TaskEnvelopeV2, TaskRelation, TaskRelationEdge,
    TaskRelationType, TaskStatus, complexity_bucket, complexity_bucket_ord, format_task_id,
    is_valid_orb_task_id, is_valid_task_id_prefix, normalize_task_tags, parse_task_number,
    task_id_prefix, validate_orb_task_id, validate_task_relations_for_source,
};
use rusqlite::{Connection, TransactionBehavior, params, params_from_iter};

use super::partition_id::{next_partition_id_candidate, sanitize_slug, validate_partition_id};
use super::queries::{
    TaskIndexWriter, decode_task_bundle_binding, decode_workspace_checkout_binding,
    task_bundle_by_id, task_ids_for_workspace, workspace_by_id, workspace_by_orbit_dir,
    workspace_checkout_by_id, workspace_checkout_by_paths,
};
use super::schema::{
    apply_schema, assert_readable_schema, assert_registry_user_version, ensure_compatible_schema,
    registry_user_version,
};
use super::util::{now_string, parse_relation_type_name, path_to_string, relation_type_name};
use crate::contracts::{
    AllocatorSeedOutcome, BindWorkspaceParams, DanglingRelationTarget, RegisterWorkspaceParams,
    TaskBundleBinding, TaskCompletionByComplexity, TaskIndexFilter, WorkspaceBinding,
    WorkspaceCheckoutBinding,
};
use crate::driver::sqlite::read_pool::{ReadGuard, ReadPool};
use crate::fs::path_safety::normalize_path;

/// Task registry handle: one writer connection behind a mutex plus a
/// read-only connection pool, the same shape as [`crate::Store`]. Under WAL
/// readers on their own connections never queue behind the writer, so a
/// long `replace_task_index` transaction no longer stalls every list/show
/// that shares the registry file (orbit-web, the MCP server).
#[derive(Clone)]
pub struct TaskRegistryStore {
    /// The single writer connection. Every mutating statement and every
    /// transaction serializes here; reads go through [`Self::read`].
    pub(super) conn: Arc<Mutex<Connection>>,
    /// Read pool for writable registries. `None` when the registry was
    /// opened read-only, where reads fall back to the writer connection
    /// (see [`crate::driver::sqlite::read_pool`]).
    readers: Option<Arc<ReadPool>>,
    workspaces_dir: PathBuf,
}

impl TaskRegistryStore {
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        let registry_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let workspaces_dir = normalize_path(&registry_dir.join("workspaces"));
        let opened = orbit_common::storage::sqlite::open_private(path)?;
        let mut conn = opened.connection;
        let read_only = opened.read_only;
        if !read_only {
            orbit_common::storage::sqlite::create_private_dir_all(&workspaces_dir)?;
        }
        // The registry is the commit point that makes a created task official, so
        // its writes must be durable against power loss the moment they ack. WAL's
        // synchronous=NORMAL default only fsyncs the WAL at checkpoint, leaving an
        // acked register_task_bundle exposed to rollback on a hard reset. FULL
        // fsyncs the WAL on every commit, closing that window. The registry is
        // low-write (≈one commit per task create/bind/unregister), so the extra
        // fsync cost is negligible. Scoped to this connection only — the shared
        // Store::open stays at NORMAL for higher-write stores.
        if !read_only && let Err(error) = conn.pragma_update(None, "synchronous", "FULL") {
            let mapped = OrbitError::Store(format!("failed to set synchronous=FULL: {error}"));
            if mapped.is_readonly_or_access_failure() {
                orbit_common::tracing::warn!(
                    target: "orbit.store.task_registry",
                    path = %path.display(),
                    error = %error,
                    "could not set synchronous=FULL on a read-only task registry; continuing for reads"
                );
            } else {
                return Err(mapped);
            }
        }
        // Setup, migration and recovery all need a write transaction, so a
        // registry opened for observation is either already readable as-is or
        // reported as needing writable storage. Attempting them here is how a
        // read-only mount produced `attempt to write a readonly database`.
        if read_only {
            assert_readable_schema(&conn, path)?;
        } else {
            if registry_user_version(&conn)? < super::REGISTRY_SCHEMA_VERSION {
                apply_schema(&conn)?;
            }
            ensure_compatible_schema(&mut conn, path)?;
            assert_registry_user_version(&conn)?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: (!read_only).then(|| Arc::new(ReadPool::new(path.to_path_buf()))),
            workspaces_dir,
        })
    }

    /// Check out a read connection: a pooled query-only reader for writable
    /// registries, or the writer mutex for read-only opens. Never takes the
    /// writer mutex on the pooled path, so reads complete while a write
    /// transaction is open.
    pub(super) fn read(&self) -> Result<ReadGuard<'_>, OrbitError> {
        match &self.readers {
            Some(pool) => {
                let (generation, connection) = pool.checkout()?;
                Ok(ReadGuard::pooled(generation, connection, pool))
            }
            None => {
                let guard = self
                    .conn
                    .lock()
                    .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
                Ok(ReadGuard::Writer(guard))
            }
        }
    }

    #[cfg(test)]
    pub(super) fn reader_pool_for_test(&self) -> Option<&ReadPool> {
        self.readers.as_deref()
    }

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

    /// Allocate a monotonic local task ID.
    ///
    /// Allocation commits independently from bundle registration. A crash between
    /// allocation and registration can leave numeric holes; those holes are expected
    /// and are not reused.
    pub fn allocate_task_id(&self, partition_id: &str) -> Result<String, OrbitError> {
        self.allocate_task_ids(partition_id, 1)?
            .pop()
            .ok_or_else(|| OrbitError::Store("task id allocation returned no id".into()))
    }

    /// Allocate `count` consecutive task IDs with a single counter bump.
    ///
    /// Same contract as [`allocate_task_id`](Self::allocate_task_id) — the
    /// reservation commits before anything is registered against it, and ids a
    /// crash leaves unused become holes rather than being reused. Reserving the
    /// whole run at once is what keeps a bulk renumber at one commit (and one
    /// WAL fsync under `synchronous=FULL`) instead of one per task.
    pub fn allocate_task_ids(
        &self,
        partition_id: &str,
        count: usize,
    ) -> Result<Vec<String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let count = i64::try_from(count).map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if workspace_by_id(&tx, &partition_id)?.is_none() {
            return Err(OrbitError::not_found(NotFoundKind::Workspace, partition_id));
        }

        let (next, task_prefix): (i64, String) = tx
            .query_row(
                "SELECT next_number, task_prefix FROM allocator_state WHERE authority = 'local'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        // `next + count - 1` is the last id this reservation hands out, so a run
        // that would cross the ceiling is refused whole rather than part-served.
        if next.saturating_add(count - 1) > i64::from(ORB_TASK_ID_MAX) {
            return Err(OrbitError::Store("ORB task id allocator exhausted".into()));
        }
        tx.execute(
            "UPDATE allocator_state SET next_number = ?1, updated_at = ?2 WHERE authority = 'local'",
            params![next.saturating_add(count), now_string()],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;

        (next..next.saturating_add(count))
            .map(|number| {
                let number = u32::try_from(number).map_err(|e| OrbitError::Store(e.to_string()))?;
                format_task_id(&task_prefix, number).map_err(Into::into)
            })
            .collect()
    }

    /// Bind the allocator to the immutable prefix from this machine's host
    /// identity. A pristine legacy-default row may adopt the configured prefix;
    /// an allocator that has minted anything cannot be renamed.
    pub fn set_task_prefix(&self, task_prefix: &str) -> Result<(), OrbitError> {
        if !is_valid_task_id_prefix(task_prefix) {
            return Err(OrbitError::InvalidInput(format!(
                "task prefix '{task_prefix}' must be 2-5 uppercase ASCII letters and must not use a reserved artifact namespace"
            )));
        }
        // Runtime construction reasserts the same prefix on every command. That
        // no-op is an observation, so answer it without a write transaction:
        // BEGIN IMMEDIATE here is what made every read fail on a read-only
        // registry. Reading outside the lock is safe because a bound prefix is
        // immutable — only a pristine `ORB` row can still adopt one.
        {
            let conn = self.read()?;
            let current: String = conn
                .query_row(
                    "SELECT task_prefix FROM allocator_state WHERE authority = 'local'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            if current == task_prefix {
                return Ok(());
            }
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let (current, next): (String, i64) = tx
            .query_row(
                "SELECT task_prefix, next_number FROM allocator_state WHERE authority = 'local'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        if current == task_prefix {
            tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
            return Ok(());
        }
        let task_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM task_bundle_bindings", [], |row| {
                row.get(0)
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        if current != "ORB" || next != 0 || task_count != 0 {
            return Err(OrbitError::InvalidInput(format!(
                "task prefix is immutable after allocation begins (registry uses '{current}', host identity requests '{task_prefix}')"
            )));
        }
        tx.execute(
            "UPDATE allocator_state SET task_prefix = ?1, updated_at = ?2 WHERE authority = 'local'",
            params![task_prefix, now_string()],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// The prefix this host mints under. Task authority follows the prefix, so
    /// this is what separates a locally-owned task from a mirror of another
    /// host's task.
    pub fn local_task_prefix(&self) -> Result<String, OrbitError> {
        let conn = self.read()?;
        conn.query_row(
            "SELECT task_prefix FROM allocator_state WHERE authority = 'local'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Prefixes recognized by the local registry: the active minting prefix
    /// plus every prefix already present in registered task bundles.
    pub fn known_task_prefixes(&self) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.read()?;
        known_task_prefixes(&conn)
    }

    pub fn canonical_task_bundle_path(
        &self,
        partition_id: &str,
        task_id: &str,
    ) -> Result<PathBuf, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        validate_orb_task_id(task_id)?;
        Ok(self.workspaces_dir.join(partition_id).join(task_id))
    }

    pub fn register_task_bundle(
        &self,
        task_id: &str,
        partition_id: &str,
        canonical_path: &Path,
    ) -> Result<TaskBundleBinding, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let canonical_path = self.validated_binding_path(&partition_id, task_id, canonical_path)?;

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if workspace_by_id(&tx, &partition_id)?.is_none() {
            return Err(OrbitError::not_found(NotFoundKind::Workspace, partition_id));
        }

        upsert_task_binding(&tx, task_id, &partition_id, &canonical_path, &now_string())?;

        let binding = task_bundle_by_id(&tx, task_id)?.ok_or_else(|| {
            OrbitError::Store("failed to read inserted task bundle binding".into())
        })?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(binding)
    }

    /// Register every `(task_id, canonical_path)` binding in one transaction.
    ///
    /// Bulk publishers — reindex, migration import, publication restore — land
    /// a whole set at once, so they pay one `BEGIN IMMEDIATE` commit and one
    /// WAL fsync for the set instead of one per task. Each entry is validated
    /// exactly as [`register_task_bundle`](Self::register_task_bundle)
    /// validates its single binding, and the set is all-or-nothing: a rejected
    /// entry leaves every binding in the batch untouched.
    pub fn register_task_bundles(
        &self,
        partition_id: &str,
        bundles: &[(String, PathBuf)],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if bundles.is_empty() {
            return Ok(());
        }
        let mut rows = Vec::with_capacity(bundles.len());
        for (task_id, canonical_path) in bundles {
            rows.push((
                task_id,
                self.validated_binding_path(&partition_id, task_id, canonical_path)?,
            ));
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if workspace_by_id(&tx, &partition_id)?.is_none() {
            return Err(OrbitError::not_found(NotFoundKind::Workspace, partition_id));
        }

        let now = now_string();
        for (task_id, canonical_path) in &rows {
            upsert_task_binding(&tx, task_id, &partition_id, canonical_path, &now)?;
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// A binding may only ever name the path this registry derives for its id,
    /// so both registration paths resolve and check it the same way.
    fn validated_binding_path(
        &self,
        partition_id: &str,
        task_id: &str,
        canonical_path: &Path,
    ) -> Result<PathBuf, OrbitError> {
        validate_orb_task_id(task_id)?;
        let canonical_path = normalize_path(canonical_path);
        let expected_path =
            normalize_path(&self.canonical_task_bundle_path(partition_id, task_id)?);
        if canonical_path != expected_path {
            return Err(OrbitError::InvalidInput(format!(
                "canonical path for task '{task_id}' in workspace '{partition_id}' must be '{}', got '{}'",
                expected_path.display(),
                canonical_path.display()
            )));
        }
        Ok(canonical_path)
    }

    pub fn unregister_task_bundle(
        &self,
        task_id: &str,
        partition_id: &str,
    ) -> Result<bool, OrbitError> {
        validate_orb_task_id(task_id)?;
        let partition_id = validate_partition_id(partition_id)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let Some(binding) = task_bundle_by_id(&tx, task_id)? else {
            return Ok(false);
        };
        if binding.partition_id != partition_id {
            return Ok(false);
        }

        tx.execute(
            "DELETE FROM task_bundle_relations
             WHERE source_task_id = ?1 OR target_task_id = ?1",
            [task_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.execute("DELETE FROM task_bundle_tags WHERE task_id = ?1", [task_id])
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.execute(
            "DELETE FROM task_bundle_index WHERE task_id = ?1",
            [task_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        let deleted = tx
            .execute(
                "DELETE FROM task_bundle_bindings
                 WHERE task_id = ?1 AND workspace_id = ?2",
                params![task_id, partition_id],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(deleted > 0)
    }

    pub fn tasks_for_workspace(
        &self,
        partition_id: &str,
    ) -> Result<Vec<TaskBundleBinding>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT task_id, workspace_id, canonical_path, created_at, updated_at
                 FROM task_bundle_bindings
                 WHERE workspace_id = ?1
                 ORDER BY task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([partition_id], decode_task_bundle_binding)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn replace_task_index(
        &self,
        partition_id: &str,
        envelope: &TaskEnvelopeV2,
    ) -> Result<(), OrbitError> {
        self.replace_task_indexes(partition_id, std::slice::from_ref(envelope))
    }

    /// Replace the index rows for exactly `envelopes` in one transaction,
    /// leaving every other task's rows in the workspace untouched.
    ///
    /// The partial counterpart of
    /// [`replace_workspace_task_indexes`](Self::replace_workspace_task_indexes):
    /// a repair pass that could only read some of a workspace's bundles has to
    /// reindex the healthy ones without dropping rows it cannot rebuild.
    /// Relations are validated against the batch as a unit, so members may
    /// reference each other in any order.
    pub fn replace_task_indexes(
        &self,
        partition_id: &str,
        envelopes: &[TaskEnvelopeV2],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if envelopes.is_empty() {
            return Ok(());
        }
        for envelope in envelopes {
            envelope.validate()?;
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        for envelope in envelopes {
            let binding = task_bundle_by_id(&tx, &envelope.id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, envelope.id.clone()))?;
            if binding.partition_id != partition_id {
                return Err(OrbitError::InvalidInput(format!(
                    "task '{}' is registered to workspace '{}', not '{}'",
                    envelope.id, binding.partition_id, partition_id
                )));
            }
        }

        validate_replacement_relations(&tx, &partition_id, envelopes)?;

        for envelope in envelopes {
            tx.execute(
                "DELETE FROM task_bundle_tags WHERE task_id = ?1",
                [&envelope.id],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
            tx.execute(
                "DELETE FROM task_bundle_relations WHERE source_task_id = ?1",
                [&envelope.id],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        }

        {
            let mut writer = TaskIndexWriter::prepare(&tx)?;
            for envelope in envelopes {
                writer.write(&partition_id, envelope)?;
            }
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn replace_workspace_task_indexes(
        &self,
        partition_id: &str,
        envelopes: &[TaskEnvelopeV2],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        for envelope in envelopes {
            envelope.validate()?;
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let registered = task_ids_for_workspace(&tx, &partition_id)?;
        let requested = envelopes
            .iter()
            .map(|envelope| envelope.id.clone())
            .collect::<BTreeSet<_>>();
        if registered != requested {
            return Err(OrbitError::Store(format!(
                "task index rebuild for workspace '{}' expected registered ids {:?}, got {:?}",
                partition_id, registered, requested
            )));
        }

        validate_replacement_relations(&tx, &partition_id, envelopes)?;

        tx.execute(
            "DELETE FROM task_bundle_tags WHERE workspace_id = ?1",
            [&partition_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.execute(
            "DELETE FROM task_bundle_relations WHERE workspace_id = ?1",
            [&partition_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.execute(
            "DELETE FROM task_bundle_index WHERE workspace_id = ?1",
            [&partition_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;

        {
            let mut writer = TaskIndexWriter::prepare(&tx)?;
            for envelope in envelopes {
                writer.write(&partition_id, envelope)?;
            }
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn indexed_task_versions_for_workspace(
        &self,
        partition_id: &str,
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT task_id, updated_at FROM task_bundle_index
                 WHERE workspace_id = ?1
                 ORDER BY task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([partition_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn indexed_task_count_for_workspace(
        &self,
        partition_id: &str,
    ) -> Result<usize, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_bundle_index WHERE workspace_id = ?1",
                [partition_id],
                |row| row.get(0),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        usize::try_from(count).map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Status projection for every task in the coordination registry. Task
    /// lists remain workspace-scoped; dependency readiness is global because
    /// ORB task IDs are globally unique.
    pub fn global_task_status_index(&self) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare("SELECT task_id, status FROM task_bundle_index ORDER BY task_id ASC")
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut statuses = BTreeMap::new();
        for row in rows {
            let (task_id, raw_status) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
            let status = raw_status.parse::<TaskStatus>().map_err(|e| {
                OrbitError::Store(format!(
                    "invalid indexed status '{raw_status}' for task '{task_id}': {e}"
                ))
            })?;
            statuses.insert(task_id, status);
        }
        Ok(statuses)
    }

    /// True when this workspace still has index rows whose `complexity` column
    /// was added by migration and has not been written yet (`NULL`).
    pub fn workspace_index_has_null_complexity(
        &self,
        partition_id: &str,
    ) -> Result<bool, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let exists: i64 = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM task_bundle_index
                    WHERE workspace_id = ?1 AND complexity IS NULL
                 )",
                [partition_id],
                |row| row.get(0),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(exists != 0)
    }

    /// Status counts grouped by complexity bucket. `NULL`, empty, and
    /// `unassessed` index values all become the named `unset` bucket — see
    /// [`complexity_bucket`].
    pub fn completion_by_complexity(
        &self,
        partition_id: &str,
    ) -> Result<Vec<TaskCompletionByComplexity>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT complexity, status, COUNT(*)
                 FROM task_bundle_index
                 WHERE workspace_id = ?1
                 GROUP BY complexity, status",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([&partition_id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let mut by_bucket: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for row in rows {
            let (raw_complexity, status, count) =
                row.map_err(|e| OrbitError::Store(e.to_string()))?;
            let bucket = complexity_bucket(raw_complexity.as_deref()).to_string();
            *by_bucket
                .entry(bucket)
                .or_default()
                .entry(status)
                .or_insert(0) += count;
        }

        let mut out: Vec<TaskCompletionByComplexity> = by_bucket
            .into_iter()
            .map(|(complexity, by_status)| {
                let total = by_status.values().sum();
                TaskCompletionByComplexity {
                    complexity,
                    total,
                    by_status,
                }
            })
            .collect();
        out.sort_by(|left, right| {
            complexity_bucket_ord(&left.complexity).cmp(&complexity_bucket_ord(&right.complexity))
        });
        Ok(out)
    }

    /// `task_id →` complexity bucket for every indexed task in the workspace.
    /// Buckets match [`Self::completion_by_complexity`], so an `unassessed`
    /// task reports `unset` here too.
    pub fn complexity_by_task_id(
        &self,
        partition_id: &str,
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT task_id, complexity FROM task_bundle_index
                 WHERE workspace_id = ?1
                 ORDER BY task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([partition_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut map = BTreeMap::new();
        for row in rows {
            let (task_id, raw) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
            map.insert(task_id, complexity_bucket(raw.as_deref()).to_string());
        }
        Ok(map)
    }

    /// Validate task relations against every workspace in the coordination
    /// registry without mutating allocator, bundle, or index state.
    pub fn validate_task_relations(
        &self,
        partition_id: &str,
        source_task_id: &str,
        relations: &[TaskRelation],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        validate_orb_task_id(source_task_id)?;
        let conn = self.read()?;
        validate_relations_in_registry(
            &conn,
            &partition_id,
            source_task_id,
            relations,
            &[source_task_id.to_string()],
            &[],
        )
    }

    /// Preflight relation targets for a task whose globally allocated source ID
    /// does not exist yet. This runs before allocation so a missing target
    /// cannot consume an ID or write a partial bundle.
    pub fn validate_new_task_relation_targets(
        &self,
        partition_id: &str,
        relations: &[TaskRelation],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        validate_relation_targets_exist(&conn, &partition_id, None, relations)
    }

    /// Audit the coordination registry for relation edges whose target is a
    /// valid `ORB-` task id with no registered task bundle — the "grandfathered"
    /// relations that make [`validate_relation_targets_exist`] reject an index
    /// rebuild (ORB-10305). Scans indexed relation rows across the whole
    /// registry, or a single workspace when `partition_id` is set, so these
    /// targets can be surfaced (and cleaned) proactively instead of only when a
    /// rebuild trips over them.
    ///
    /// Mirrors the validator's resolution semantics: only `ORB-` targets can be
    /// unresolved; friction / ADR targets that `produces`/`resolves`
    /// edges legitimately allow to dangle are excluded.
    pub fn dangling_relation_targets(
        &self,
        partition_id: Option<&str>,
    ) -> Result<Vec<DanglingRelationTarget>, OrbitError> {
        let partition_id = partition_id.map(validate_partition_id).transpose()?;
        let conn = self.read()?;

        let mut sql = String::from(
            "SELECT r.workspace_id, r.source_task_id, r.relation_type, r.target_task_id
             FROM task_bundle_relations r
             LEFT JOIN task_bundle_bindings b ON b.task_id = r.target_task_id
             WHERE b.task_id IS NULL",
        );
        let mut values: Vec<String> = Vec::new();
        if let Some(partition_id) = &partition_id {
            sql.push_str(" AND r.workspace_id = ?1");
            values.push(partition_id.clone());
        }
        sql.push_str(
            " ORDER BY r.workspace_id, r.source_task_id, r.relation_type, r.target_task_id",
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let mut dangling = Vec::new();
        let known_prefixes = known_task_prefixes(&conn)?;
        for row in rows {
            let (partition_id, source_task_id, relation_type, target_task_id) =
                row.map_err(|e| OrbitError::Store(e.to_string()))?;
            // Non-task artifact targets and foreign-prefix task references are
            // both allowed to remain unresolved here. Only a locally known
            // prefix can be a dangling relation in this registry.
            if !is_valid_orb_task_id(&target_task_id) {
                continue;
            }
            let Some(prefix) = task_id_prefix(&target_task_id) else {
                continue;
            };
            if !known_prefixes.contains(prefix) {
                continue;
            }
            dangling.push(DanglingRelationTarget {
                partition_id,
                source_task_id,
                relation_type,
                target_task_id,
            });
        }
        Ok(dangling)
    }

    pub fn indexed_task_ids_filtered(
        &self,
        partition_id: &str,
        filter: &TaskIndexFilter,
    ) -> Result<Vec<String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let required_tags = normalize_task_tags(filter.tags.clone());
        let mut sql = String::from("SELECT task_id FROM task_bundle_index WHERE workspace_id = ?");
        let mut values = vec![partition_id.clone()];
        if let Some(status) = filter.status {
            sql.push_str(" AND status = ?");
            values.push(status.to_string());
        }
        if let Some(priority) = filter.priority {
            sql.push_str(" AND priority = ?");
            values.push(priority.to_string());
        }
        if let Some(job_run_id) = &filter.job_run_id {
            sql.push_str(" AND job_run_id = ?");
            values.push(job_run_id.clone());
        }
        sql.push_str(" ORDER BY created_at DESC, task_id ASC");

        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut ids = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if required_tags.is_empty() {
            return Ok(ids);
        }

        let mut tag_sets = Vec::new();
        let mut tag_stmt = conn
            .prepare(
                "SELECT task_id FROM task_bundle_tags
                 WHERE workspace_id = ?1 AND tag = ?2
                 ORDER BY task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        for tag in required_tags {
            let rows = tag_stmt
                .query_map(params![&partition_id, &tag], |row| row.get::<_, String>(0))
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            let set = rows
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            tag_sets.push(set);
        }

        ids.retain(|id| tag_sets.iter().all(|set| set.contains(id)));
        Ok(ids)
    }

    pub fn indexed_relation_targets(
        &self,
        partition_id: &str,
        source_task_id: &str,
        relation_type: TaskRelationType,
    ) -> Result<Vec<String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        validate_orb_task_id(source_task_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT target_task_id FROM task_bundle_relations
                 WHERE workspace_id = ?1 AND source_task_id = ?2 AND relation_type = ?3
                 ORDER BY target_task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    partition_id,
                    source_task_id,
                    relation_type_name(relation_type)
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn indexed_relation_sources(
        &self,
        partition_id: &str,
        target_task_id: &str,
        relation_type: TaskRelationType,
    ) -> Result<Vec<String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        validate_orb_task_id(target_task_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT source_task_id FROM task_bundle_relations
                 WHERE workspace_id = ?1 AND target_task_id = ?2 AND relation_type = ?3
                 ORDER BY source_task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    partition_id,
                    target_task_id,
                    relation_type_name(relation_type)
                ],
                |row| row.get(0),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
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
            .prepare(
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

    /// Root directory that holds per-workspace canonical bundle trees
    /// (`<global>/tasks/workspaces`). Used by task-migration tooling to locate
    /// and enumerate on-disk bundles for a workspace.
    pub(crate) fn workspaces_dir(&self) -> &Path {
        &self.workspaces_dir
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
            .prepare("SELECT workspace_id FROM workspace_bindings")
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

    /// Look up a task-bundle binding by task id. Task ids are a global primary
    /// key in the registry, so this reports collisions across every workspace —
    /// exactly what import conflict resolution needs.
    pub fn find_task_binding(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskBundleBinding>, OrbitError> {
        validate_orb_task_id(task_id)?;
        let conn = self.read()?;
        task_bundle_by_id(&conn, task_id)
    }

    /// Current value of the local allocator counter (`next_number`) — the id the
    /// next [`allocate_task_id`](Self::allocate_task_id) call would hand out.
    pub fn allocator_next_number(&self) -> Result<u32, OrbitError> {
        let conn = self.read()?;
        read_allocator_next_number(&conn)
    }

    /// Highest numeric task id registered in the whole registry, if any.
    pub fn max_registered_task_number(&self) -> Result<Option<u32>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
            .prepare("SELECT task_id FROM task_bundle_bindings")
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut max = None;
        for id in ids {
            let id = id.map_err(|e| OrbitError::Store(e.to_string()))?;
            if let Some(number) = parse_orb_task_number(&id) {
                max = Some(max.map_or(number, |current: u32| current.max(number)));
            }
        }
        Ok(max)
    }

    /// Seed the allocator so the next allocated id is `start`.
    ///
    /// Only ever moves the counter *forward*: if `start` is below the current
    /// `next_number` the call is refused, so two machines can be handed disjoint
    /// id ranges without risk of silently rewinding a live counter.
    pub fn seed_allocator_start(&self, start: u32) -> Result<AllocatorSeedOutcome, OrbitError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let previous = read_allocator_next_number(&tx)?;
        if start < previous {
            return Err(OrbitError::InvalidInput(format!(
                "tasks.id_start {start} would lower the allocator below its current position {previous}; the counter only moves forward"
            )));
        }
        let changed = start != previous;
        if changed {
            set_allocator_next_number(&tx, start)?;
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(AllocatorSeedOutcome {
            previous,
            next: start,
            changed,
        })
    }

    /// Ensure the allocator will not hand out any id `< min_next`. Never lowers
    /// the counter. Used after import/reindex to move `next_number` past the
    /// highest landed id.
    pub fn bump_allocator_to_at_least(&self, min_next: u32) -> Result<(), OrbitError> {
        let target = min_next;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let previous = read_allocator_next_number(&tx)?;
        if target > previous {
            set_allocator_next_number(&tx, target)?;
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Restore an allocator value after a larger workflow failed after its
    /// final allocator advance. This is deliberately crate-private and guarded
    /// by the exact value the workflow observed after advancing, so it cannot
    /// rewind over a concurrent allocation.
    pub(crate) fn restore_allocator_after_failed_restore(
        &self,
        expected_current: u32,
        previous: u32,
    ) -> Result<(), OrbitError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let current = read_allocator_next_number(&tx)?;
        if current != expected_current {
            return Err(OrbitError::Store(format!(
                "cannot roll back failed publication restore allocator: expected {expected_current}, found {current}"
            )));
        }
        if previous > current {
            return Err(OrbitError::Store(format!(
                "invalid publication restore allocator rollback from {current} to {previous}"
            )));
        }
        if previous != current {
            set_allocator_next_number(&tx, previous)?;
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }
}

fn read_allocator_next_number(conn: &Connection) -> Result<u32, OrbitError> {
    let next: i64 = conn
        .query_row(
            "SELECT next_number FROM allocator_state WHERE authority = 'local'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    u32::try_from(next).map_err(|e| OrbitError::Store(e.to_string()))
}

fn set_allocator_next_number(conn: &Connection, value: u32) -> Result<(), OrbitError> {
    conn.execute(
        "UPDATE allocator_state SET next_number = ?1, updated_at = ?2 WHERE authority = 'local'",
        params![i64::from(value), now_string()],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(())
}

fn upsert_task_binding(
    tx: &Connection,
    task_id: &str,
    partition_id: &str,
    canonical_path: &Path,
    now: &str,
) -> Result<(), OrbitError> {
    tx.execute(
        "INSERT INTO task_bundle_bindings (
            task_id, workspace_id, canonical_path, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?4)
        ON CONFLICT(task_id) DO UPDATE SET
            workspace_id = excluded.workspace_id,
            canonical_path = excluded.canonical_path,
            updated_at = excluded.updated_at",
        params![task_id, partition_id, path_to_string(canonical_path), now],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(())
}

/// Validate a replacement set as a unit: its members' currently indexed edges
/// are ignored in favour of the edges it is about to write, so the set's own
/// cross-references resolve regardless of the order rows land in.
fn validate_replacement_relations(
    conn: &Connection,
    partition_id: &str,
    envelopes: &[TaskEnvelopeV2],
) -> Result<(), OrbitError> {
    if envelopes.is_empty() {
        return Ok(());
    }
    if workspace_by_id(conn, partition_id)?.is_none() {
        return Err(OrbitError::not_found(
            NotFoundKind::Workspace,
            partition_id.to_string(),
        ));
    }

    let replacement_edges = envelopes
        .iter()
        .flat_map(task_relation_edges)
        .collect::<Vec<_>>();
    let replacement_sources = envelopes
        .iter()
        .map(|envelope| envelope.id.clone())
        .collect::<BTreeSet<_>>();

    validate_replacement_relation_targets(conn, partition_id, envelopes)?;

    let seeds = cycle_walk_seeds(&[], &replacement_edges);
    let mut validation_edges = reachable_cycle_family_edges(conn, &seeds)?
        .into_iter()
        .filter(|edge| {
            !replacement_sources.contains(&edge.source) && is_valid_orb_task_id(&edge.target)
        })
        .collect::<Vec<_>>();
    validation_edges.extend(replacement_edges);

    for envelope in envelopes {
        validate_task_relations_for_source(&envelope.id, &envelope.relations, &validation_edges)
            .map_err(OrbitError::from)?;
    }
    Ok(())
}

/// Resolve every replacement relation target with one primary-key query, then
/// use one materialized prefix set for any target that query did not find.
fn validate_replacement_relation_targets(
    conn: &Connection,
    source_workspace_id: &str,
    envelopes: &[TaskEnvelopeV2],
) -> Result<(), OrbitError> {
    let candidates = envelopes
        .iter()
        .flat_map(|envelope| {
            envelope
                .relations
                .iter()
                .filter(|relation| {
                    is_valid_orb_task_id(&relation.target) && relation.target != envelope.id
                })
                .map(|relation| relation.target.clone())
        })
        .collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return Ok(());
    }

    let registered = registered_task_ids(conn, &candidates)?;
    if registered.len() == candidates.len() {
        return Ok(());
    }
    let known_prefixes = known_task_prefixes(conn)?;
    for envelope in envelopes {
        for relation in &envelope.relations {
            if !candidates.contains(&relation.target) || registered.contains(&relation.target) {
                continue;
            }
            let Some(prefix) = task_id_prefix(&relation.target) else {
                continue;
            };
            if !known_prefixes.contains(prefix) {
                continue;
            }
            return Err(OrbitError::InvalidInput(format!(
                "task relation target '{}' from workspace '{}' does not resolve in the coordination registry",
                relation.target, source_workspace_id
            )));
        }
    }
    Ok(())
}

fn validate_relations_in_registry(
    conn: &Connection,
    source_workspace_id: &str,
    source_task_id: &str,
    relations: &[TaskRelation],
    replaced_sources: &[String],
    replacement_edges: &[TaskRelationEdge],
) -> Result<(), OrbitError> {
    validate_relation_targets_exist(conn, source_workspace_id, Some(source_task_id), relations)?;

    let replaced_sources = replaced_sources.iter().collect::<BTreeSet<_>>();
    let seeds = cycle_walk_seeds(relations, replacement_edges);
    let mut existing_edges = reachable_cycle_family_edges(conn, &seeds)?
        .into_iter()
        .filter(|edge| {
            !replaced_sources.contains(&edge.source) && is_valid_orb_task_id(&edge.target)
        })
        .collect::<Vec<_>>();
    existing_edges.extend(
        replacement_edges
            .iter()
            .filter(|edge| edge.source != source_task_id)
            .cloned(),
    );
    validate_task_relations_for_source(source_task_id, relations, &existing_edges)
        .map_err(Into::into)
}

/// Where the cycle check can start walking, and therefore what the registry
/// subgraph must be closed over.
///
/// The validator only probes reachability forward from a new relation's
/// target, so those targets are the primary seeds. A replacement edge is not
/// in the registry yet, so any path crossing one resumes at its target —
/// seeding those as well keeps the fetched subgraph closed over the batch's
/// own unwritten edges. Only cycle-family targets matter; the other relation
/// types are queryable metadata the walk never follows.
fn cycle_walk_seeds(
    relations: &[TaskRelation],
    replacement_edges: &[TaskRelationEdge],
) -> BTreeSet<String> {
    let relation_targets = relations
        .iter()
        .filter(|relation| CYCLIC_RELATION_TYPES.contains(&relation.relation_type))
        .map(|relation| relation.target.clone());
    let replacement_targets = replacement_edges
        .iter()
        .filter(|edge| CYCLIC_RELATION_TYPES.contains(&edge.relation_type))
        .map(|edge| edge.target.clone());
    relation_targets
        .chain(replacement_targets)
        .filter(|target| is_valid_orb_task_id(target))
        .collect()
}

/// The subgraph walk from [`reachable_cycle_family_edges`], as SQL over
/// `seed_count` bound seed ids.
///
/// Separate from its caller so `relation_subgraph_query_stays_indexed` can put
/// it through `EXPLAIN QUERY PLAN`; both of its joins have to resolve as index
/// searches.
pub(super) fn reachable_cycle_family_sql(seed_count: usize) -> String {
    let seed_rows = (1..=seed_count)
        .map(|index| format!("SELECT ?{index}"))
        .collect::<Vec<_>>()
        .join(" UNION ");
    let families = CYCLIC_RELATION_TYPES
        .iter()
        .map(|relation_type| format!("'{}'", relation_type_name(*relation_type)))
        .collect::<Vec<_>>()
        .join(", ");
    // `UNION` (not `UNION ALL`) is what terminates the walk on an existing
    // cycle: the registry is not guaranteed acyclic from this query's side.
    //
    // The collecting select uses `CROSS JOIN` purely to pin the join order.
    // SQLite has no cardinality estimate for a recursive CTE, and its choice
    // here is not stable: with a plain join the same statement plans as a
    // `SEARCH` against this registry's schema but as a full `SCAN` of
    // `task_bundle_relations` against a reduced one. A scan is exactly the
    // cost this query exists to avoid, so the order is not left to the
    // planner. `relation_subgraph_query_stays_indexed` checks the result.
    format!(
        "WITH RECURSIVE reachable(task_id) AS (
             {seed_rows}
             UNION
             SELECT edge.target_task_id
             FROM task_bundle_relations AS edge
             JOIN reachable ON edge.source_task_id = reachable.task_id
             WHERE edge.relation_type IN ({families})
         )
         SELECT edge.source_task_id, edge.relation_type, edge.target_task_id
         FROM reachable
         CROSS JOIN task_bundle_relations AS edge
             ON edge.source_task_id = reachable.task_id
         WHERE edge.relation_type IN ({families})
         ORDER BY edge.source_task_id, edge.relation_type, edge.target_task_id"
    )
}

/// The cycle-family edges forward-reachable from `seeds`, as one recursive
/// walk of the registry's relation rows.
///
/// This is exactly the subgraph the cycle check can observe, so fetching the
/// whole `task_bundle_relations` table on every task write only ever bought
/// rows the validator would ignore. The walk deliberately crosses workspace
/// boundaries — a relation may target a task in another workspace, and a cycle
/// through one is still a cycle — so it cannot be narrowed to a
/// `workspace_id = ?` filter.
///
/// Rows belonging to a replaced source are filtered by the caller rather than
/// here: traversing through a stale edge can only over-collect real edges, and
/// an over-collected edge whose only link into the graph was that stale edge
/// is unreachable from the seeds and cannot change the verdict.
fn reachable_cycle_family_edges(
    conn: &Connection,
    seeds: &BTreeSet<String>,
) -> Result<Vec<TaskRelationEdge>, OrbitError> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&reachable_cycle_family_sql(seeds.len()))
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map(params_from_iter(seeds.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let mut edges = Vec::new();
    for row in rows {
        let (source, relation_type, target) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
        edges.push(TaskRelationEdge {
            source,
            relation_type: parse_relation_type_name(&relation_type).map_err(OrbitError::Store)?,
            target,
        });
    }
    Ok(edges)
}

fn validate_relation_targets_exist(
    conn: &Connection,
    source_workspace_id: &str,
    source_task_id: Option<&str>,
    relations: &[TaskRelation],
) -> Result<(), OrbitError> {
    if workspace_by_id(conn, source_workspace_id)?.is_none() {
        return Err(OrbitError::not_found(
            NotFoundKind::Workspace,
            source_workspace_id.to_string(),
        ));
    }
    let candidates = relations
        .iter()
        .filter(|relation| {
            is_valid_orb_task_id(&relation.target)
                && source_task_id != Some(relation.target.as_str())
        })
        .map(|relation| relation.target.clone())
        .collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return Ok(());
    }

    let registered = registered_task_ids(conn, &candidates)?;
    let active_prefix = active_task_prefix(conn)?;
    let mut probed: BTreeMap<&str, bool> = BTreeMap::new();
    for relation in relations {
        if !candidates.contains(&relation.target) || registered.contains(&relation.target) {
            continue;
        }
        let Some(prefix) = task_id_prefix(&relation.target) else {
            continue;
        };
        // An unresolvable target under a prefix this registry has never issued
        // is a foreign id, not a dangling edge; only a known prefix means the
        // task should have been here.
        let known = if prefix == active_prefix {
            true
        } else {
            match probed.get(prefix) {
                Some(known) => *known,
                None => {
                    let known = task_prefix_is_registered(conn, prefix)?;
                    probed.insert(prefix, known);
                    known
                }
            }
        };
        if !known {
            continue;
        }
        return Err(OrbitError::InvalidInput(format!(
            "task relation target '{}' from workspace '{}' does not resolve in the coordination registry",
            relation.target, source_workspace_id
        )));
    }
    Ok(())
}

/// Every prefix this registry recognizes, materialized in full.
///
/// Only callers that already scan the registry want this — the audit in
/// [`TaskRegistryStore::dangling_relation_targets`] tests many rows against the
/// set, and [`TaskRegistryStore::known_task_prefixes`] exposes it. A write-path
/// caller checking one target's prefix must use
/// [`task_prefix_is_registered`] instead, which resolves the same predicate
/// without reading every binding.
fn known_task_prefixes(conn: &Connection) -> Result<BTreeSet<String>, OrbitError> {
    let mut prefixes = BTreeSet::from([active_task_prefix(conn)?]);
    let mut statement = conn
        .prepare("SELECT task_id FROM task_bundle_bindings")
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    for id in ids {
        let id = id.map_err(|e| OrbitError::Store(e.to_string()))?;
        if let Some(prefix) = task_id_prefix(&id) {
            prefixes.insert(prefix.to_string());
        }
    }
    Ok(prefixes)
}

fn active_task_prefix(conn: &Connection) -> Result<String, OrbitError> {
    conn.query_row(
        "SELECT task_prefix FROM allocator_state WHERE authority = 'local'",
        [],
        |row| row.get(0),
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
}

/// Which of `task_ids` have a registered bundle, resolved by one prepared
/// statement of primary-key seeks instead of one statement per relation.
fn registered_task_ids(
    conn: &Connection,
    task_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, OrbitError> {
    let placeholders = (1..=task_ids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn
        .prepare(&format!(
            "SELECT task_id FROM task_bundle_bindings WHERE task_id IN ({placeholders})"
        ))
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map(params_from_iter(task_ids.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let mut registered = BTreeSet::new();
    for row in rows {
        registered.insert(row.map_err(|e| OrbitError::Store(e.to_string()))?);
    }
    Ok(registered)
}

/// Has this registry ever registered a task under `prefix`?
///
/// Probes the `task_bundle_bindings` primary key over the half-open range of
/// ids beginning `<prefix>-`, rather than reading every binding and re-parsing
/// its prefix. `'.'` is the byte immediately after `'-'`, so the upper bound
/// excludes exactly the ids the lower bound admits. `prefix` comes from
/// [`task_id_prefix`], so it is 2-5 uppercase ASCII letters.
fn task_prefix_is_registered(conn: &Connection, prefix: &str) -> Result<bool, OrbitError> {
    let exists: i64 = conn
        .query_row(
            TASK_PREFIX_PROBE_SQL,
            params![format!("{prefix}-"), format!("{prefix}.")],
            |row| row.get(0),
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(exists != 0)
}

/// The range probe behind [`task_prefix_is_registered`], named so
/// `relation_subgraph_query_stays_indexed` can check its plan.
pub(super) const TASK_PREFIX_PROBE_SQL: &str = "SELECT EXISTS(
     SELECT 1 FROM task_bundle_bindings
     WHERE task_id >= ?1 AND task_id < ?2
 )";

fn task_relation_edges(envelope: &TaskEnvelopeV2) -> Vec<TaskRelationEdge> {
    envelope
        .relations
        .iter()
        .filter(|relation| is_valid_orb_task_id(&relation.target))
        .map(|relation| TaskRelationEdge {
            source: envelope.id.clone(),
            relation_type: relation.relation_type,
            target: relation.target.clone(),
        })
        .collect()
}

/// Parse the numeric suffix of any canonical task id.
pub(crate) fn parse_orb_task_number(task_id: &str) -> Option<u32> {
    parse_task_number(task_id)
}
