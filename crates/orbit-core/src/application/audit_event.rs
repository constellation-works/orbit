use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter, AuditEventInsertParams,
    AuditInvocationFields, AuditRoleAggregate, AuditToolAggregate, AuditToolModelAggregate,
    FailureIncidentQuery, FailureIncidentReport,
};
use orbit_types::identity::{infer_agent_family_from_model, normalize_attribution_label};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus, AuditStats};
use orbit_types::tool::{McpCapability, McpTransport};
use serde_json::Value;

use crate::OrbitRuntime;

/// Page size of `GET /api/audit` when the request names no limit.
const AUDIT_HTTP_DEFAULT_LIMIT: usize = 50;

/// Largest page of `GET /api/audit`, and the batch its bounded scan reads.
const AUDIT_HTTP_MAX_LIMIT: usize = 200;

/// Largest `?offset=` accepted by `GET /api/audit`. SQLite walks and discards
/// every skipped row, so an unbounded offset is an unbounded scan. Rejecting
/// (rather than clamping) keeps a too-deep page from silently returning rows
/// from the wrong position.
const AUDIT_HTTP_MAX_OFFSET: usize = 100_000;

/// Rows a single `/api/audit` request may pull from SQLite while satisfying
/// a Rust-side predicate. Bounds the cost of a needle that matches nothing
/// in a long history; a page that hits the cap simply comes back short.
const AUDIT_HTTP_POST_FILTER_SCAN_CAP: usize = 10_000;

/// The incident rollup itself is bounded to this many source rows, so an
/// exact incident drilldown never needs more IDs than this.
const AUDIT_HTTP_MAX_EXACT_IDS: usize = 10_000;

/// Column filters both audit listing surfaces accept. A surface decides how
/// its own input maps onto them; [`OrbitRuntime::audit_listing_page`] runs them.
#[derive(Default)]
pub struct AuditListingColumns {
    pub since: Option<DateTime<Utc>>,
    pub tool_name: Option<String>,
    pub status: Option<AuditEventStatus>,
    pub role: Option<String>,
    pub workspace_id: Option<String>,
    pub caller_machine_id: Option<String>,
    pub process_machine_id: Option<String>,
    pub transport: Option<McpTransport>,
    pub capability: Option<McpCapability>,
    pub origin_session_id: Option<String>,
    pub mcp_call_id: Option<String>,
    pub job_run_id: Option<String>,
    pub lease_id: Option<String>,
}

impl From<AuditListingColumns> for AuditEventFilter {
    fn from(columns: AuditListingColumns) -> Self {
        Self {
            since: columns.since,
            tool_name: columns.tool_name,
            status: columns.status,
            role: columns.role,
            workspace_id: columns.workspace_id,
            caller_machine_id: columns.caller_machine_id,
            process_machine_id: columns.process_machine_id,
            transport: columns.transport,
            capability: columns.capability,
            origin_session_id: columns.origin_session_id,
            mcp_call_id: columns.mcp_call_id,
            job_run_id: columns.job_run_id,
            lease_id: columns.lease_id,
            ..Self::default()
        }
    }
}

/// What each audit listing surface may ask for beyond the shared columns.
pub enum AuditListingProfile {
    /// `orbit audit list`: one store window from the newest row.
    Cli {
        target_type: Option<String>,
        limit: usize,
    },
    /// `GET /api/audit`: an exact evidence set, or a bounded page with a
    /// keyset cursor and predicates that have no store column.
    Http {
        window: AuditListingHttpWindow,
        before_id: Option<i64>,
        exclude_unverified_success: bool,
        predicates: AuditListingPredicates,
        /// Takes precedence over the predicates and the window.
        exact_ids: Option<AuditListingExactIds>,
    },
}

/// The bounded page of an ordinary HTTP audit listing. A `limit` of zero
/// requests an empty page: it never falls through to the store's internal
/// `filter.limit == 0` default, so an ordinary page never exceeds the
/// 200-row HTTP maximum.
pub struct AuditListingHttpWindow {
    limit: usize,
    offset: usize,
}

