//! The workspace scan behind the codebase analytics: one pass over every task
//! (envelope plus history), the reservation history and the lock-conflict
//! denials, reduced to the per-task facts the rankings slice by window.
//!
//! The scan reads every task bundle's history. A caller that serves repeated
//! requests (the dashboard) caches the facts and slices every window,
//! threshold and rollup from them; the CLI and the tool scan once per call.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::selector::Selector;
use orbit_store::contracts::AuditEventFilter;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, TaskHistoryEntry, TaskRelationType,
    TaskStatus,
};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::workflow::{BUBBLEWRAP_NAMESPACE_REFUSALS, ProviderFailureClass};
use serde_json::Value;

use crate::OrbitRuntime;

/// Lock-conflict denials read per scan. Audit rows are pruned by retention,
/// so this bounds a pathological retry storm rather than normal history.
const LOCK_DENIAL_SCAN_LIMIT: usize = 50_000;
const TASK_LOCK_RESERVE_TOOL: &str = "orbit.task.locks.reserve";

/// Status events that put a task in `blocked` because a run judged the work:
/// the failure is attributed to the task's footprint unless its text names an
/// infrastructure cause.
const RUN_BLOCK_EVENTS: &[&str] = &[
    "workflow_run_failed",
    "task_blocked_by_agent",
    "final_recovery_escalated",
];
const RUN_INTERRUPTED_EVENT: &str = "workflow_run_interrupted";

/// A footprint entry: a workspace-relative path and whether it names a
/// directory (`dir:` selector) rather than a file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Anchor {
    pub(super) path: String,
    pub(super) dir: bool,
}

impl Anchor {
    /// Parse a context selector (`file:`, `dir:`, `symbol:`) or a bare
    /// reservation path. Module and command selectors carry no path.
    pub(super) fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let selector = if raw.contains(':') && !raw.starts_with('/') {
            Selector::from_str(raw).ok()
        } else {
            None
        };
        let (path, dir) = match selector {
            Some(Selector::Dir { path }) => (path, true),
            Some(Selector::File { path } | Selector::Symbol { path, .. }) => (path, false),
            Some(Selector::Module { .. } | Selector::Command { .. }) => return None,
            None => (raw.to_string(), false),
        };
        let path = path
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        if path.is_empty() || path == "." {
            return None;
        }
        Some(Self { path, dir })
    }
}

/// Why a task entered `blocked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockCause {
    /// An infrastructure failure the run failure classification names; never
    /// counted against the task's files.
    Infra(&'static str),
    /// A run failed or escalated on the work itself: counted against the
    /// task's footprint.
    Run,
    /// Nothing identifies the cause; reported in its own bucket.
    Unclassified,
}

/// Infrastructure causes, in the order the dashboard lists them.
pub(super) const INFRA_CAUSES: &[&str] = &[
    "auth",
    "sandbox",
    "disk",
    "provider_limit",
    "provider_unavailable",
    "interrupted",
];

#[derive(Debug, Clone)]
pub(super) struct TaskFacts {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) crew: Option<String>,
    pub(super) status: TaskStatus,
    /// The declared footprint: `context_files`, widened selectors included.
    pub(super) footprint: Vec<Anchor>,
    /// Last update; places the task in a window for denominators.
    pub(super) updated_at: DateTime<Utc>,
    /// When the task last became `done`, while it is done.
    pub(super) done_at: Option<DateTime<Utc>>,
    /// When the task last became `rejected`, while it is rejected.
    pub(super) rejected_at: Option<DateTime<Utc>>,
    /// Whether any job run ever picked the task up.
    pub(super) had_run: bool,
    /// Each `context_files_widened` event: when, and the paths it added.
    pub(super) widenings: Vec<(DateTime<Utc>, Vec<Anchor>)>,
    /// Each entry into `blocked`, with its classified cause.
    pub(super) blocks: Vec<(DateTime<Utc>, BlockCause)>,
    /// Distinct tasks whose `regression_from` relation targets this task.
    pub(super) regressed_by: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ReservationFacts {
    pub(super) at: DateTime<Utc>,
    /// Each requested task, or the owning run for a file-only reservation.
    /// Without either, the reservation itself identifies one request.
    pub(super) requesters: Vec<String>,
    pub(super) files: Vec<Anchor>,
}

#[derive(Debug, Clone)]
pub(super) struct LockDenial {
    pub(super) at: DateTime<Utc>,
    /// Who waited: each requested task, or the run (or invocation) when the
    /// request named no task. A retry loop repeats the same contender.
    pub(super) contenders: Vec<String>,
    /// The files the request conflicted on.
    pub(super) files: Vec<Anchor>,
}

