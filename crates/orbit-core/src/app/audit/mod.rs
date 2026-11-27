//! The audit app: `orbit audit`, `orbit log tail`, and the dashboard's audit
//! listing, summary and denial read models.

mod event;

mod summary;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{parse_duration_seconds, parse_since_with};
use orbit_store::contract::AuditEventFilter;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus, AuditStats};
use orbit_types::workflow::activity_job::V2_DENIAL_EVENT_TYPES;

use super::exec::{self, App, Operation};
use crate::Core;
pub use event::{
    AuditListingColumns, AuditListingExactIds, AuditListingHttpWindow, AuditListingPage,
    AuditListingPredicates, AuditListingProfile, AuditPolicyDenialStats,
};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use event::{
    audit_denials_by_operation, audit_event_count, audit_event_stats, audit_failure_incidents,
    audit_policy_denial_stats, list_audit_events, list_audit_events_by_ids,
    list_audit_events_filtered, list_audit_events_with_kind, record_audit_event,
};
#[cfg(not(feature = "test-support"))]
pub(crate) use event::{
    audit_event_count, audit_failure_incidents, list_audit_events_by_ids,
    list_audit_events_filtered, record_audit_event,
};
pub use orbit_store::contract::V2AuditEventRow;
pub use summary::{AuditSummary, AuditToolDuration};

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use orbit_runtime::context::ActorIdentity;
pub use orbit_store::contract::V2AuditEventFilter;
pub use orbit_store::{
    AuditToolAggregate, AuditToolModelAggregate, InvocationInsertParams, InvocationQuery,
    InvocationRecord,
};

/// One audit event plus its application callback rows when it is an
/// application entry invocation.
pub struct AuditEventDetail {
    /// The requested audit event.
    pub event: AuditEvent,
    /// Callback audit rows, oldest first (empty when none or not an entry).
    pub callbacks: Vec<AuditEvent>,
    /// True when more callbacks existed than `callback_limit`.
    pub callbacks_incomplete: bool,
}

/// Raw denial evidence and canonical policy-decision counts for the dashboard
/// denials view.
pub struct AuditDenialsData {
    /// Matching v2 audit denial events across all denial event types.
    pub v2_events: Vec<V2AuditEventRow>,
    /// Matching SQLite denied audit events.
    pub sqlite_events: Vec<AuditEvent>,
    /// Canonical policy-denial decision totals for the window.
    pub policy_decisions: AuditPolicyDenialStats,
}

/// One audit or process-log command.
pub enum Command {
    /// List audit events for CLI or HTTP pagination.
    List {
        /// Shared column filters.
        columns: Box<AuditListingColumns>,
        /// Surface-specific pagination and post-filter profile.
        profile: AuditListingProfile,
    },
    /// Show one audit event and any application callback rows it owns.
    Show {
        /// Audit row ID.
        id: i64,
        /// Maximum callback rows to return.
        callback_limit: usize,
    },
    /// Export every matching audit event newest-first.
    Export {
        /// Optional lower timestamp cutoff.
        since: Option<DateTime<Utc>>,
        /// Optional canonical tool filter.
        tool: Option<String>,
    },
    /// Compute audit event statistics and policy-denial counts.
    Stats {
        /// Optional lower timestamp cutoff.
        since: Option<DateTime<Utc>>,
        /// Optional canonical tool filter.
        tool: Option<String>,
    },
    /// Prune audit events older than `older_than` (a duration or timestamp
    /// parsed after command authorization so relative cutoffs are evaluated
    /// after any authorization audit row is recorded).
    Prune {
        /// Raw duration or timestamp cutoff (parsed after authorization).
        older_than: String,
        /// Whether `--confirm` was passed (also gates command authorization).
        confirm: bool,
    },
    /// Compute the dashboard audit summary read model.
    Summary {
        /// Lower timestamp cutoff for the summary window.
        since: DateTime<Utc>,
        /// Maximum raw failure rows scanned for grouped incidents.
        incident_scan_limit: usize,
    },
    /// Read denial evidence and policy-denial decision counts for the
    /// dashboard denials view.
    Denials {
        /// Optional lower timestamp cutoff.
        since: Option<DateTime<Utc>>,
        /// Optional agent/role filter.
        agent_filter: Option<String>,
        /// Maximum rows fetched per store query.
        scan_limit: usize,
    },
    /// Resolve the unified JSONL log feed path for `orbit log tail`.
    LogTail {
        /// Explicit `--path` override, if supplied.
        path: Option<PathBuf>,
    },
}

/// An admitted `orbit log tail` stream: carries the resolved log path and
/// keeps the command's invocation audit row open until the stream terminates.
pub struct LogTailSession {
    path: PathBuf,
    audit: exec::StreamAudit,
}

impl LogTailSession {
    /// The resolved JSONL log feed path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run the tail stream, then settle and persist the command's one
    /// invocation audit row with its final outcome and full elapsed duration.
    pub fn run(
        self,
        stream: impl FnOnce(&Path) -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        let result = stream(&self.path);
        self.audit.finish(&result);
        result
    }
}

/// What an audit command produced, before the caller renders it.
pub enum Outcome {
    /// One page of audit events.
    Listed(AuditListingPage),
    /// One audit event with its optional callback rows.
    Shown(Box<AuditEventDetail>),
    /// Exported audit events.
    Exported(Vec<AuditEvent>),
    /// Aggregate audit statistics and policy-denial counts.
    Stats {
        /// Raw forensic event statistics.
        stats: AuditStats,
        /// Canonical capability/policy denial counts.
        policy_denials: AuditPolicyDenialStats,
    },
    /// Count of pruned audit rows.
    Pruned(usize),
    /// Dashboard audit summary read model.
    Summary(Box<AuditSummary>),
    /// Dashboard denials read model.
    Denials(Box<AuditDenialsData>),
    /// Admitted log-tail session whose audit row completes when the stream ends.
    LogTail(Box<LogTailSession>),
}