impl AuditListingHttpWindow {
    /// Applies the default and clamp to `limit` and refuses a too-deep
    /// `offset`, returning the refusal text for the surface to report.
    /// An omitted `limit` becomes the 50-row default; a positive `limit` is
    /// clamped to the 200-row maximum; an explicit zero is preserved and
    /// means an empty page (no store rows).
    pub fn from_request(limit: Option<usize>, offset: Option<usize>) -> Result<Self, String> {
        let limit = limit
            .unwrap_or(AUDIT_HTTP_DEFAULT_LIMIT)
            .min(AUDIT_HTTP_MAX_LIMIT);
        let offset = offset.unwrap_or(0);
        if offset > AUDIT_HTTP_MAX_OFFSET {
            return Err(format!(
                "offset must be <= {AUDIT_HTTP_MAX_OFFSET}; got {offset}"
            ));
        }
        Ok(Self { limit, offset })
    }
}

/// Deduplicated positive audit row IDs of an incident drilldown.
pub struct AuditListingExactIds(Vec<i64>);

impl AuditListingExactIds {
    /// Parses a comma-separated ID list, returning the refusal text for the
    /// surface to report.
    pub fn parse_comma_separated(raw: &str) -> Result<Self, String> {
        if raw.split(',').count() > AUDIT_HTTP_MAX_EXACT_IDS {
            return Err(format!(
                "ids must contain at most {AUDIT_HTTP_MAX_EXACT_IDS} values"
            ));
        }

        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for raw_id in raw.split(',') {
            let id = raw_id
                .parse::<i64>()
                .map_err(|_| "ids must be comma-separated positive audit row IDs".to_string())?;
            if id <= 0 {
                return Err("ids must be comma-separated positive audit row IDs".to_string());
            }
            if seen.insert(id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return Err("ids must include at least one audit row ID".to_string());
        }
        Ok(Self(ids))
    }
}

/// HTTP listing predicates with no store column, as requested. Blank values
/// request nothing; [`OrbitRuntime::audit_listing_page`] normalizes the rest.
#[derive(Default)]
pub struct AuditListingPredicates {
    pub agent_family: Option<String>,
    pub non_success: bool,
    pub execution_id: Option<String>,
    pub fs_profile: Option<String>,
    pub text: Option<String>,
}

/// One page of an audit listing.
pub struct AuditListingPage {
    pub events: Vec<AuditEvent>,
    /// The `before` value that resumes the listing just past this page. `None`
    /// once the window is exhausted. A short page can still carry one when a
    /// bounded scan stopped at its cap.
    pub next_before: Option<i64>,
}

/// Predicates without a direct store filter, applied to each fetched row in Rust.
struct AuditPostFilter {
    agent_family: Option<String>,
    non_success: bool,
    execution_id: Option<String>,
    fs_profile: Option<String>,
    /// Lowercased free-text needle.
    needle: Option<String>,
}

impl AuditPostFilter {
    fn from_predicates(predicates: AuditListingPredicates) -> Self {
        Self {
            agent_family: predicates
                .agent_family
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(audit_agent_family_key),
            non_success: predicates.non_success,
            execution_id: predicates.execution_id.filter(|s| !s.is_empty()),
            fs_profile: predicates
                .fs_profile
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            needle: predicates
                .text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_lowercase),
        }
    }

    fn is_empty(&self) -> bool {
        self.agent_family.is_none()
            && !self.non_success
            && self.execution_id.is_none()
            && self.fs_profile.is_none()
            && self.needle.is_none()
    }