/// One workspace scan: the per-task facts every ranking slices by window,
/// minimum support and rollup.
#[derive(Debug)]
pub struct CodebaseFacts {
    pub(super) computed_at: DateTime<Utc>,
    pub(super) tasks: Vec<TaskFacts>,
    pub(super) reservations: Vec<ReservationFacts>,
    pub(super) lock_denials: Vec<LockDenial>,
}

/// Scan the workspace once. A task whose history cannot be read keeps its
/// envelope facts with an empty history rather than failing the whole view.
pub(super) fn scan(runtime: &OrbitRuntime) -> Result<CodebaseFacts, OrbitError> {
    let computed_at = Utc::now();
    let tasks = runtime.list_task_metadata()?;

    let mut regressed_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for task in &tasks {
        for relation in &task.relations {
            if relation.relation_type == TaskRelationType::RegressionFrom {
                regressed_by
                    .entry(relation.target.to_string())
                    .or_default()
                    .insert(task.id.to_string());
            }
        }
    }

    let mut facts = Vec::with_capacity(tasks.len());
    for task in tasks {
        let id = task.id.to_string();
        let history = runtime.get_task_history(&id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "codebase analytics: task history unreadable");
            Vec::new()
        });
        let entered = |status: TaskStatus| {
            history
                .iter()
                .rev()
                .find(|entry| entry.to_status == Some(status))
                .map(|entry| entry.at)
        };
        let done_at = (task.status == TaskStatus::Done)
            .then(|| entered(TaskStatus::Done).unwrap_or(task.updated_at));
        let rejected_at = (task.status == TaskStatus::Rejected)
            .then(|| entered(TaskStatus::Rejected).unwrap_or(task.updated_at));
        let widenings: Vec<_> = history
            .iter()
            .filter(|entry| entry.event == CONTEXT_FILES_WIDENED_EVENT)
            .filter_map(|entry| {
                let widening = ContextFilesWidening::from_entry(entry)?;
                let anchors = widening
                    .selectors
                    .iter()
                    .filter_map(|selector| Anchor::parse(selector))
                    .collect::<Vec<_>>();
                Some((entry.at, anchors))
            })
            .collect();
        let had_run = task.job_run_id.is_some()
            || !widenings.is_empty()
            || history.iter().any(entry_marks_run);
        let blocks = history
            .iter()
            .filter(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .map(|entry| {
                (
                    entry.at,
                    classify_block(&entry.event, entry.note.as_deref()),
                )
            })
            .collect();
        let mut footprint: Vec<Anchor> = task
            .context_files
            .iter()
            .filter_map(|selector| Anchor::parse(selector))
            .collect();
        footprint.sort();
        footprint.dedup();
        facts.push(TaskFacts {
            regressed_by: regressed_by.remove(&id).unwrap_or_default(),
            id,
            title: task.title,
            crew: task.crew.or(task.implemented_by),
            status: task.status,
            footprint,
            updated_at: task.updated_at,
            done_at,
            rejected_at,
            had_run,
            widenings,
            blocks,
        });
    }

    let reservations = runtime
        .list_task_reservation_history()?
        .into_iter()
        .filter_map(|record| {
            let at = DateTime::parse_from_rfc3339(&record.created_at)
                .ok()?
                .with_timezone(&Utc);
            Some(ReservationFacts {
                at,
                requesters: requester_identities(
                    record.task_ids,
                    record.owner_run_id,
                    format!("reservation:{}", record.reservation_id),
                ),
                files: record
                    .files
                    .iter()
                    .filter_map(|file| Anchor::parse(file))
                    .collect(),
            })
        })
        .collect();

    // Filter in the store before applying the limit, so other workspaces
    // cannot crowd out local evidence. Legacy unscoped denials cannot be
    // safely attributed from task IDs or shared relative paths; exclude them.
    let lock_denials = runtime
        .list_audit_events_filtered(&AuditEventFilter {
            workspace_id: Some(runtime.workspace_id()?),
            tool_name: Some(TASK_LOCK_RESERVE_TOOL.to_string()),
            status: Some(AuditEventStatus::Denied),
            limit: LOCK_DENIAL_SCAN_LIMIT,
            ..Default::default()
        })?
        .iter()
        .filter_map(lock_denial)
        .collect();

    Ok(CodebaseFacts {
        computed_at,
        tasks: facts,
        reservations,
        lock_denials,
    })
}

/// A history entry only a job run writes: worktree setup moving the task to
/// `in-progress`, or a run outcome naming its `run_id`.
fn entry_marks_run(entry: &TaskHistoryEntry) -> bool {
    entry.event == "started"
        || entry.to_status == Some(TaskStatus::InProgress)
        || entry
            .note
            .as_deref()
            .is_some_and(|note| note.contains("run_id="))
}

