//! The bounded run-detail projection: live progress and one page of CLI
//! invocation logs read from a fixed metadata budget, however long the run's
//! audit trail has grown.
//!
//! [`RuntimeKernel::collect_run_execution_progress`] is served from this
//! reader; the full CLI-invocation collectors still replay the complete
//! trail, so their cost grows with every provider event a run has written.
//! This reader selects metadata by kind store-side instead: a prefix of
//! step lifecycle rows, the newest provider spawn and completion rows, the
//! newest provider samples since those, the ancestors that place them in a
//! step, and exactly the requested log page. Counts and presence come from
//! the store. When a class of rows exceeds its budget the projection says
//! so in [`BoundedRunDetail::complete`] rather than presenting a partial
//! view as the whole history.

use std::collections::{HashMap, HashSet};

use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::storage::blob_store::BlobStore;
use orbit_store::V2AuditEventRow;
use orbit_store::contracts::V2AuditStoreBackend;
use serde_json::Value;

use crate::RuntimeKernel;
use orbit_store::V2AuditEventFilter;

use super::projection::{
    audit_steps_from_events, bound_provider_processes, cli_invocation_record,
    provider_processes_from_events, run_audit_event, step_index_by_id,
};
use super::trail::{
    MAX_PROVIDER_PROCESSES, RunAuditEvent, RunAuditStep, RunCliInvocationRecord,
    RunExecutionProgress, STEP_LIFECYCLE_KINDS,
};

const SOURCE: &str = "v2_envelope";
/// Rows that decide which provider child a later row belongs to.
const PROVIDER_STRUCTURAL_KINDS: [&str; 3] = [
    "cli_invocation_process",
    "cli_invocation_finished",
    "cli_invocation_stopped_descendant",
];
/// Periodic rows of a running child, only the newest of which matter.
const PROVIDER_SAMPLE_KINDS: [&str; 2] = ["cli_invocation_activity", "cli_invocation_build_budget"];
const LOG_KIND: &str = "cli_invocation_finished";

/// Step lifecycle rows read oldest first. A run writes a few per step.
const STEP_LIFECYCLE_ROW_BUDGET: usize = 512;
/// Newest provider spawn, completion and stopped-descendant rows. Read apart
/// from samples so a long-running child's samples cannot push its own spawn
/// out of the window.
const PROVIDER_STRUCTURAL_ROW_BUDGET: usize = 256;
/// Newest provider activity and build-budget samples since the oldest
/// structural row read. Samples arrive at most every supervision interval.
const PROVIDER_SAMPLE_ROW_BUDGET: usize = 256;
/// Parent rows fetched by id to place the rows above in their steps.
const ANCESTOR_ROW_BUDGET: usize = 256;
/// Parent links followed from any one row before its step is given up.
const MAX_ANCESTRY_DEPTH: usize = 16;
/// Event ids per lookup, under SQLite's bound-parameter cap.
const ANCESTOR_LOOKUP_CHUNK: usize = 500;
/// Most audit rows a bounded progress read can load: every window budget,
/// one probe row per windowed class, and the ancestor budget.
pub(crate) const BOUNDED_PROGRESS_ROW_BUDGET: usize = STEP_LIFECYCLE_ROW_BUDGET
    + PROVIDER_STRUCTURAL_ROW_BUDGET
    + PROVIDER_SAMPLE_ROW_BUDGET
    + 3
    + ANCESTOR_ROW_BUDGET;

/// The progress half of a bounded run-detail read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BoundedRunProgress {
    pub progress: RunExecutionProgress,
    /// Steps reconstructed from the bounded lifecycle prefix.
    pub steps: Vec<RunAuditStep>,
    /// Whether the lifecycle prefix includes every lifecycle row.
    pub steps_complete: bool,
    /// False when some class of progress metadata exceeded its budget.
    pub complete: bool,
    /// Audit rows this read loaded, never more than
    /// [`BOUNDED_PROGRESS_ROW_BUDGET`].
    pub rows_read: usize,
}

/// Kind-filtered windows that decide bounded progress.
struct ProgressWindows {
    lifecycle: Vec<ParsedRow>,
    provider: Vec<ParsedRow>,
    steps_complete: bool,
    structural_complete: bool,
    samples_complete: bool,
}

/// Progress projected from [`ProgressWindows`], plus the lookup tables a
/// caller needs to place extra rows (such as a log page) in their steps.
struct ProgressProjection {
    progress: RunExecutionProgress,
    steps: Vec<RunAuditStep>,
    step_indices: HashMap<String, u32>,
    known: HashMap<String, Value>,
    ancestry_complete: bool,
}

