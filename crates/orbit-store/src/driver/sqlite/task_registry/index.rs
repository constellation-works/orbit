//! Task index rows: replacement from bundle envelopes and the status,
//! complexity and version projections read from them.

use super::partition_id::validate_partition_id;
use super::queries::{TaskIndexWriter, task_bundle_by_id, task_ids_for_workspace};
use super::relations::validate_replacement_relations;
use super::store::TaskRegistryStore;
use crate::contracts::{TaskCompletionByComplexity, TaskIndexFilter, TaskTerminalTransition};
use chrono::{DateTime, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    TaskEnvelopeV2, TaskStatus, complexity_bucket, complexity_bucket_ord, normalize_task_tags,
};
use rusqlite::TransactionBehavior;
use std::collections::{BTreeMap, BTreeSet};

impl TaskRegistryStore {
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

        let mut conn = orbit_common::fs::lock_wait::mutex(&self.conn)
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

        let mut conn = orbit_common::fs::lock_wait::mutex(&self.conn)
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
            .prepare_cached(
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

    /// Status projection for every task in the coordination registry. Task
    /// lists remain workspace-scoped; dependency readiness is global because
    /// ORB task IDs are globally unique.
    pub fn global_task_status_index(&self) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached("SELECT task_id, status FROM task_bundle_index ORDER BY task_id ASC")
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

    /// Which of `task_ids` carry `tag`, in any workspace on the host. Task IDs
    /// are globally unique, so the lookup needs no partition.
    pub fn task_ids_with_tag(
        &self,
        tag: &str,
        task_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached("SELECT 1 FROM task_bundle_tags WHERE task_id = ?1 AND tag = ?2")
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut tagged = BTreeSet::new();
        for task_id in task_ids {
            if stmt
                .exists(rusqlite::params![task_id, tag])
                .map_err(|e| OrbitError::Store(e.to_string()))?
            {
                tagged.insert(task_id.clone());
            }
        }
        Ok(tagged)
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
            .prepare_cached(
                "SELECT EXISTS(
                    SELECT 1 FROM task_bundle_index
                    WHERE workspace_id = ?1 AND complexity IS NULL
                 )",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .query_row([partition_id], |row| row.get(0))
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
            .prepare_cached(
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

    /// Every terminal row in the workspace whose `updated_at` is at or after
    /// `since` (all of them for `None`), oldest first. `terminal_month` narrows
    /// the read through its index; the exact bound is applied after parsing.
    pub fn terminal_transitions(
        &self,
        partition_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<TaskTerminalTransition>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let first_month = since
            .map(|since| since.format("%Y-%m").to_string())
            .unwrap_or_default();
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT task_id, status, complexity, updated_at FROM task_bundle_index
                 WHERE workspace_id = ?1
                   AND terminal_month IS NOT NULL
                   AND terminal_month >= ?2",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([&partition_id, &first_month], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (task_id, raw_status, raw_complexity, raw_updated_at) =
                row.map_err(|e| OrbitError::Store(e.to_string()))?;
            let at = DateTime::parse_from_rfc3339(&raw_updated_at)
                .map_err(|e| {
                    OrbitError::Store(format!(
                        "invalid indexed updated_at '{raw_updated_at}' for task '{task_id}': {e}"
                    ))
                })?
                .with_timezone(&Utc);
            if since.is_some_and(|since| at < since) {
                continue;
            }
            let status = raw_status.parse::<TaskStatus>().map_err(|e| {
                OrbitError::Store(format!(
                    "invalid indexed status '{raw_status}' for task '{task_id}': {e}"
                ))
            })?;
            out.push(TaskTerminalTransition {
                status,
                complexity: complexity_bucket(raw_complexity.as_deref()).to_string(),
                at,
            });
        }
        out.sort_by_key(|transition| transition.at);
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
            .prepare_cached(
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

    /// Every id `filter` selects, newest first; see
    /// [`indexed_task_selection`](Self::indexed_task_selection) for the
    /// bounded, counted form listing uses.
    pub fn indexed_task_ids_filtered(
        &self,
        partition_id: &str,
        filter: &TaskIndexFilter,
    ) -> Result<Vec<String>, OrbitError> {
        let filter = TaskIndexFilter {
            tags: normalize_task_tags(filter.tags.clone()),
            ..filter.clone()
        };
        self.indexed_task_selection(partition_id, &filter, false, None)
            .map(|selection| selection.rows.into_iter().map(|row| row.task_id).collect())
    }
}
