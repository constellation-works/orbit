//! Workspace-partitioned friction records and their legacy import codec.

pub(crate) mod legacy;
mod queries;
mod sql_write;
mod stats;
pub(crate) mod taxonomy;

use crate::contract::{FrictionListFilter, FrictionReportedCount, StoredFrictionRecord};
use crate::{Snapshot, Tx};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::record::FrictionRecord;
use rusqlite::Connection;
use std::collections::BTreeMap;

/// Friction records in one host-store snapshot or write transaction. Each
/// query is explicitly scoped to a workspace because IDs are workspace-local.
pub struct FrictionRepository<'a, Access> {
    conn: &'a Connection,
    access: &'a Access,
}

impl<'a, 's> FrictionRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            access: snapshot,
        }
    }
}

impl<'a, 'tx> FrictionRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            access: tx,
        }
    }

    /// Insert or replace a friction row and its indexed tags.
    pub fn put(
        &self,
        workspace_id: &str,
        record: &FrictionRecord,
        month: &str,
        seq: u32,
        legacy_path: Option<&str>,
    ) -> Result<(), OrbitError> {
        sql_write::upsert_record(self.access, workspace_id, record, month, seq, legacy_path)
    }

    /// Next monthly sequence for a friction row, under the writer lock.
    pub fn next_month_seq(&self, workspace_id: &str, month: &str) -> Result<i64, OrbitError> {
        sql_write::next_month_seq(self.access, workspace_id, month)
    }
}

impl<Access> FrictionRepository<'_, Access> {
    /// One page of friction records.
    pub fn list(
        &self,
        workspace_id: &str,
        filter: &FrictionListFilter,
    ) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
        queries::list_records(self.conn, workspace_id, filter)
    }

    /// One friction row by its workspace-local ID.
    pub fn get(
        &self,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        queries::show_record(self.conn, workspace_id, id)
    }

    /// Whether a workspace already holds the ID, without decoding its row.
    pub(crate) fn contains(&self, workspace_id: &str, id: &str) -> Result<bool, OrbitError> {
        let count: i64 = self.conn
            .query_row(
                "SELECT COUNT(*) FROM friction_records WHERE workspace_id = ?1 AND friction_id = ?2",
                rusqlite::params![workspace_id, id],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)?;
        Ok(count > 0)
    }

    /// Workspace IDs outside this partition holding the same friction ID.
    pub fn foreign_owners_of(
        &self,
        workspace_id: &str,
        id: &str,
    ) -> Result<Vec<String>, OrbitError> {
        queries::foreign_owners_of(self.conn, workspace_id, id)
    }

    /// Oldest row with the given literal marker in its body.
    pub fn first_with_body_marker(
        &self,
        workspace_id: &str,
        marker: &str,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        queries::find_by_dedupe_marker(self.conn, workspace_id, marker)
    }

    /// Record counts by status.
    pub fn counts_by_status(
        &self,
        workspace_id: &str,
    ) -> Result<BTreeMap<String, u64>, OrbitError> {
        stats::status_counts(self.conn, workspace_id)
    }

    /// Records resolved within the given time window.
    pub fn resolved_in_window(
        &self,
        workspace_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<u64, OrbitError> {
        stats::resolved_in_window(self.conn, workspace_id, start, end)
    }

    /// Record counts by reporting model.
    pub fn counts_by_model(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<FrictionReportedCount>, OrbitError> {
        stats::counts_by_model(self.conn, workspace_id)
    }

    /// Record counts by tag and reporting model.
    pub fn counts_by_tag_and_model(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<(String, String, u64)>, OrbitError> {
        stats::counts_by_tag_and_model(self.conn, workspace_id)
    }
}