/// Why an audit command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The flow failed.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct AuditApp;

impl App for AuditApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        match command {
            // `orbit audit` emits no command-level invocation row (it would
            // audit reads of the audit log); authorization refusals still
            // write their own decision row.
            Command::List { .. } => Operation::command("audit", "list").without_invocation_audit(),
            Command::Show { .. } => Operation::command("audit", "show").without_invocation_audit(),
            Command::Export { .. } => {
                Operation::command("audit", "export").without_invocation_audit()
            }
            Command::Stats { .. } => {
                Operation::command("audit", "stats").without_invocation_audit()
            }
            Command::Prune { confirm: true, .. } => {
                Operation::command("audit", "prune").without_invocation_audit()
            }
            Command::Prune { confirm: false, .. } => Operation::command("audit", "prune")
                .without_command_row()
                .without_invocation_audit(),
            Command::Summary { .. } => {
                Operation::command("audit", "summary").without_invocation_audit()
            }
            Command::Denials { .. } => {
                Operation::command("audit", "denials").without_invocation_audit()
            }
            Command::LogTail { .. } => {
                unreachable!("log tail uses stream admission")
            }
        }
    }

    fn run(core: &Core, command: Self::Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::List { columns, profile } => {
                Outcome::Listed(event::audit_listing_page(runtime, *columns, profile)?)
            }
            Command::Show { id, callback_limit } => {
                let event = event::show_audit_event(runtime, id)?;
                let (callbacks, callbacks_incomplete) = if event
                    .application
                    .as_ref()
                    .is_some_and(|application| !application.callback)
                {
                    let mut callbacks = event::list_audit_events_filtered(
                        runtime,
                        &AuditEventFilter {
                            application_entry_execution_id: Some(event.execution_id.clone()),
                            limit: callback_limit + 1,
                            ..AuditEventFilter::default()
                        },
                    )?;
                    let incomplete = callbacks.len() > callback_limit;
                    callbacks.truncate(callback_limit);
                    callbacks.reverse();
                    (callbacks, incomplete)
                } else {
                    (Vec::new(), false)
                };
                Outcome::Shown(Box::new(AuditEventDetail {
                    event,
                    callbacks,
                    callbacks_incomplete,
                }))
            }
            Command::Export { since, tool } => {
                Outcome::Exported(event::export_audit_events(runtime, since, tool)?)
            }
            Command::Stats { since, tool } => {
                let stats = event::audit_event_stats(runtime, since, tool)?;
                let policy_denials = event::audit_policy_denial_stats(runtime, since.as_ref())?;
                Outcome::Stats {
                    stats,
                    policy_denials,
                }
            }
            Command::Prune {
                older_than,
                confirm,
            } => {
                // Parse the cutoff after `exec::run` authorizes the command so
                // relative durations like `0s` compute `Utc::now()` after any
                // authorization override audit row is written, while still
                // reporting duration typos before asking for `--confirm`.
                let cutoff = parse_since_with(&older_than, parse_duration_seconds)?;
                if !confirm {
                    return Err(OrbitError::InvalidInput(
                        "audit pruning is irreversible; pass --confirm to proceed".to_string(),
                    )
                    .into());
                }
                Outcome::Pruned(event::prune_audit_events(runtime, &cutoff)?)
            }
            Command::Summary {
                since,
                incident_scan_limit,
            } => Outcome::Summary(Box::new(summary::read(core, since, incident_scan_limit)?)),
            Command::Denials {
                since,
                agent_filter,
                scan_limit,
            } => {
                let mut v2_events = Vec::new();
                for event_type in V2_DENIAL_EVENT_TYPES {
                    v2_events.extend(runtime.list_v2_audit_events(V2AuditEventFilter {
                        workspace_id: String::new(),
                        since,
                        event_type: Some((*event_type).to_string()),
                        limit: Some(scan_limit),
                        ..Default::default()
                    })?);
                }
                let sqlite_events = event::list_audit_events(
                    runtime,
                    since,
                    None,
                    Some(AuditEventStatus::Denied),
                    agent_filter,
                    scan_limit,
                )?;
                let policy_decisions = event::audit_policy_denial_stats(runtime, since.as_ref())?;
                Outcome::Denials(Box::new(AuditDenialsData {
                    v2_events,
                    sqlite_events,
                    policy_decisions,
                }))
            }
            Command::LogTail { .. } => unreachable!("log tail uses stream admission"),
        })
    }
}

/// Authorize, audit and run one audit command.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    match command {
        Command::LogTail { path } => {
            let operation = Operation::command("log", "tail").target(Some("log_feed"), None);
            let (resolved, audit) =
                exec::start_stream(core, operation, || resolve_log_path(core, path.as_deref()))?;
            Ok(Outcome::LogTail(Box::new(LogTailSession {
                path: resolved,
                audit,
            })))
        }
        command => exec::run::<AuditApp>(core, command),
    }
}

fn resolve_log_path(core: &Core, override_path: Option<&Path>) -> Result<PathBuf, OrbitError> {
    if let Some(path) = override_path {
        return Ok(path.to_path_buf());
    }
    let env = core.runtime().env();
    if let Some(path) = env.orbit_log_path().and_then(OsStr::to_str)
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    orbit_common::observability::logging::global_jsonl_log_path(env).map_err(|err| {
        OrbitError::InvalidInput(format!("cannot resolve global JSONL log path: {err}"))
    })
}
