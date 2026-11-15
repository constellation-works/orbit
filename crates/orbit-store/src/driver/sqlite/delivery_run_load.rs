//! Loading the retained sampled runs a profile rebuild reads
//! (`ResourceUsageStoreBackend::delivery_run_resources` and
//! `reload_delivery_runs`).
//!
//! A host keeps hundreds of thousands of minute rows, and a rebuild runs on
//! the request that first sees a finished delivery. Reading every row again,
//! each joined to `job_runs` and decoded whole, cost most of that request
//! (ORB-21826). So a reload first reads each sampled run's shape (its row
//! count and last minute index, from the primary key, and its `job_runs`
//! row) and re-reads the minutes only of runs whose shape moved; a full read
//! takes `job_runs` from the same shapes instead of joining every row. Either
//! way the result is the runs, fields and minutes a whole read returns, in
//! its order.

use chrono::{DateTime, NaiveDate, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Rows, params};

use crate::contracts::{
    DeliveryRunResources, DeliveryRunShape, DeliveryRunsLoad, ResourceMinuteLoad,
};

use super::codec::{stamp, unsigned};

/// The columns [`push_rows`] reads, in its order.
const MINUTE_COLUMNS: &str = "workspace_id, run_id, task_id, work_class, complexity, resumed,
    delivery_admitted_at, minute_started_at, cpu_cores, memory_bytes,
    cpu_accounting, samples, crew_model, orbit_version";

/// Every run with a minute admitted at or after `since`, with its minutes
/// oldest first, ordered by workspace then run id.
pub(super) fn load_delivery_runs(
    conn: &Connection,
    since: DateTime<Utc>,
) -> Result<Vec<DeliveryRunResources>, OrbitError> {
    in_read_snapshot(conn, |conn| {
        let shapes = run_shapes(conn)?;
        read_all(conn, since, &shapes)
    })
}

/// [`load_delivery_runs`] since the epoch, reusing each run of `previous`
/// whose shape is unchanged.
pub(super) fn reload_delivery_runs(
    conn: &Connection,
    previous: Option<DeliveryRunsLoad>,
) -> Result<DeliveryRunsLoad, OrbitError> {
    in_read_snapshot(conn, |conn| {
        let shapes = run_shapes(conn)?;
        let since = DateTime::UNIX_EPOCH;
        let held = held_runs(&shapes, previous.unwrap_or_default());
        let moved = held.iter().filter(|run| run.is_none()).count();
        // Past half the runs, one ordered scan beats a lookup per run.
        let runs = if moved * 2 > shapes.len() {
            read_all(conn, since, &shapes)?
        } else {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT {MINUTE_COLUMNS}
                     FROM run_resource_minutes
                     WHERE workspace_id = ?1 AND run_id = ?2 AND delivery_admitted_at >= ?3
                     ORDER BY minute_index",
                ))
                .map_err(sqlite_store_error)?;
            let since = stamp(since);
            let mut runs = Vec::with_capacity(shapes.len());
            for (shape, held) in shapes.iter().zip(held) {
                match held {
                    Some(run) => runs.extend(run),
                    None => {
                        let mut rows = statement
                            .query(params![shape.workspace_id, shape.run_id, since])
                            .map_err(sqlite_store_error)?;
                        push_rows(&mut rows, std::slice::from_ref(shape), &mut runs)?;
                    }
                }
            }
            runs
        };
        Ok(DeliveryRunsLoad { runs, shapes })
    })
}

/// What the earlier load holds for one current run: `Some` while the run's
/// shape is unchanged (with `None` inside when that load read no minute of
/// it), `None` when its minutes must be read again.
type HeldRun = Option<Option<DeliveryRunResources>>;

/// [`HeldRun`] for each of `shapes`, in order.
fn held_runs(shapes: &[DeliveryRunShape], previous: DeliveryRunsLoad) -> Vec<HeldRun> {
    let mut old_shapes = previous.shapes.iter().peekable();
    let mut old_runs = previous.runs.into_iter().peekable();
    shapes
        .iter()
        .map(|shape| {
            let current = (shape.workspace_id.as_str(), shape.run_id.as_str());
            while old_shapes
                .peek()
                .is_some_and(|old| (old.workspace_id.as_str(), old.run_id.as_str()) < current)
            {
                old_shapes.next();
            }
            if !old_shapes.peek().is_some_and(|old| *old == shape) {
                return None;
            }
            while old_runs
                .peek()
                .is_some_and(|old| (old.workspace_id.as_str(), old.run_id.as_str()) < current)
            {
                old_runs.next();
            }
            Some(
                old_runs.next_if(|old| (old.workspace_id.as_str(), old.run_id.as_str()) == current),
            )
        })
        .collect()
}