/// What a bounded run-detail read observed.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundedRunDetail {
    /// `observed` when the run has v2 audit evidence, `unavailable` when not.
    pub state: &'static str,
    pub progress: RunExecutionProgress,
    /// Steps reconstructed from the bounded lifecycle prefix already read for
    /// progress. Consumers can render runs whose record has no step history.
    pub steps: Vec<RunAuditStep>,
    /// Whether the lifecycle prefix includes every lifecycle row for the run.
    pub steps_complete: bool,
    /// Every finished CLI invocation the run recorded, counted by the store.
    pub logs_total: usize,
    /// The requested page of those invocations, oldest first.
    pub logs: Vec<RunCliInvocationRecord>,
    /// False when some class of metadata exceeded its budget, so older
    /// provider children, their samples, step positions, or (past the step
    /// lifecycle budget) the active step may be missing from `progress` and
    /// `logs` step indices. Log counts and pages are exact either way.
    pub complete: bool,
    /// Audit rows this read loaded, never more than `row_budget`.
    pub rows_read: usize,
    /// The most rows this read could load: every class budget, one probe row
    /// per windowed class, and the log page.
    pub row_budget: usize,
}

impl RuntimeKernel {
    /// Bounded progress for a run: the progress half of
    /// [`Self::collect_bounded_run_detail`], with the liveness probe injected
    /// so pairing and projection can be asserted without real live PIDs.
    ///
    /// Loads at most [`BOUNDED_PROGRESS_ROW_BUDGET`] audit rows
    /// (512 lifecycle + 256 structural + 256 samples + 256 ancestors + 3
    /// probe rows), however long the trail has grown.
    pub(crate) fn collect_bounded_run_progress_with<P>(
        &self,
        run_id: &str,
        probe: P,
    ) -> Result<BoundedRunProgress, OrbitError>
    where
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        let workspace_id = self.workspace_id()?;
        let store = self.v2_audit_store()?;
        let present = store
            .list_v2_audit_run_ids_with_events(&workspace_id, &[run_id.to_string()], Some(SOURCE))?
            .contains(run_id);
        if !present {
            return Ok(BoundedRunProgress {
                progress: RunExecutionProgress::unavailable(),
                steps: Vec::new(),
                steps_complete: true,
                complete: true,
                rows_read: 0,
            });
        }
        let mut reader = TrailReader {
            store: store.as_ref(),
            workspace_id,
            run_id,
            rows_read: 0,
        };
        let windows = Self::read_progress_windows(&mut reader)?;
        let projection = Self::project_bounded_progress(&mut reader, &windows, &[], run_id, probe)?;
        Ok(BoundedRunProgress {
            complete: windows.steps_complete
                && windows.structural_complete
                && windows.samples_complete
                && projection.ancestry_complete,
            steps_complete: windows.steps_complete,
            progress: projection.progress,
            steps: projection.steps,
            rows_read: reader.rows_read,
        })
    }

    /// Read the lifecycle, structural and sample windows for bounded
    /// progress. Each window reads one probe row past its budget to tell
    /// whether it is complete.
    fn read_progress_windows(reader: &mut TrailReader) -> Result<ProgressWindows, OrbitError> {
        let (lifecycle, steps_complete) =
            reader.window(&STEP_LIFECYCLE_KINDS, true, None, STEP_LIFECYCLE_ROW_BUDGET)?;
        let (mut structural, structural_complete) = reader.window(
            &PROVIDER_STRUCTURAL_KINDS,
            false,
            None,
            PROVIDER_STRUCTURAL_ROW_BUDGET,
        )?;
        structural.reverse();
        // A sample older than every structural row read can only belong to a
        // child spawned outside the window.
        let (samples, samples_complete) = match structural.first() {
            Some(anchor) => reader.window(
                &PROVIDER_SAMPLE_KINDS,
                false,
                Some(anchor.ts),
                PROVIDER_SAMPLE_ROW_BUDGET,
            )?,
            None => (Vec::new(), true),
        };
        let lifecycle = parse_rows(lifecycle);
        let mut provider = parse_rows(structural);
        provider.extend(parse_rows(samples));
        // Trail order, as the full replay sees it.
        provider.sort_by_key(|row| (row.ts, row.id));
        Ok(ProgressWindows {
            lifecycle,
            provider,
            steps_complete,
            structural_complete,
            samples_complete,
        })
    }

    /// Resolve ancestors for the windowed provider rows (and any `extra`
    /// rows the caller needs placed, such as a log page) and project them
    /// into progress. `truncated` is set when children were dropped by the
    /// provider bound or when the structural window overflowed; the active
    /// step is reported only when the lifecycle prefix is complete.
    fn project_bounded_progress<P>(
        reader: &mut TrailReader,
        windows: &ProgressWindows,
        extra: &[ParsedRow],
        run_id: &str,
        probe: P,
    ) -> Result<ProgressProjection, OrbitError>
    where
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        let mut known = HashMap::new();
        for row in windows
            .lifecycle
            .iter()
            .chain(&windows.provider)
            .chain(extra)
        {
            known.insert(row.event_id.clone(), row.raw.clone());
        }
        let ancestry_complete =
            reader.resolve_ancestors(windows.provider.iter().chain(extra), &mut known)?;
        let events = |rows: &[ParsedRow]| -> Vec<RunAuditEvent> {
            rows.iter()
                .map(|row| run_audit_event(row.event_id.clone(), row.raw.clone(), &known))
                .collect()
        };
        let steps = audit_steps_from_events(&events(&windows.lifecycle));
        let step_indices = step_index_by_id(&steps);
        // Indices from the prefix are exact, but a step whose finish lies
        // past it would look open, so a truncated prefix names no active
        // step.
        let active_step = steps
            .iter()
            .rev()
            .find(|step| windows.steps_complete && step.finished_at.is_none())
            .cloned();
        let records =
            provider_processes_from_events(run_id, events(&windows.provider), &step_indices, probe);
        let (provider_processes, dropped) =
            bound_provider_processes(records, MAX_PROVIDER_PROCESSES);
        let progress = RunExecutionProgress {
            state: "observed",
            active_step,
            provider_processes,
            limit: MAX_PROVIDER_PROCESSES,
            // Children spawned before the window may exist unseen.
            truncated: dropped || !windows.structural_complete,
        };
        Ok(ProgressProjection {
            progress,
            steps,
            step_indices,
            known,
            ancestry_complete,
        })
    }

    /// Progress and one page of CLI invocation logs for a run, reading at most
    /// `BoundedRunDetail::row_budget` audit rows.
    ///
    /// `log_offset` and `log_limit` select finished invocations in trail
    /// order; blob previews read at most `blob_preview_max_bytes` of each
    /// stream. The full-history readers, which `orbit run show` and follow
    /// mode depend on, are unchanged.
    pub fn collect_bounded_run_detail(
        &self,
        run_id: &str,
        log_offset: usize,
        log_limit: usize,
        blob_preview_max_bytes: usize,
    ) -> Result<BoundedRunDetail, OrbitError> {
        let row_budget = BOUNDED_PROGRESS_ROW_BUDGET + log_limit;
        let workspace_id = self.workspace_id()?;
        let store = self.v2_audit_store()?;
        let present = store
            .list_v2_audit_run_ids_with_events(&workspace_id, &[run_id.to_string()], Some(SOURCE))?
            .contains(run_id);
        if !present {
            return Ok(BoundedRunDetail {
                state: "unavailable",
                progress: RunExecutionProgress::unavailable(),
                steps: Vec::new(),
                steps_complete: true,
                logs_total: 0,
                logs: Vec::new(),
                complete: true,
                rows_read: 0,
                row_budget,
            });
        }

        let mut reader = TrailReader {
            store: store.as_ref(),
            workspace_id,
            run_id,
            rows_read: 0,
        };
        let windows = Self::read_progress_windows(&mut reader)?;
        let logs_total = usize::try_from(reader.store.count_v2_audit_events(&reader.filter(
            &[LOG_KIND],
            true,
            None,
            None,
        ))?)
        .unwrap_or(usize::MAX);
        let log_rows = if log_limit == 0 {
            Vec::new()
        } else {
            reader.list(V2AuditEventFilter {
                offset: Some(log_offset),
                ..reader.filter(&[LOG_KIND], true, None, Some(log_limit))
            })?
        };
        let log_rows = parse_rows(log_rows);
        let projection = Self::project_bounded_progress(
            &mut reader,
            &windows,
            &log_rows,
            run_id,
            probe_process_liveness,
        )?;

        let blob_store = BlobStore::new(self.v2_audit_blob_root());
        let logs = log_rows
            .into_iter()
            .map(|row| run_audit_event(row.event_id, row.raw, &projection.known))
            .map(|event| {
                cli_invocation_record(
                    run_id,
                    event,
                    &blob_store,
                    Some(blob_preview_max_bytes),
                    &projection.step_indices,
                )
            })
            .collect();

        Ok(BoundedRunDetail {
            state: "observed",
            steps: projection.steps,
            steps_complete: windows.steps_complete,
            progress: projection.progress,
            logs_total,
            logs,
            complete: windows.steps_complete
                && windows.structural_complete
                && windows.samples_complete
                && projection.ancestry_complete,
            rows_read: reader.rows_read,
            row_budget,
        })
    }
}

