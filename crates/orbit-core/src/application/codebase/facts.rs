//! The workspace scan behind the codebase analytics: one pass over every task
//! (envelope plus history), the reservation history and the lock-conflict
//! denials, reduced to the per-task facts the rankings slice by window.
//!
//! Lock waits are read from the same records: a task's audited denials say
//! when it started waiting, and its next reservation grant (or its leaving
//! the backlog) says when the wait ended.
//!
//! The scan reads every task bundle's history. A caller that serves repeated
//! requests (the dashboard) caches the facts and slices every window,
//! threshold and rollup from them; the CLI and the tool scan once per call.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::selector::Selector;
use orbit_store::contracts::{
    AuditEventFilter, ClaimFailedNote, ClaimSettlementEvidence, ExecutionClaimPhase,
};
use orbit_types::identity::all_agent_families;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, NO_DIFF_EXPECTED_TAG, TaskHistoryEntry,
    TaskRelationType, TaskStatus,
};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::workflow::{
    BUBBLEWRAP_NAMESPACE_REFUSALS, ClaimFailureClass, FORGE_UNAVAILABLE_EXPIRED_EVENT,
    ProviderFailureClass,
};
use serde_json::Value;

use crate::OrbitRuntime;

/// Lock-conflict denials read per scan. Audit rows are pruned by retention,
/// so this bounds a pathological retry storm rather than normal history.
const LOCK_DENIAL_SCAN_LIMIT: usize = 50_000;
const TASK_LOCK_RESERVE_TOOL: &str = "orbit.task.locks.reserve";

/// Status events that put a task in `blocked` because a run judged the work:
/// the failure is attributed to the task's footprint unless its text names an
/// infrastructure cause. A before-PR review gate's escalation judged the
/// candidate too.
const RUN_BLOCK_EVENTS: &[&str] = &[
    "workflow_run_failed",
    "task_blocked_by_agent",
    "final_recovery_escalated",
    "review_gate_escalation",
];
const RUN_INTERRUPTED_EVENT: &str = "workflow_run_interrupted";
/// A claimed leaf's failure settlement, recorded on its owner.
const CLAIM_FAILED_EVENT: &str = "claim_failed";
/// The failure handoff of a run whose candidate could not be carried onto
/// its moved base, with the conflicting paths it recorded.
const CONFLICT_BLOCKED_EVENT: &str = "pr_conflict_blocked";
/// A run's failure handoff; a base conflict when the step that failed was
/// `sync_base`, a failure of the work otherwise.
const FAILURE_HANDOFF_EVENT: &str = "pr_failure_handoff";
const SYNC_BASE_STEP: &str = "sync_base";
/// Required validation could not find a tool: the host, not the candidate.
const VALIDATION_ENVIRONMENT_EVENT: &str = "validation_environment_blocked";
/// Status writes an operator, agent session or automation made by hand.
const MANUAL_BLOCK_EVENTS: &[&str] = &["status_changed", "claim_revoked", "forced"];
/// How far a claim settlement's time may stand from the `claim_failed` entry
/// it explains: both are stamped in the same owner commit.
const SETTLEMENT_MATCH_SECONDS: i64 = 300;