/// Every run's minutes admitted at or after `since`, in one ordered scan.
fn read_all(
    conn: &Connection,
    since: DateTime<Utc>,
    shapes: &[DeliveryRunShape],
) -> Result<Vec<DeliveryRunResources>, OrbitError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {MINUTE_COLUMNS}
             FROM run_resource_minutes
             WHERE delivery_admitted_at >= ?1
             ORDER BY workspace_id, run_id, minute_index",
        ))
        .map_err(sqlite_store_error)?;
    let mut rows = statement
        .query(params![stamp(since)])
        .map_err(sqlite_store_error)?;
    let mut runs = Vec::with_capacity(shapes.len());
    push_rows(&mut rows, shapes, &mut runs)?;
    Ok(runs)
}

/// Group `rows`, ordered by workspace, run id and minute index, into runs
/// appended to `runs`. `shapes` holds the runs' `job_runs` rows in the same
/// order. A run takes its task, class and admission from its first minute;
/// it is resumed when any minute is; its crew model and version are the
/// newest any minute recorded, a minute without a crew model reading its
/// `job_runs` row's.
fn push_rows(
    rows: &mut Rows<'_>,
    shapes: &[DeliveryRunShape],
    runs: &mut Vec<DeliveryRunResources>,
) -> Result<(), OrbitError> {
    let first = runs.len();
    // The raw admission stamp of the current run's first minute, so a later
    // minute's equal stamp is not parsed again.
    let mut admitted_raw = String::new();
    let mut job_crew: Option<String> = None;
    let mut next_shape = 0;
    while let Some(row) = rows.next().map_err(sqlite_store_error)? {
        let workspace_id = text(row, 0)?;
        let run_id = text(row, 1)?;
        let continues = runs.len() > first
            && runs
                .last()
                .is_some_and(|last| last.workspace_id == workspace_id && last.run_id == run_id);
        let admitted = text(row, 6)?;
        if !continues || admitted != admitted_raw {
            // Every minute's stamp must parse, as when each row was read whole.
            let parsed = parse_stamp(admitted)?;
            if !continues {
                admitted_raw.clear();
                admitted_raw.push_str(admitted);
                while shapes.get(next_shape).is_some_and(|shape| {
                    (shape.workspace_id.as_str(), shape.run_id.as_str()) < (workspace_id, run_id)
                }) {
                    next_shape += 1;
                }
                let job = shapes.get(next_shape).filter(|shape| {
                    shape.job_id.is_some()
                        && shape.workspace_id == workspace_id
                        && shape.run_id == run_id
                });
                job_crew = job.and_then(|job| job.crew_model.clone());
                runs.push(DeliveryRunResources {
                    workspace_id: workspace_id.to_string(),
                    run_id: run_id.to_string(),
                    job_id: job.and_then(|job| job.job_id.clone()),
                    task_id: row.get(2).map_err(sqlite_store_error)?,
                    work_class: row.get(3).map_err(sqlite_store_error)?,
                    complexity: row.get(4).map_err(sqlite_store_error)?,
                    resumed: false,
                    delivery_admitted_at: parsed,
                    finished_at: job.and_then(|job| job.finished_at),
                    crew_model: None,
                    orbit_version: None,
                    minutes: Vec::new(),
                });
            }
        }
        let Some(run) = runs.last_mut() else {
            continue;
        };
        run.resumed |= row.get::<_, bool>(5).map_err(sqlite_store_error)?;
        match optional_text(row, 12)? {
            Some(model) => replace_if_changed(&mut run.crew_model, model),
            None => {
                if job_crew.is_some() && run.crew_model != job_crew {
                    run.crew_model.clone_from(&job_crew);
                }
            }
        }
        if let Some(version) = optional_text(row, 13)? {
            replace_if_changed(&mut run.orbit_version, version);
        }
        run.minutes.push(ResourceMinuteLoad {
            minute_started_at: parse_stamp(text(row, 7)?)?,
            cpu_cores: row.get(8).map_err(sqlite_store_error)?,
            memory_bytes: unsigned(row.get(9).map_err(sqlite_store_error)?),
            cpu_accounting: row.get(10).map_err(sqlite_store_error)?,
            samples: row.get(11).map_err(sqlite_store_error)?,
        });
    }
    Ok(())
}