    fn matches(&self, e: &AuditEvent) -> bool {
        if let Some(family) = self.agent_family.as_deref()
            && audit_agent_family_key(&e.role) != family
        {
            return false;
        }
        if self.non_success && e.status == AuditEventStatus::Success {
            return false;
        }
        if let Some(eid) = self.execution_id.as_deref()
            && e.execution_id != eid
        {
            return false;
        }
        if let Some(profile) = self.fs_profile.as_deref()
            && !arguments_json_matches_profile(e.arguments_json.as_deref(), profile)
        {
            return false;
        }
        if let Some(needle) = self.needle.as_deref() {
            let haystacks = [
                e.command.as_str(),
                e.subcommand.as_deref().unwrap_or(""),
                e.tool_name.as_deref().unwrap_or(""),
                e.target_id.as_deref().unwrap_or(""),
                e.target_type.as_deref().unwrap_or(""),
                e.role.as_str(),
                e.error_message.as_deref().unwrap_or(""),
            ];
            if !haystacks.iter().any(|h| h.to_lowercase().contains(needle)) {
                return false;
            }
        }
        true
    }
}

/// Maps a free-form audit `role` onto a canonical agent family.
fn audit_agent_family_key(role: &str) -> String {
    let normalized = normalize_attribution_label(role, None);
    infer_agent_family_from_model(&normalized).unwrap_or(normalized)
}

/// Best-effort match of a stringified `arguments_json` payload against a
/// requested fsProfile name. Looks for any of the conventional keys
/// (`fsProfile`, `fs_profile`, `profile`) at the top level of the parsed
/// object. Returns `false` for malformed or empty payloads — the SQLite schema
/// has no profile column, so absence cannot be distinguished from mismatch.
fn arguments_json_matches_profile(raw: Option<&str>, expected: &str) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    const KEYS: &[&str] = &["fsProfile", "fs_profile", "profile"];
    let Some(obj) = value.as_object() else {
        return false;
    };
    for key in KEYS {
        if let Some(Value::String(found)) = obj.get(*key)
            && found == expected
        {
            return true;
        }
    }
    false
}

impl OrbitRuntime {
    /// Returns persistent audit events recorded by the CLI middleware across all invocations.
    /// Backed by SQLite; survives process restarts. For in-process session events only, see
    /// [`crate::runtime::RuntimeKernel::list_session_events`].
    pub fn list_audit_events(
        &self,
        since: Option<DateTime<Utc>>,
        tool: Option<String>,
        status: Option<AuditEventStatus>,
        role: Option<String>,
        limit: usize,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        self.list_audit_events_with_kind(since, tool, None, status, role, limit)
    }

    pub fn list_audit_events_with_kind(
        &self,
        since: Option<DateTime<Utc>>,
        tool: Option<String>,
        target_type: Option<String>,
        status: Option<AuditEventStatus>,
        role: Option<String>,
        limit: usize,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        self.stores()
            .audit_events()
            .list_audit_events(&AuditEventFilter {
                since,
                tool_name: tool,
                target_type,
                status,
                role,
                workspace_id: None,
                caller_machine_id: None,
                process_machine_id: None,
                transport: None,
                capability: None,
                origin_session_id: None,
                mcp_call_id: None,
                job_run_id: None,
                lease_id: None,
                before_id: None,
                exclude_unverified_success: false,
                application_entry_execution_id: None,
                limit,
                offset: 0,
            })
    }

    /// Every matching event, newest first, read in pages so an export is
    /// never cut at the store's default page size.
    pub fn export_audit_events(
        &self,
        since: Option<DateTime<Utc>>,
        tool: Option<String>,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        const PAGE: usize = 1000;
        let mut filter = AuditEventFilter {
            since,
            tool_name: tool,
            limit: PAGE,
            ..AuditEventFilter::default()
        };
        let mut events: Vec<AuditEvent> = Vec::new();
        loop {
            let page = self.stores().audit_events().list_audit_events(&filter)?;
            let fetched = page.len();
            // Pages run newest first by id; an event recorded mid-export
            // shifts later pages, so skip rows an earlier page returned.
            let oldest_seen = events.last().map(|event| event.id);
            events.extend(
                page.into_iter()
                    .filter(|event| oldest_seen.is_none_or(|oldest| event.id < oldest)),
            );
            if fetched < PAGE {
                return Ok(events);
            }
            filter.offset += fetched;
        }
    }

    pub fn list_audit_events_filtered(
        &self,
        filter: &AuditEventFilter,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        self.stores().audit_events().list_audit_events(filter)
    }