/// A footprint entry: a workspace-relative path and whether it names a
/// directory (`dir:` selector) rather than a file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::application) struct Anchor {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::application) enum BlockCause {
    /// An infrastructure failure the run failure classification names; never
    /// counted against the task's files.
    Infra(&'static str),
    /// A run failed or escalated on the work itself: counted against the
    /// task's footprint.
    Run,
    /// The candidate could not be carried onto a base that moved under it:
    /// counted only against the conflicting paths it recorded.
    BaseConflict(Vec<Anchor>),
    /// A status write made by hand, by the named [actor kind](actor_kind).
    Manual(&'static str),
    /// Not a failure: a review evidence hold or an operator's cancel.
    NotFailure,
    /// Nothing identifies the cause; reported in its own bucket.
    Unclassified,
}

/// One entry into `blocked`: when, the history event that wrote it, and its
/// classified cause.
#[derive(Debug, Clone)]
pub(super) struct Block {
    pub(super) at: DateTime<Utc>,
    pub(super) event: String,
    pub(super) cause: BlockCause,
}

/// Infrastructure causes, in the order the dashboard lists them.
pub(super) const INFRA_CAUSES: &[&str] = &[
    "auth",
    "sandbox",
    "disk",
    "provider_limit",
    "provider_unavailable",
    "forge_unavailable",
    "baseline_red",
    "environment",
    "owner_route",
    "transient",
    "interrupted",
];

/// Who made a manual block, in the order the dashboard lists them.
pub(super) const ACTOR_KINDS: &[&str] = &["operator", "agent", "system", "unknown"];

#[derive(Debug, Clone)]
pub(super) struct TaskFacts {
    pub(super) landed: Option<super::landed::Landed>,
    /// Landing receipt time, else done time: admits delivery fallback and
    /// places unavailable deliveries in the same window.
    pub(super) delivery_at: Option<DateTime<Utc>>,
    /// For a landed task, what its task-pilot receipts predicted.
    pub(super) pilot: Option<super::pilot::PilotFacts>,
    /// Tagged `no-diff-expected`: the task was not meant to change files.
    pub(super) no_diff: bool,
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
    pub(super) blocks: Vec<Block>,
    /// Distinct tasks whose `regression_from` relation targets this task.
    pub(super) regressed_by: BTreeSet<String>,
    /// When the task entered a status other than `backlog`, ascending: each
    /// ends any lock wait in progress, since only a backlog task is admitted.
    pub(super) left_backlog: Vec<DateTime<Utc>>,
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
    /// The tasks the request named; empty for a file-only request. Only a
    /// task's wait has an admission to end it.
    pub(super) task_ids: Vec<String>,
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
    /// When each task was granted a reservation, ascending: its admission.
    pub(super) admissions: BTreeMap<String, Vec<DateTime<Utc>>>,
}

/// Scan the workspace once. A task whose history cannot be read keeps its
/// envelope facts with an empty history rather than failing the whole view.
pub(super) fn scan(
    runtime: &OrbitRuntime,
    delivery_window: Option<&super::sections::Params>,
    include_landed: bool,
) -> Result<CodebaseFacts, OrbitError> {
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

    let settlements = failed_claim_settlements(runtime);
    let mut facts = Vec::with_capacity(tasks.len());
    let mut landed_scan = include_landed.then(|| super::landed::Scanner::new(runtime));
    let mut pilot_runs = None;
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
        let task_settlements = settlements.get(&id).map(Vec::as_slice).unwrap_or_default();
        let blocks = history
            .iter()
            .filter(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .map(|entry| Block {
                at: entry.at,
                event: entry.event.clone(),
                cause: classify_block(entry, task_settlements),
            })
            .collect();
        let mut footprint: Vec<Anchor> = task
            .context_files
            .iter()
            .filter_map(|selector| Anchor::parse(selector))
            .collect();
        footprint.sort();
        footprint.dedup();
        let mut left_backlog: Vec<DateTime<Utc>> = history
            .iter()
            .filter(|entry| entry.to_status.is_some_and(|to| to != TaskStatus::Backlog))
            .map(|entry| entry.at)
            .collect();
        left_backlog.sort();
        let receipt = super::landed::receipt(&history);
        let delivery_at =
            done_at.map(|at| receipt.as_ref().map_or(at, |(_, landed_at)| *landed_at));
        let landed = delivery_at
            .filter(|at| delivery_window.is_none_or(|params| params.within(*at)))
            .and_then(|_| {
                landed_scan
                    .as_mut()?
                    .read(runtime, &id, receipt, delivery_window.is_some())
            });
        let no_diff = task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG);
        let pilot = (landed.is_some() && !no_diff)
            .then(|| {
                let runs = pilot_runs.get_or_insert_with(|| super::pilot::PilotRuns::load(runtime));
                super::pilot::read(runtime, runs, &id, computed_at)
            })
            .flatten();
        facts.push(TaskFacts {
            landed,
            delivery_at,
            pilot,
            no_diff,
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
            left_backlog,
        });
    }

    let mut admissions: BTreeMap<String, Vec<DateTime<Utc>>> = BTreeMap::new();
    let reservations = runtime
        .list_task_reservation_history()?
        .into_iter()
        .filter_map(|record| {
            let at = DateTime::parse_from_rfc3339(&record.created_at)
                .ok()?
                .with_timezone(&Utc);
            for task_id in &record.task_ids {
                admissions.entry(task_id.clone()).or_default().push(at);
            }
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

    for grants in admissions.values_mut() {
        grants.sort();
    }

    Ok(CodebaseFacts {
        computed_at,
        tasks: facts,
        reservations,
        lock_denials,
        admissions,
    })
}

/// A history entry only a job run writes: worktree setup moving the task to
/// `in-progress`, or a run outcome naming its run.
fn entry_marks_run(entry: &TaskHistoryEntry) -> bool {
    entry.event == "started"
        || entry.to_status == Some(TaskStatus::InProgress)
        || entry.note.as_deref().is_some_and(names_run)
}

/// Whether a note names the run that wrote it: `run_id=<id>` as a run
/// failure writes it, or `run=<id>` as a run's failure handoff does. A field
/// that merely ends in `run`, such as `source_run=`, does not count.
fn names_run(note: &str) -> bool {
    ["run_id=", "run="].iter().any(|key| {
        note.match_indices(key).any(|(start, _)| {
            !note[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
    })
}

/// What a claimed leaf's failure settlement recorded: from its owner's
/// `claim_failed` note, or for an entry older than that note, from the
/// claim's settlement record.
#[derive(Debug, Clone)]
pub(in crate::application) struct SettledFailure {
    /// Set when the settlement was matched by time rather than read off the
    /// entry; the entry it explains is the nearest.
    settled_at: Option<DateTime<Utc>>,
    evidence: String,
    failure_class: Option<ClaimFailureClass>,
    leaf_run: bool,
    reason: String,
}

/// Every failed claim's settlement, by task, for `claim_failed` entries
/// written before their note named the cause. An owner whose claims cannot
/// be read classifies those entries as unclassified rather than failing.
pub(in crate::application) fn failed_claim_settlements(
    runtime: &OrbitRuntime,
) -> BTreeMap<String, Vec<SettledFailure>> {
    let settlements = runtime
        .leaf_settlements(None, false)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "codebase analytics: claim settlements unreadable");
            Vec::new()
        });
    let mut by_task: BTreeMap<String, Vec<SettledFailure>> = BTreeMap::new();
    for settlement in settlements {
        if settlement.phase != ExecutionClaimPhase::Failed {
            continue;
        }
        let Ok(settled_at) = DateTime::parse_from_rfc3339(&settlement.settled_at) else {
            continue;
        };
        by_task
            .entry(settlement.task_id)
            .or_default()
            .push(SettledFailure {
                settled_at: Some(settled_at.with_timezone(&Utc)),
                evidence: settlement.evidence.to_string(),
                failure_class: settlement.failure_class,
                leaf_run: settlement.leaf_run_id.is_some(),
                reason: settlement.reason.unwrap_or_default(),
            });
    }
    by_task
}

/// The settlement behind a `claim_failed` entry: the note it carries, else
/// the task's failed claim settled nearest to it.
fn settled_failure(
    entry: &TaskHistoryEntry,
    settlements: &[SettledFailure],
) -> Option<SettledFailure> {
    if let Some(note) = entry.note.as_deref().and_then(ClaimFailedNote::parse) {
        return Some(SettledFailure {
            settled_at: None,
            evidence: note.evidence.as_str().to_string(),
            failure_class: note.failure_class,
            leaf_run: note.run_id.is_some(),
            reason: note.reason,
        });
    }
    settlements
        .iter()
        .filter_map(|settlement| {
            let distance = (settlement.settled_at? - entry.at).num_seconds().abs();
            (distance <= SETTLEMENT_MATCH_SECONDS).then_some((distance, settlement))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, settlement)| settlement.clone())
}

/// The kind of actor a history entry's `by` label names: an agent family, an
/// Orbit automation (`system`, `system:*`), an unattributed write, or else a
/// person (`human:<user>`, `operator`, an `ORBIT_ACTOR` label, or the
/// machine of an operator's claim recovery).
pub(in crate::application) fn actor_kind(by: &str) -> &'static str {
    let by = by.trim();
    if all_agent_families().contains(&by) {
        "agent"
    } else if by == "system" || by.starts_with("system:") {
        "system"
    } else if by.is_empty() || by == "unknown" || by == "anonymous" {
        "unknown"
    } else {
        "operator"
    }
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
    let task_ids = strings("task_ids");
    let contenders = requester_identities(
        task_ids.clone(),
        run,
        format!("invocation:{}", event.execution_id),
    );
    Some(LockDenial {
        at: event.timestamp,
        contenders,
        task_ids,
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

/// Classify one entry into `blocked` from its status event, its typed
/// fields and, where no typed field names the cause, its note. The note of a
/// run failure carries the run's `error_code` and error text, which the run
/// failure classification reads.
pub(in crate::application) fn classify_block(
    entry: &TaskHistoryEntry,
    settlements: &[SettledFailure],
) -> BlockCause {
    let event = entry.event.as_str();
    let text = entry.note.as_deref().unwrap_or("");
    match event {
        CLAIM_FAILED_EVENT => {
            return match settled_failure(entry, settlements) {
                Some(settled) => claim_cause(&settled),
                None => BlockCause::Unclassified,
            };
        }
        CONFLICT_BLOCKED_EVENT => return BlockCause::BaseConflict(conflicting_paths(text)),
        FAILURE_HANDOFF_EVENT if note_field(text, "failed_step=") == Some(SYNC_BASE_STEP) => {
            return BlockCause::BaseConflict(conflicting_paths(text));
        }
        VALIDATION_ENVIRONMENT_EVENT => return BlockCause::Infra("environment"),
        FORGE_UNAVAILABLE_EXPIRED_EVENT => return BlockCause::Infra("forge_unavailable"),
        _ if MANUAL_BLOCK_EVENTS.contains(&event) => {
            return BlockCause::Manual(actor_kind(&entry.by));
        }
        _ => {}
    }
    if let Some(cause) = infra_cause(text) {
        return BlockCause::Infra(cause);
    }
    if event == RUN_INTERRUPTED_EVENT {
        return BlockCause::Infra("interrupted");
    }
    if RUN_BLOCK_EVENTS.contains(&event) || event == FAILURE_HANDOFF_EVENT || names_run(text) {
        return BlockCause::Run;
    }
    BlockCause::Unclassified
}

/// A claimed leaf's failure by its settlement's evidence class, then its
/// typed failure class. The reason's text only tells apart the
/// infrastructure causes a typed provider or environment class leaves open.
fn claim_cause(settled: &SettledFailure) -> BlockCause {
    let refine = |allowed: &[&'static str], fallback: &'static str| {
        infra_cause(&settled.reason)
            .filter(|cause| allowed.contains(cause))
            .unwrap_or(fallback)
    };
    let provider = || {
        BlockCause::Infra(refine(
            &["auth", "sandbox", "provider_limit"],
            "provider_unavailable",
        ))
    };
    match ClaimSettlementEvidence::parse(&settled.evidence) {
        Some(ClaimSettlementEvidence::EvidenceHold) => return BlockCause::NotFailure,
        Some(ClaimSettlementEvidence::ProviderUnavailable) => return provider(),
        Some(ClaimSettlementEvidence::ForgeUnavailable) => {
            return BlockCause::Infra("forge_unavailable");
        }
        Some(ClaimSettlementEvidence::BaselineRed) => return BlockCause::Infra("baseline_red"),
        Some(ClaimSettlementEvidence::FinalRecovery) => return BlockCause::Run,
        Some(ClaimSettlementEvidence::Failure | ClaimSettlementEvidence::Summary) | None => {}
    }
    match settled.failure_class {
        Some(ClaimFailureClass::Candidate | ClaimFailureClass::TaskInput) => BlockCause::Run,
        Some(ClaimFailureClass::OperatorCancel) => BlockCause::NotFailure,
        Some(ClaimFailureClass::Provider) => provider(),
        Some(ClaimFailureClass::Environment) => {
            BlockCause::Infra(refine(&["auth", "sandbox", "disk"], "environment"))
        }
        Some(ClaimFailureClass::BaselineRed) => BlockCause::Infra("baseline_red"),
        Some(ClaimFailureClass::OwnerRoute) => BlockCause::Infra("owner_route"),
        Some(ClaimFailureClass::Transient) => BlockCause::Infra("transient"),
        Some(ClaimFailureClass::BaseConflict) => BlockCause::BaseConflict(Vec::new()),
        // An untyped settlement: the leaf's reason as a run failure's note
        // would read, when a leaf ran; one that never launched says nothing
        // about the work.
        None => match infra_cause(&settled.reason) {
            Some(cause) => BlockCause::Infra(cause),
            None if settled.leaf_run => BlockCause::Run,
            None => BlockCause::Unclassified,
        },
    }
}

/// `key<value>` in a `key=value, ` note, up to the next separator.
fn note_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let start = text.find(key)? + key.len();
    let value = text[start..].split([',', ' ', ';', '\n']).next()?;
    (!value.is_empty() && value != "-").then_some(value)
}

/// The paths a failure handoff recorded as conflicting: its last field,
/// `conflicts=<path>, <path>` or `conflicts=none reported`.
fn conflicting_paths(text: &str) -> Vec<Anchor> {
    let Some((_, paths)) = text.rsplit_once("conflicts=") else {
        return Vec::new();
    };
    let mut anchors: Vec<Anchor> = paths
        .lines()
        .next()
        .unwrap_or_default()
        .split(", ")
        .filter(|path| *path != "none reported")
        .filter_map(Anchor::parse)
        .collect();
    anchors.sort();
    anchors.dedup();
    anchors
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