/// Every run with a minute row, with its row count and `job_runs` row, in
/// key order. The counts come from the primary key alone.
fn run_shapes(conn: &Connection) -> Result<Vec<DeliveryRunShape>, OrbitError> {
    let mut statement = conn
        .prepare(
            r#"
            SELECT r.workspace_id, r.run_id, r.minute_rows, r.last_minute_index,
                   j.job_id, j.finished_at, j.crew_model
            FROM (
                SELECT workspace_id, run_id, COUNT(*) AS minute_rows,
                       MAX(minute_index) AS last_minute_index
                FROM run_resource_minutes
                GROUP BY workspace_id, run_id
            ) r
            LEFT JOIN job_runs j ON j.workspace_id = r.workspace_id AND j.run_id = r.run_id
            ORDER BY r.workspace_id, r.run_id
            "#,
        )
        .map_err(sqlite_store_error)?;
    let mut rows = statement.query([]).map_err(sqlite_store_error)?;
    let mut shapes = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_store_error)? {
        shapes.push(DeliveryRunShape {
            workspace_id: row.get(0).map_err(sqlite_store_error)?,
            run_id: row.get(1).map_err(sqlite_store_error)?,
            minute_rows: unsigned(row.get(2).map_err(sqlite_store_error)?),
            last_minute_index: row.get(3).map_err(sqlite_store_error)?,
            job_id: row.get(4).map_err(sqlite_store_error)?,
            finished_at: optional_text(row, 5)?.map(parse_stamp).transpose()?,
            crew_model: row.get(6).map_err(sqlite_store_error)?,
        });
    }
    Ok(shapes)
}

/// Run `read` in one deferred read transaction, so its statements see one
/// snapshot.
fn in_read_snapshot<T>(
    conn: &Connection,
    read: impl FnOnce(&Connection) -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    conn.execute_batch("BEGIN DEFERRED")
        .map_err(sqlite_store_error)?;
    match read(conn) {
        Ok(value) => {
            conn.execute_batch("COMMIT").map_err(sqlite_store_error)?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn replace_if_changed(slot: &mut Option<String>, value: &str) {
    if slot.as_deref() != Some(value) {
        *slot = Some(value.to_string());
    }
}

/// A text column, borrowed from the row.
fn text<'row>(row: &'row rusqlite::Row<'_>, index: usize) -> Result<&'row str, OrbitError> {
    row.get_ref(index)
        .and_then(|value| value.as_str().map_err(Into::into))
        .map_err(sqlite_store_error)
}

/// A nullable text column, borrowed from the row.
fn optional_text<'row>(
    row: &'row rusqlite::Row<'_>,
    index: usize,
) -> Result<Option<&'row str>, OrbitError> {
    match row.get_ref(index).map_err(sqlite_store_error)? {
        ValueRef::Null => Ok(None),
        value => value
            .as_str()
            .map(Some)
            .map_err(|error| sqlite_store_error(error.into())),
    }
}

/// A stored timestamp. The store writes fixed-width UTC stamps
/// (`YYYY-MM-DDTHH:MM:SS.ffffffZ`), read here without the general parser;
/// any other text goes through it, so what it accepts and refuses is
/// unchanged.
fn parse_stamp(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    match fixed_width_stamp(raw) {
        Some(at) => Ok(at),
        None => crate::parse_timestamp(raw).map_err(sqlite_store_error),
    }
}

fn fixed_width_stamp(raw: &str) -> Option<DateTime<Utc>> {
    let bytes = raw.as_bytes();
    if bytes.len() != 27
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[26] != b'Z'
    {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<u32> {
        bytes[from..to].iter().try_fold(0u32, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(byte - b'0'))
        })
    };
    let year = i32::try_from(number(0, 4)?).ok()?;
    let at = NaiveDate::from_ymd_opt(year, number(5, 7)?, number(8, 10)?)?.and_hms_micro_opt(
        number(11, 13)?,
        number(14, 16)?,
        number(17, 19)?,
        number(20, 26)?,
    )?;
    Some(at.and_utc())
}
