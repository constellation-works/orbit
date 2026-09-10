//! One-time per-workspace import of the legacy Markdown friction tree, plus
//! the export route that keeps the corpus inspectable after cutover
//! (ORB-10680).
//!
//! The import is transactional and idempotent. Everything it does — the
//! discovery walk, every insert, and the completion marker — lives in one
//! transaction, so an interruption leaves the workspace exactly as it was and
//! the next open retries from scratch. A malformed record, a friction ID
//! claimed twice inside one source tree, an ID that does not match the file
//! holding it, or a discovered/handled count mismatch aborts that transaction
//! rather than importing a partial corpus.
//!
//! Legacy files are never modified or deleted: after the marker commits they
//! are read-only rollback evidence, and [`export_workspace_frictions`] can
//! re-materialize the live corpus beside them for inspection.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::{Connection, TransactionBehavior};

use crate::Store;
use crate::driver::file::friction_store::{
    friction_record_paths, read_record_at, validated_friction_root, write_record_at,
};

use crate::contracts::{FrictionListFilter, StoredFrictionRecord};
use crate::repository::friction::queries::upsert_record;

/// Shape of the import this binary knows how to produce and re-verify. A
/// marker recorded by a newer binary is refused rather than reinterpreted.
pub(super) const FRICTION_IMPORT_SCHEMA_VERSION: u32 = 1;

/// Rows exported per page, so export memory tracks the page rather than the
/// corpus it is walking.
const EXPORT_PAGE: usize = 200;

/// Outcome of importing one legacy source tree into one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrictionImportReport {
    pub workspace_id: String,
    /// Canonical key for the imported source tree.
    pub source_key: String,
    /// Records discovered in the source tree.
    pub discovered: u64,
    /// Records this import inserted.
    pub imported: u64,
    /// Records skipped because SQLite already held that friction ID. SQLite is
    /// the live source after cutover, so it wins over a stale file copy.
    pub skipped_existing: u64,
    /// `true` when a previous run had already committed this source tree.
    pub already_complete: bool,
}

/// Import `source_root` into `workspace_id` unless a completion marker for
/// that pair already exists.
pub fn import_workspace_frictions(
    store: &Store,
    workspace_id: &str,
    source_root: &Path,
) -> Result<FrictionImportReport, OrbitError> {
    let source_key = source_key(source_root);
    if let Some(report) =
        store.with_read_connection(|conn| completed_marker(conn, workspace_id, &source_key))?
    {
        return Ok(report);
    }

    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();
        // Re-check inside the writer transaction in case another process
        // completed the import after the observational fast path.
        if let Some(report) = completed_marker(conn, workspace_id, &source_key)? {
            return Ok(report);
        }
        run_import(conn, workspace_id, source_root, &source_key)
    })
}

fn completed_marker(
    conn: &Connection,
    workspace_id: &str,
    source_key: &str,
) -> Result<Option<FrictionImportReport>, OrbitError> {
    let row = conn
        .query_row(
            "SELECT schema_version, record_count, imported_count FROM friction_import_state
             WHERE workspace_id = ?1 AND source_key = ?2",
            rusqlite::params![workspace_id, source_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(OrbitError::Store(other.to_string())),
        })?;

    let Some((schema_version, record_count, imported_count)) = row else {
        return Ok(None);
    };
    if schema_version > i64::from(FRICTION_IMPORT_SCHEMA_VERSION) {
        return Err(OrbitError::Migration(format!(
            "friction import for workspace '{workspace_id}' was written by a newer Orbit \
             (import schema v{schema_version}, this binary supports \
             v{FRICTION_IMPORT_SCHEMA_VERSION}); upgrade before reading friction records"
        )));
    }
    Ok(Some(FrictionImportReport {
        workspace_id: workspace_id.to_string(),
        source_key: source_key.to_string(),
        discovered: record_count.max(0) as u64,
        imported: imported_count.max(0) as u64,
        skipped_existing: (record_count - imported_count).max(0) as u64,
        already_complete: true,
    }))
}