/// A lock-conflict denial from `orbit.task.locks.reserve`. Capability
/// refusals and denials without a conflict are not contention.
fn lock_denial(event: &AuditEvent) -> Option<LockDenial> {
    if event.command == "authorization" {
        return None;
    }
    let arguments: Value = serde_json::from_str(event.arguments_json.as_deref()?).ok()?;
    let strings = |key: &str| -> Vec<String> {
        arguments
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()
    };
    let conflicts = arguments.get("conflicts").and_then(Value::as_array)?;
    if conflicts.is_empty() {
        return None;
    }
    let mut files: Vec<Anchor> = conflicts
        .iter()
        .filter_map(|conflict| conflict.get("file").and_then(Value::as_str))
        .filter_map(Anchor::parse)
        .collect();
    if files.is_empty() {
        files = strings("files")
            .iter()
            .filter_map(|f| Anchor::parse(f))
            .collect();
    }
    files.sort();
    files.dedup();
    let run = arguments
        .get("owner_run_id")
        .and_then(Value::as_str)
        .filter(|run| !run.is_empty())
        .map(str::to_string)
        .or_else(|| event.job_run_id.clone());
    let contenders = requester_identities(
        strings("task_ids"),
        run,
        format!("invocation:{}", event.execution_id),
    );
    Some(LockDenial {
        at: event.timestamp,
        contenders,
        files,
    })
}

/// Task retries retain their task identity across runs. Requests without
/// tasks use the run, or one recorded request when no run is known.
fn requester_identities(
    task_ids: Vec<String>,
    run: Option<String>,
    fallback: String,
) -> Vec<String> {
    if !task_ids.is_empty() {
        return task_ids;
    }
    vec![
        run.filter(|run| !run.is_empty())
            .map(|run| format!("run:{run}"))
            .unwrap_or(fallback),
    ]
}

/// Classify one entry into `blocked` from its status event and note. The
/// note of a run failure carries the run's `error_code` and error text, which
/// the run failure classification reads.
pub(super) fn classify_block(event: &str, note: Option<&str>) -> BlockCause {
    let text = note.unwrap_or("");
    if let Some(cause) = infra_cause(text) {
        return BlockCause::Infra(cause);
    }
    if event == RUN_INTERRUPTED_EVENT {
        return BlockCause::Infra("interrupted");
    }
    if RUN_BLOCK_EVENTS.contains(&event) || text.contains("run_id=") {
        return BlockCause::Run;
    }
    BlockCause::Unclassified
}

fn infra_cause(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    // A component or test name is not evidence of a failed infrastructure
    // operation. Match concrete diagnostics, outside identifier tokens.
    let any = |needles: &[&str]| {
        needles.iter().any(|needle| {
            let needle = needle.to_ascii_lowercase();
            lower.match_indices(&needle).any(|(start, matched)| {
                let identifier = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
                !lower[..start].chars().next_back().is_some_and(identifier)
                    && !lower[start + matched.len()..]
                        .chars()
                        .next()
                        .is_some_and(identifier)
            })
        })
    };
    if any(BUBBLEWRAP_NAMESPACE_REFUSALS)
        || any(&[
            "sandbox_apply: Operation not permitted",
            "sandbox-exec cannot apply a profile",
            "trusted sandbox-exec not available at",
            "Bubblewrap capability probe failed",
            "trusted Bubblewrap not available at",
            "bwrap: Creating new namespace failed:",
            "Orbit linux-bwrap policy denied the attempted write",
            "macOS sandbox hid the stored credential",
        ])
    {
        return Some("sandbox");
    }
    if any(&[
        "oauth session expired",
        "authentication failed",
        "provider authentication failure",
        "failed to authenticate",
        "authentication required",
        "not authenticated",
        "no authentication information found",
        "401 unauthorized",
        "http 401",
        "oauth token revoked",
        "invalid api key",
        "api key not valid",
        "login required",
        "please log in",
        "please login",
        "token expired",
        "expired token",
        "invalid token",
        "token has expired",
        "not logged in",
        "real login failure",
        "provider_unavailable/auth",
    ]) {
        return Some("auth");
    }
    if any(&["no space left", "enospc", "disk full", "disk quota"]) {
        return Some("disk");
    }
    let error_code = text
        .split_once("error_code=")
        .map(|(_, rest)| rest.split([',', ' ']).next().unwrap_or(""))
        .filter(|code| !code.is_empty() && *code != "-");
    match ProviderFailureClass::of(error_code, Some(text)) {
        // Terse provider responses need the typed infrastructure evidence:
        // these tokens alone also occur as symbols in repository failures.
        Some(ProviderFailureClass::Unavailable)
            if any(&[
                "unauthorized",
                "unauthenticated",
                "authentication_error",
                "invalid_api_key",
            ]) =>
        {
            Some("auth")
        }
        Some(ProviderFailureClass::Limit | ProviderFailureClass::Capacity) => {
            Some("provider_limit")
        }
        Some(ProviderFailureClass::Unavailable | ProviderFailureClass::Refusal) => {
            Some("provider_unavailable")
        }
        None => None,
    }
}