    /// Returns exact persistent audit rows by their database IDs, newest first.
    pub fn list_audit_events_by_ids(
        &self,
        ids: &[i64],
        workspace_id: Option<&str>,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        self.stores()
            .audit_events()
            .list_audit_events_by_ids(ids, workspace_id)
    }

    /// The audit listing of `orbit audit list` and `GET /api/audit`: selects
    /// the rows for `profile` and says where the listing resumes.
    pub fn audit_listing_page(
        &self,
        columns: AuditListingColumns,
        profile: AuditListingProfile,
    ) -> Result<AuditListingPage, OrbitError> {
        match profile {
            AuditListingProfile::Cli { target_type, limit } => {
                self.audit_listing_store_window(&AuditEventFilter {
                    target_type,
                    limit,
                    ..columns.into()
                })
            }
            AuditListingProfile::Http {
                window,
                before_id,
                exclude_unverified_success,
                predicates,
                exact_ids,
            } => {
                if let Some(AuditListingExactIds(ids)) = exact_ids {
                    return Ok(AuditListingPage {
                        events: self
                            .list_audit_events_by_ids(&ids, columns.workspace_id.as_deref())?,
                        next_before: None,
                    });
                }
                // An explicit `?limit=0` is an empty ordinary page. Answer
                // here so a zero window never reaches the store, whose
                // `filter.limit == 0` convention means its internal default;
                // the exact-ID evidence lookup above keeps its own behavior.
                if window.limit == 0 {
                    return Ok(AuditListingPage {
                        events: Vec::new(),
                        next_before: None,
                    });
                }
                let post_filter = AuditPostFilter::from_predicates(predicates);
                let filter = AuditEventFilter {
                    before_id,
                    exclude_unverified_success,
                    limit: window.limit,
                    offset: window.offset,
                    ..columns.into()
                };
                if post_filter.is_empty() {
                    // Every requested predicate has a column, so the page is
                    // exactly the SQL window: no prefetch, no Rust-side slicing.
                    self.audit_listing_store_window(&filter)
                } else {
                    self.audit_listing_scan(filter, &post_filter)
                }
            }
        }
    }

    fn audit_listing_store_window(
        &self,
        filter: &AuditEventFilter,
    ) -> Result<AuditListingPage, OrbitError> {
        let events = self.list_audit_events_filtered(filter)?;
        let next_before = if events.len() >= filter.limit {
            events.last().map(|event| event.id)
        } else {
            None
        };
        Ok(AuditListingPage {
            events,
            next_before,
        })
    }

    /// Walk the SQL window in `AUDIT_HTTP_MAX_LIMIT` batches, keeping rows that
    /// pass `post_filter`, until `offset + limit` matches are in hand, the store
    /// runs dry, or the scan cap is reached. `next_before` resumes after the
    /// last row examined, so a capped scan can be continued instead of looking
    /// finished.
    fn audit_listing_scan(
        &self,
        mut filter: AuditEventFilter,
        post_filter: &AuditPostFilter,
    ) -> Result<AuditListingPage, OrbitError> {
        let limit = filter.limit;
        let mut to_skip = filter.offset;
        let mut events = Vec::new();
        let mut scanned = 0usize;
        let mut last_scanned_id = None;
        let mut exhausted = false;
        filter.limit = AUDIT_HTTP_MAX_LIMIT;
        filter.offset = 0;
        while events.len() < limit && scanned < AUDIT_HTTP_POST_FILTER_SCAN_CAP {
            let batch = self.list_audit_events_filtered(&filter)?;
            let fetched = batch.len();
            scanned += fetched;
            last_scanned_id = batch.last().map(|event| event.id).or(last_scanned_id);
            // Keep only the requested page; matches before `offset` are counted,
            // not buffered.
            for event in batch.into_iter().filter(|e| post_filter.matches(e)) {
                if to_skip > 0 {
                    to_skip -= 1;
                } else if events.len() < limit {
                    events.push(event);
                }
            }
            if fetched < AUDIT_HTTP_MAX_LIMIT {
                exhausted = true;
                break;
            }
            filter.offset += fetched;
        }
        let next_before = if events.len() >= limit {
            events.last().map(|event| event.id)
        } else if exhausted {
            None
        } else {
            last_scanned_id
        };
        Ok(AuditListingPage {
            events,
            next_before,
        })
    }