fn run_import(
    conn: &Connection,
    workspace_id: &str,
    source_root: &Path,
    source_key: &str,
) -> Result<FrictionImportReport, OrbitError> {
    // `validated_friction_root` follows symlinks the same way `Path::is_dir`
    // does, so a missing or non-directory source is "nothing to import" here
    // exactly as it is for the caller that reports `report.discovered == 0`
    // — not a hard error. A resolved root is kept so every record location
    // below is checked against the same canonical path the walk used.
    let canonical_root = validated_friction_root(source_root).ok();
    let paths = match &canonical_root {
        Some(root) => friction_record_paths(root)?,
        None => Vec::new(),
    };

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut imported = 0u64;
    let mut skipped_existing = 0u64;

    // Streamed one record at a time: the whole corpus is never resident, only
    // the ID set used to detect a collision inside this source tree. `paths`
    // is only non-empty when `canonical_root` resolved, so the two stay in
    // lockstep without an unreachable-error branch.
    if let Some(corpus_root) = canonical_root.as_deref() {
        for path in &paths {
            let stored = read_record_at(path)?;
            let record = stored.record;
            let (month, seq) = split_friction_id(&record.id).ok_or_else(|| {
                OrbitError::Store(format!(
                    "friction record '{}' declares malformed id '{}'",
                    path.display(),
                    record.id
                ))
            })?;
            verify_record_location(path, corpus_root, &month, seq, &record.id)?;
            if !seen.insert(record.id.clone()) {
                return Err(OrbitError::Store(format!(
                    "friction id '{}' is claimed twice in source tree '{}' (at '{}')",
                    record.id,
                    source_root.display(),
                    path.display()
                )));
            }
            if record_exists(conn, workspace_id, &record.id)? {
                skipped_existing += 1;
                continue;
            }
            upsert_record(
                conn,
                workspace_id,
                &record,
                &month,
                seq,
                Some(&path.to_string_lossy()),
            )?;
            imported += 1;
        }
    }

    let discovered = paths.len() as u64;
    if imported + skipped_existing != discovered {
        return Err(OrbitError::Store(format!(
            "friction import for workspace '{workspace_id}' handled {} of {discovered} \
             discovered records",
            imported + skipped_existing
        )));
    }

    conn.execute(
        "INSERT INTO friction_import_state
             (workspace_id, source_key, record_count, imported_count, schema_version, completed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            workspace_id,
            source_key,
            discovered as i64,
            imported as i64,
            i64::from(FRICTION_IMPORT_SCHEMA_VERSION),
            crate::now_string(),
        ],
    )
    .map_err(|error| OrbitError::Store(format!("record friction import completion: {error}")))?;

    if discovered > 0 {
        orbit_common::tracing::info!(
            target: "orbit.store.friction",
            workspace_id,
            discovered,
            imported,
            skipped_existing,
            "imported legacy friction records into SQLite",
        );
    }

    Ok(FrictionImportReport {
        workspace_id: workspace_id.to_string(),
        source_key: source_key.to_string(),
        discovered,
        imported,
        skipped_existing,
        already_complete: false,
    })
}

fn record_exists(
    conn: &Connection,
    workspace_id: &str,
    friction_id: &str,
) -> Result<bool, OrbitError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM friction_records WHERE workspace_id = ?1 AND friction_id = ?2",
            rusqlite::params![workspace_id, friction_id],
            |row| row.get(0),
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    Ok(count > 0)
}

/// The legacy layout addressed a record by its ID, so a record filed under a
/// path its ID does not resolve to was unreachable through `friction show`.
/// Importing it silently would change which record an ID refers to.
///
/// `corpus_root` must be the same canonical root `friction_record_paths`
/// walked to produce `path`; comparing against the caller's raw, possibly
/// symlinked `source_root` would report every record as misfiled.
fn verify_record_location(
    path: &Path,
    corpus_root: &Path,
    month: &str,
    seq: u32,
    id: &str,
) -> Result<(), OrbitError> {
    let expected = corpus_root.join(month).join(format!("F{seq:03}.md"));
    if path != expected {
        return Err(OrbitError::Store(format!(
            "friction record '{}' declares id '{id}', which addresses '{}'",
            path.display(),
            expected.display()
        )));
    }
    Ok(())
}

/// Splits `F<YYYY-MM>-<NNN>` into its month and counter parts.
fn split_friction_id(id: &str) -> Option<(String, u32)> {
    orbit_types::identity::validate_friction_id(id).ok()?;
    let month = id.get(1..8)?.to_string();
    let seq = id.get(9..12)?.parse::<u32>().ok()?;
    (seq > 0).then_some((month, seq))
}

fn source_key(source_root: &Path) -> String {
    std::fs::canonicalize(source_root)
        .unwrap_or_else(|_| source_root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Re-materialize a workspace's live SQLite corpus as the Markdown layout the
/// file store used, for inspection or rollback. Reads one bounded page at a
/// time, so export memory does not grow with the corpus.
pub fn export_workspace_frictions(
    store: &Store,
    workspace_id: &str,
    destination_root: &Path,
) -> Result<u64, OrbitError> {
    let mut exported = 0u64;
    let mut offset = 0usize;
    loop {
        let page = crate::repository::friction::read_page(
            store,
            workspace_id,
            &FrictionListFilter {
                limit: Some(EXPORT_PAGE),
                offset,
                ..FrictionListFilter::default()
            },
        )?;
        if page.is_empty() {
            break;
        }
        for stored in &page {
            write_export_record(destination_root, stored)?;
            exported += 1;
        }
        offset += page.len();
    }
    Ok(exported)
}

fn write_export_record(
    destination_root: &Path,
    stored: &StoredFrictionRecord,
) -> Result<(), OrbitError> {
    let (month, seq) = split_friction_id(&stored.record.id).ok_or_else(|| {
        OrbitError::Store(format!(
            "cannot export friction record with malformed id '{}'",
            stored.record.id
        ))
    })?;
    let path = destination_root.join(month).join(format!("F{seq:03}.md"));
    write_record_at(&path, &stored.record)?;
    Ok(())
}