/// Reads one run's v2 envelope rows by kind and id, counting every row loaded.
struct TrailReader<'a> {
    store: &'a dyn V2AuditStoreBackend,
    workspace_id: String,
    run_id: &'a str,
    rows_read: usize,
}

impl TrailReader<'_> {
    fn filter(
        &self,
        kinds: &[&str],
        oldest_first: bool,
        since: Option<chrono::DateTime<chrono::Utc>>,
        limit: Option<usize>,
    ) -> V2AuditEventFilter {
        V2AuditEventFilter {
            workspace_id: self.workspace_id.clone(),
            run_id: Some(self.run_id.to_string()),
            source: Some(SOURCE.to_string()),
            body_kinds: kinds.iter().map(|kind| kind.to_string()).collect(),
            oldest_first,
            since,
            limit,
            ..Default::default()
        }
    }

    fn list(&mut self, filter: V2AuditEventFilter) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        let rows = self.store.list_v2_audit_events(&filter)?;
        self.rows_read += rows.len();
        Ok(rows)
    }

    /// At most `budget` rows of `kinds` from one end of the trail, and whether
    /// those are all of them. One extra row is read to tell.
    fn window(
        &mut self,
        kinds: &[&str],
        oldest_first: bool,
        since: Option<chrono::DateTime<chrono::Utc>>,
        budget: usize,
    ) -> Result<(Vec<V2AuditEventRow>, bool), OrbitError> {
        let mut rows = self.list(self.filter(kinds, oldest_first, since, Some(budget + 1)))?;
        let complete = rows.len() <= budget;
        rows.truncate(budget);
        Ok((rows, complete))
    }

    /// Fetch the parents `rows` need to find their enclosing step, level by
    /// level, into `known`. Returns false when the ancestor budget or depth
    /// ran out with links still unresolved.
    fn resolve_ancestors<'r>(
        &mut self,
        rows: impl Iterator<Item = &'r ParsedRow>,
        known: &mut HashMap<String, Value>,
    ) -> Result<bool, OrbitError> {
        let mut requested = HashSet::new();
        let mut frontier = Vec::new();
        for row in rows {
            if row.raw.get("step_id").and_then(Value::as_str).is_none() {
                push_parent(&row.raw, known, &mut requested, &mut frontier);
            }
        }
        let mut fetched = 0;
        for _ in 0..MAX_ANCESTRY_DEPTH {
            if frontier.is_empty() {
                return Ok(true);
            }
            let remaining = ANCESTOR_ROW_BUDGET - fetched;
            if remaining == 0 {
                return Ok(false);
            }
            let truncated = frontier.len() > remaining;
            frontier.truncate(remaining);
            let mut next = Vec::new();
            for chunk in frontier.chunks(ANCESTOR_LOOKUP_CHUNK) {
                let rows = self.list(V2AuditEventFilter {
                    event_ids: chunk.to_vec(),
                    ..self.filter(&[], false, None, Some(chunk.len()))
                })?;
                fetched += rows.len();
                for row in parse_rows(rows) {
                    // The walk stops at a step start; other parents continue it.
                    if row.raw.get("body_kind").and_then(Value::as_str) != Some("step_started") {
                        push_parent(&row.raw, known, &mut requested, &mut next);
                    }
                    known.insert(row.event_id, row.raw);
                }
            }
            if truncated {
                return Ok(false);
            }
            frontier = next;
        }
        Ok(frontier.is_empty())
    }
}

fn push_parent(
    raw: &Value,
    known: &HashMap<String, Value>,
    requested: &mut HashSet<String>,
    frontier: &mut Vec<String>,
) {
    if let Some(parent) = raw.get("parent_event_id").and_then(Value::as_str)
        && !known.contains_key(parent)
        && requested.insert(parent.to_string())
    {
        frontier.push(parent.to_string());
    }
}

/// An envelope row whose payload parsed and names its `event_id`.
struct ParsedRow {
    id: i64,
    ts: chrono::DateTime<chrono::Utc>,
    event_id: String,
    raw: Value,
}

fn parse_rows(rows: Vec<V2AuditEventRow>) -> Vec<ParsedRow> {
    rows.into_iter()
        .filter_map(|row| {
            let raw: Value = serde_json::from_str(&row.payload_json).ok()?;
            let event_id = raw.get("event_id").and_then(Value::as_str)?.to_string();
            Some(ParsedRow {
                id: row.id,
                ts: row.ts,
                event_id,
                raw,
            })
        })
        .collect()
}
