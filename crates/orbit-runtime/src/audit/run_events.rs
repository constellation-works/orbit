//! The one run-events query behind `orbit run events` and the dashboard's
//! `GET /api/runs/:id/events`.
//!
//! Each surface first reads the run through its own existing read policy (the
//! CLI's reconciling or observed `RunRead`, the dashboard's reconciling
//! `show_job_run`) and hands the [`JobRun`] it read to the query, which
//! never writes. The query owns source selection, payload parsing, selector
//! filtering, paging, the tail snapshot and the kind-scan budget; surfaces
//! parse their inputs into its typed selectors and render its result in
//! their own envelope.
//!
//! Two projections exist, one per entry point:
//!
//! - **Enriched** ([`RuntimeKernel::run_events_trail`], `orbit run events`):
//!   the complete chronological trail as [`RunAuditEvent`]s. Payloads that are malformed or carry no
//!   `event_id` are dropped, and [`RunAuditEvent::json_with_step_id`] adds
//!   the inherited `step_id`.
//! - **Stored** ([`RuntimeKernel::run_events_page`], the dashboard API): each
//!   stored envelope exactly as written, a forward or tail page at a time. Only rows whose payload is not JSON
//!   are dropped; a payload without an `event_id` is still returned.

use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use crate::RuntimeKernel;
use orbit_store::V2AuditEventFilter;

use super::run::RunAuditEvent;

/// Hard cap on rows a kind-filtered forward page scans from one run.
pub const RUN_EVENTS_MAX_SCAN_ROWS: usize = 50_000;
/// Capacity hint for a returned page. A fixed constant rather than the
/// caller-supplied `limit` so the allocation size never derives from request
/// input, however large a `limit` a caller asks for.
const RUN_EVENTS_PAGE_CAPACITY_HINT: usize = 64;
const RUN_EVENTS_SOURCE: &str = "v2_envelope";

/// The CLI trail's exact selectors, already resolved by the caller: a step
/// selector given as an id or index arrives here as the audit `step_id`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunEventsSelection<'a> {
    pub step_id: Option<&'a str>,
    pub event_type: Option<&'a str>,
}

/// A stored-projection page of a run's events. A tail page has no kind
/// filter, so that combination has no variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEventsPage<'a> {
    /// Read forward from the run's first event. Without `kind` it is a plain
    /// window; with an exact `body_kind` the match is made inside the payload
    /// under [`RUN_EVENTS_MAX_SCAN_ROWS`].
    Forward {
        kind: Option<&'a str>,
        offset: usize,
        limit: usize,
    },
    /// A window counted back from the newest event, in chronological order,
    /// with the total from the same read snapshot.
    Tail { offset: usize, limit: usize },
}

/// The outcome of [`RuntimeKernel::run_events_page`].
#[derive(Clone, Debug, PartialEq)]
pub enum RunEventsPageResult {
    Forward(Vec<Value>),
    Tail {
        events: Vec<Value>,
        total: usize,
        offset: usize,
    },
    /// A kind-filtered forward page could not be filled within
    /// [`RUN_EVENTS_MAX_SCAN_ROWS`] scanned rows while more rows remained.
    /// A page the budget did fill is returned as [`Self::Forward`].
    ScanBudgetExceeded,
}

impl RuntimeKernel {
    /// `run`'s complete trail, oldest first, in the enriched projection,
    /// narrowed to `selection`'s exact step and event type. `run` is the
    /// record the caller already read under its own read policy; this read
    /// never writes.
    pub fn run_events_trail(
        &self,
        run: &JobRun,
        selection: RunEventsSelection<'_>,
    ) -> Result<Vec<RunAuditEvent>, OrbitError> {
        Ok(self
            .collect_run_audit_events(&run.run_id)?
            .into_iter()
            .filter(|event| {
                selection
                    .step_id
                    .is_none_or(|step| event.step_id.as_deref() == Some(step))
                    && selection
                        .event_type
                        .is_none_or(|kind| event.event_type.as_deref() == Some(kind))
            })
            .collect())
    }

    /// One stored-projection page of `run`'s events. `run` is the record the
    /// caller already read under its own read policy; this read never writes.
    pub fn run_events_page(
        &self,
        run: &JobRun,
        page: RunEventsPage<'_>,
    ) -> Result<RunEventsPageResult, OrbitError> {
        match page {
            RunEventsPage::Forward {
                kind,
                offset,
                limit,
            } => self.forward_run_events(&run.run_id, kind, offset, limit),
            RunEventsPage::Tail { offset, limit } => {
                let filter = V2AuditEventFilter {
                    workspace_id: self.workspace_id()?,
                    run_id: Some(run.run_id.clone()),
                    source: Some(RUN_EVENTS_SOURCE.to_string()),
                    limit: Some(limit),
                    offset: Some(offset),
                    ..Default::default()
                };
                // One read snapshot: a row committed after the count cannot
                // move this page relative to the total that describes it.
                let page = self.v2_audit_store()?.list_v2_audit_event_tail(&filter)?;
                Ok(RunEventsPageResult::Tail {
                    events: page
                        .events
                        .into_iter()
                        .filter_map(|row| serde_json::from_str(&row.payload_json).ok())
                        .collect(),
                    total: usize::try_from(page.total).unwrap_or(0),
                    offset,
                })
            }
        }
    }

    fn forward_run_events(
        &self,
        run_id: &str,
        kind: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<RunEventsPageResult, OrbitError> {
        // Without a kind filter the page is a plain SQL window; `body_kind`
        // lives inside the payload, so a filtered page scans under a row
        // budget instead.
        let (sql_offset, sql_limit, skip) = match kind {
            None => (offset, limit, 0),
            Some(_) => (0, RUN_EVENTS_MAX_SCAN_ROWS + 1, offset),
        };
        let rows = self.list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run_id.to_string()),
            source: Some(RUN_EVENTS_SOURCE.to_string()),
            limit: Some(sql_limit),
            offset: Some(sql_offset),
            oldest_first: true,
            ..Default::default()
        })?;
        let budget_exceeded = kind.is_some() && rows.len() > RUN_EVENTS_MAX_SCAN_ROWS;
        let mut page: Vec<Value> = Vec::with_capacity(RUN_EVENTS_PAGE_CAPACITY_HINT);
        let mut matched: usize = 0;

        for row in rows.into_iter().take(RUN_EVENTS_MAX_SCAN_ROWS) {
            let Ok(value) = serde_json::from_str::<Value>(&row.payload_json) else {
                continue;
            };
            if let Some(needle) = kind {
                let body_kind = value.get("body_kind").and_then(Value::as_str).unwrap_or("");
                if body_kind != needle {
                    continue;
                }
            }
            if matched < skip {
                matched = matched.saturating_add(1);
                continue;
            }
            page.push(value);
            matched = matched.saturating_add(1);
            if page.len() >= limit {
                break;
            }
        }

        if budget_exceeded && page.len() < limit {
            return Ok(RunEventsPageResult::ScanBudgetExceeded);
        }
        Ok(RunEventsPageResult::Forward(page))
    }
}