    pub fn show_audit_event(&self, id: i64) -> Result<AuditEvent, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event(id)?
            .ok_or_else(|| OrbitError::InvalidInput(format!("audit event not found: {id}")))
    }

    pub fn prune_audit_events(&self, older_than: &DateTime<Utc>) -> Result<usize, OrbitError> {
        self.stores().audit_events().prune_audit_events(older_than)
    }

    pub fn audit_event_stats(
        &self,
        since: Option<DateTime<Utc>>,
        tool: Option<String>,
    ) -> Result<AuditStats, OrbitError> {
        let (total, success_count, failure_count, denied_count, avg_duration_ms, max_duration_ms) =
            self.stores()
                .audit_events()
                .get_audit_event_stats(since.as_ref(), tool.as_deref())?;

        let durations = self
            .stores()
            .audit_events()
            .get_audit_event_durations(since.as_ref(), tool.as_deref())?;

        let p95_duration_ms = compute_p95(&durations);

        Ok(AuditStats {
            total,
            success_count,
            failure_count,
            denied_count,
            avg_duration_ms,
            p95_duration_ms,
            max_duration_ms,
        })
    }

    /// Audit events in `since`'s window, or every stored event when `since`
    /// is `None`.
    ///
    /// This is [`AuditStats::total`] without the duration samples
    /// [`Self::audit_event_stats`] reads and sorts to compute a percentile.
    pub fn audit_event_count(&self, since: Option<DateTime<Utc>>) -> Result<i64, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_count(since.as_ref())
    }

    pub fn record_audit_event(&self, params: &AuditEventInsertParams) -> Result<(), OrbitError> {
        self.record_audit_event_with_invocation(params, AuditInvocationFields::default())
    }

    /// Hourly count buckets `(rfc3339_hour_start, count)` for audit events at or
    /// after `since`. Empty hours are NOT in the result; callers must zero-fill
    /// when rendering a sparkline that needs every bucket.
    pub fn audit_event_hourly_buckets(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_hourly_buckets(since)
    }

    /// Canonical SQLite capability/policy decisions by operation, excluding
    /// coordination and settlement refusals and duplicate invocation evidence.
    pub fn audit_denials_by_operation(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_denials_by_operation(since)
    }

    /// Shared policy-denial population for the dashboard and audit CLI.
    /// Raw `AuditStats::denied_count` remains the forensic row count.
    pub fn audit_policy_denial_stats(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<AuditPolicyDenialStats, OrbitError> {
        use orbit_store::contracts::V2AuditEventFilter;
        use orbit_types::workflow::activity_job::{
            V2_DENIAL_EVENT_TYPES, V2_EVENT_TYPE_FS_CALL_DENIED, V2_EVENT_TYPE_STEP_DENIED,
        };

        let mut operations: BTreeMap<String, i64> = self
            .audit_denials_by_operation(since)?
            .into_iter()
            .collect();
        let sql_denied = operations.values().sum();
        let mut v2_denied = 0;
        const PAGE: usize = 1000;
        for event_type in V2_DENIAL_EVENT_TYPES {
            let mut offset = 0;
            loop {
                let events = self.list_v2_audit_events(V2AuditEventFilter {
                    since: since.copied(),
                    event_type: Some((*event_type).to_string()),
                    limit: Some(PAGE),
                    offset: Some(offset),
                    ..Default::default()
                })?;
                let fetched = events.len();
                for event in events {
                    let Ok(payload) =
                        serde_json::from_str::<serde_json::Value>(&event.payload_json)
                    else {
                        continue;
                    };
                    let operation = if *event_type == V2_EVENT_TYPE_FS_CALL_DENIED {
                        format!("fs.{}", payload["op"].as_str().unwrap_or("unknown"))
                    } else if *event_type == V2_EVENT_TYPE_STEP_DENIED {
                        payload["step_id"].as_str().unwrap_or("step").to_string()
                    } else {
                        payload["tool_name"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string()
                    };
                    *operations.entry(operation).or_default() += 1;
                    v2_denied += 1;
                }
                if fetched < PAGE {
                    break;
                }
                offset += fetched;
            }
        }
        let mut by_operation: Vec<_> = operations.into_iter().collect();
        by_operation.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(AuditPolicyDenialStats {
            sql_denied,
            v2_denied,
            by_operation,
        })
    }

    /// Audited tool invocations (`failed` counts every non-success run),
    /// split by how each row's identity was established [ORB-10890].
    ///
    /// Authenticated, self-reported, and anonymous traffic are disjoint
    /// buckets carrying their own `total`/`failed`, so a per-agent fail rate
    /// can name the denominator it was computed over instead of quietly
    /// dropping the ~92% of MCP calls Orbit cannot authenticate.
    pub fn audit_tool_call_counts_by_attribution(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditAttributionAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_tool_call_counts_by_attribution(since)
    }

    /// Per-tool aggregate of audit events at or after `since`. One row per
    /// distinct `tool_name` (NULL folded into `"unknown"`), each carrying
    /// `total`, `failures`, MCP/CLI surface counts, and `avg_duration_ms`.
    /// Backs the audit-summary side panel.
    pub fn audit_event_aggregates_by_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_aggregates_by_tool(since)
    }

    /// Callable tool calls at or after `since`, one row per tool and
    /// server-derived model. The per-tool aggregate is unchanged.
    pub fn audit_event_aggregates_by_tool_model(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolModelAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_aggregates_by_tool_model(since)
    }

    /// Per-role aggregate of audit events at or after `since`, with the
    /// MCP-vs-CLI surface split (`mcp` = `subcommand='run-mcp'`, `cli` =
    /// `subcommand='run'`). Drives the MCP-vs-CLI card.
    pub fn audit_event_aggregates_by_role(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditRoleAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_aggregates_by_role(since)
    }

    /// Per-canonical-actor aggregate of audit events at or after `since`
    /// [ORB-10888].
    ///
    /// Same window and same MCP-vs-CLI split as
    /// [`Self::audit_event_aggregates_by_role`], but grouped on the canonical
    /// actor: one agent recorded as `claude`, `opus`, and `claude-opus-5` is a
    /// single row, and `kind` separates synthetic and human actors from real
    /// agents without matching on the label.
    pub fn audit_event_aggregates_by_actor(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditActorAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_aggregates_by_actor(since)
    }

    /// Failure incidents grouped from the raw failed/denied audit rows in
    /// `query`'s window [ORB-10871]. The report carries both the incident
    /// list and the raw event denominators it was derived from, so a caller
    /// never has to infer one count from the other. Raw rows are neither
    /// mutated nor withheld — `list_audit_events_filtered` remains the
    /// authority on evidence.
    pub fn audit_failure_incidents(
        &self,
        query: &FailureIncidentQuery,
    ) -> Result<FailureIncidentReport, OrbitError> {
        self.stores().audit_events().get_failure_incidents(query)
    }
}

/// Nearest-rank p95 over a slice of `duration_ms` values that the caller has
/// already sorted ascending. Returns 0 for an empty slice.
pub fn compute_p95(sorted_durations: &[i64]) -> i64 {
    if sorted_durations.is_empty() {
        return 0;
    }
    let idx = ((sorted_durations.len() as f64) * 0.95).ceil() as usize;
    let idx = idx.min(sorted_durations.len()) - 1;
    sorted_durations[idx]
}

/// Decision counts shared by the audit CLI and dashboard KPI.
pub struct AuditPolicyDenialStats {
    /// Canonical SQLite capability and policy decisions.
    pub sql_denied: i64,
    /// Policy decisions in the workspace's v2 audit envelope.
    pub v2_denied: i64,
    /// Combined decisions by operation.
    pub by_operation: Vec<(String, i64)>,
}
