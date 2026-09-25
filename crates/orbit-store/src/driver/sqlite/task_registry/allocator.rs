//! The local task-id allocator and its prefix authority.

use super::partition_id::validate_partition_id;
use super::queries::workspace_by_id;
use super::store::TaskRegistryStore;
use super::util::now_string;
use crate::contracts::AllocatorSeedOutcome;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ORB_TASK_ID_MAX, format_task_id, is_valid_task_id_prefix, parse_task_number, task_id_prefix,
};
use rusqlite::{Connection, TransactionBehavior, params, params_from_iter};
use std::collections::BTreeSet;

fn read_allocator_next_number(conn: &Connection) -> Result<u32, OrbitError> {
    let next: i64 = conn
        .prepare_cached("SELECT next_number FROM allocator_state WHERE authority = 'local'")
        .map_err(|e| OrbitError::Store(e.to_string()))?
        .query_row([], |row| row.get(0))
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

/// Every prefix this registry recognizes, materialized in full.
///
/// Only callers that already scan the registry want this — the audit in
/// [`TaskRegistryStore::dangling_relation_targets`] tests many rows against the
/// set, and [`TaskRegistryStore::known_task_prefixes`] exposes it. A write-path
/// caller checking one target's prefix must use
/// [`task_prefix_is_registered`] instead, which resolves the same predicate
/// without reading every binding.
pub(super) fn known_task_prefixes(conn: &Connection) -> Result<BTreeSet<String>, OrbitError> {
    let mut prefixes = BTreeSet::from([active_task_prefix(conn)?]);
    let mut statement = conn
        .prepare_cached("SELECT task_id FROM task_bundle_bindings")
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

pub(super) fn active_task_prefix(conn: &Connection) -> Result<String, OrbitError> {
    conn.prepare_cached("SELECT task_prefix FROM allocator_state WHERE authority = 'local'")
        .map_err(|e| OrbitError::Store(e.to_string()))?
        .query_row([], |row| row.get(0))
        .map_err(|e| OrbitError::Store(e.to_string()))
}

/// Which of `task_ids` have a registered bundle, resolved by one prepared
/// statement of primary-key seeks instead of one statement per relation.
pub(super) fn registered_task_ids(
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
pub(super) fn task_prefix_is_registered(
    conn: &Connection,
    prefix: &str,
) -> Result<bool, OrbitError> {
    let exists: i64 = conn
        .prepare_cached(TASK_PREFIX_PROBE_SQL)
        .map_err(|e| OrbitError::Store(e.to_string()))?
        .query_row(params![format!("{prefix}-"), format!("{prefix}.")], |row| {
            row.get(0)
        })
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(exists != 0)
}

/// The range probe behind [`task_prefix_is_registered`], named so
/// `relation_subgraph_query_stays_indexed` can check its plan.
pub(super) const TASK_PREFIX_PROBE_SQL: &str = "SELECT EXISTS(
     SELECT 1 FROM task_bundle_bindings
     WHERE task_id >= ?1 AND task_id < ?2
 )";

/// Parse the numeric suffix of any canonical task id.
pub(crate) fn parse_orb_task_number(task_id: &str) -> Option<u32> {
    parse_task_number(task_id)
}

impl TaskRegistryStore {
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
        conn.prepare_cached("SELECT task_prefix FROM allocator_state WHERE authority = 'local'")
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .query_row([], |row| row.get(0))
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Prefixes recognized by the local registry: the active minting prefix
    /// plus every prefix already present in registered task bundles.
    pub fn known_task_prefixes(&self) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.read()?;
        known_task_prefixes(&conn)
    }

    /// Whether this registry has ever minted or registered a task id under
    /// `prefix`.
    ///
    /// This is the authority test a cross-workspace dependency read needs: an
    /// id under a prefix this machine has never issued belongs to another
    /// host's registry, and no amount of local searching can resolve it.
    /// Bounded like the write-path check it shares —
    /// [`task_prefix_is_registered`] — rather than materializing every prefix.
    pub fn task_prefix_is_known(&self, prefix: &str) -> Result<bool, OrbitError> {
        let conn = self.read()?;
        if active_task_prefix(&conn)? == prefix {
            return Ok(true);
        }
        task_prefix_is_registered(&conn, prefix)
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
            .prepare_cached("SELECT task_id FROM task_bundle_bindings")
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
