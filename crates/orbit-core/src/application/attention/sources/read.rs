use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use orbit_common::{NotFoundKind, OrbitError, governance::attention::filter::AttentionFilter};
use orbit_types::{
    task::{DecisionGate, DecisionRequestState, Task, TaskStatus},
    tool::{AttentionHost, AttentionKind, AttentionWorkspace},
};
use serde_json::json;

use super::super::notice_sources::{
    friction_attention_items, notice_attention_items, resident_stalled_items,
};
use super::subject::{self, Subject};
use super::{AttentionCandidates, SharedOpenTasks, item};
use crate::OrbitRuntime;

impl SharedOpenTasks {
    pub(in super::super) fn get(&self, runtime: &OrbitRuntime) -> Result<&[Task], OrbitError> {
        match self.cell.get_or_init(|| {
            runtime
                .observe_open_task_metadata()
                .map_err(|e| e.to_string())
        }) {
            Ok(tasks) => Ok(tasks),
            Err(message) => Err(OrbitError::Store(message.clone())),
        }
    }
}

/// The non-repairing claim inspection's pending-commit refusal, when one
/// is pending. Task hydration reads (`get_task`, comments, history,
/// artifacts) settle a pending commit as a side effect, so an
/// observational attention read must degrade rather than reach them while
/// a commit awaits an ordinary operational read.
pub(in super::super) fn pending_commit_message(runtime: &OrbitRuntime) -> Option<String> {
    if !runtime.coordination_task_reads_visible() {
        return None;
    }
    match runtime.inspect_live_execution_claims() {
        Err(error) if error.to_string().contains("pending commit") => Some(error.to_string()),
        _ => None,
    }
}

pub(in super::super) fn read_attention_sources(
    runtime: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    filter: &AttentionFilter,
    now: DateTime<Utc>,
    open_tasks: &SharedOpenTasks,
) -> Result<AttentionCandidates, OrbitError> {
    use AttentionKind as K;
    let mut result = AttentionCandidates {
        items: vec![],
        degraded: vec![],
        timings: BTreeMap::new(),
    };
    // Kinds whose sources later slices own are accepted by name and reported
    // as unreadable, never as an empty all-clear. `host_unhealthy` is read by
    // the caller, beside these task-level sources.
    for kind in filter
        .kinds
        .iter()
        .filter(|kind| !AttentionKind::DEFAULT_NAMES.contains(&kind.as_str()))
    {
        result.degraded.push(item::workspace_degraded(
            workspace,
            kind.as_str(),
            "source_unavailable",
            "This source belongs to a later attention slice and is not implemented on this host",
        ));
    }
    // routine_tripped reads this host's scheduler store, never task records,
    // so a pending task commit does not hold it.
    if filter.selected(K::RoutineTripped) {
        let started = Instant::now();
        match super::super::routine_sources::routine_tripped_items(runtime, workspace, host, now) {
            Ok(items) => result.items.extend(items),
            Err(error) => result.degraded.push(item::workspace_degraded(
                workspace,
                K::RoutineTripped.as_str(),
                "source_unavailable",
                error.to_string(),
            )),
        }
        result
            .timings
            .insert("routine_tripped".into(), started.elapsed().as_micros());
    }
    // A pending task commit makes every task-backed source unreadable
    // without recovery: degrade them instead of letting selection or
    // hydration settle the journal. Notice, resident and task-level
    // sources all read task records, so they share this refusal.
    // Friction is the exception: its workspace-partitioned store reads no
    // task records, so it is read (or degraded on error) before returning
    // rather than silently omitted behind the pending guard.
    if let Some(message) = pending_commit_message(runtime) {
        for kind in filter.kinds.iter().copied().filter(|kind| {
            *kind == K::NoticeOpen
                || *kind == K::ResidentStalled
                || *kind == K::DrainPaused
                || *kind == K::DrainBreakerOpen
                || AttentionKind::TASK_LEVEL_NAMES.contains(&kind.as_str())
        }) {
            result.degraded.push(item::workspace_degraded(
                workspace,
                kind.as_str(),
                "source_unavailable",
                message.clone(),
            ));
        }
        if filter.selected(K::FrictionUntriaged) {
            let started = Instant::now();
            match friction_attention_items(runtime, workspace, host, now) {
                Ok(items) => result.items.extend(items),
                Err(error) => result.degraded.push(item::workspace_degraded(
                    workspace,
                    K::FrictionUntriaged.as_str(),
                    "source_unavailable",
                    error.to_string(),
                )),
            }
            result
                .timings
                .insert("friction_untriaged".into(), started.elapsed().as_micros());
        }
        return Ok(result);
    }
    // notice_open is derived only on the home owner: a replica's notice
    // store is not authoritative, so it degrades like the task kinds.
    if filter.selected(K::NoticeOpen) {
        let started = Instant::now();
        if runtime.coordination_task_reads_visible() {
            match notice_attention_items(runtime, workspace, host, now) {
                Ok(items) => result.items.extend(items),
                Err(error) => result.degraded.push(item::workspace_degraded(
                    workspace,
                    K::NoticeOpen.as_str(),
                    "source_unavailable",
                    error.to_string(),
                )),
            }
        } else if !filter.candidate_read {
            result.degraded.push(item::workspace_degraded(
                workspace,
                K::NoticeOpen.as_str(),
                "source_unavailable",
                "Notices on a replica are not authoritative; read the workspace owner",
            ));
        }
        result
            .timings
            .insert("notice_open".into(), started.elapsed().as_micros());
    }
    // resident_stalled reads the owner's task records, like the task kinds.
    if filter.selected(K::ResidentStalled) {
        let started = Instant::now();
        let read = if runtime.coordination_task_reads_visible() {
            resident_stalled_items(runtime, workspace, host, now, open_tasks)
                .map_err(|error| error.to_string())
        } else {
            Err("Task records on a replica are not authoritative; read the workspace owner".into())
        };
        match read {
            Ok(items) => result.items.extend(items),
            // The aggregator's candidate read asks the owner; a replica stays silent.
            Err(_) if filter.candidate_read && !runtime.coordination_task_reads_visible() => {}
            Err(message) => result.degraded.push(item::workspace_degraded(
                workspace,
                K::ResidentStalled.as_str(),
                "source_unavailable",
                message,
            )),
        }
        result
            .timings
            .insert("resident_stalled".into(), started.elapsed().as_micros());
    }
    // friction_untriaged is derived only on the home owner: a replica's
    // friction store is not authoritative, so it degrades like the task kinds.
    if filter.selected(K::FrictionUntriaged) {
        let started = Instant::now();
        if runtime.coordination_task_reads_visible() {
            match friction_attention_items(runtime, workspace, host, now) {
                Ok(items) => result.items.extend(items),
                Err(error) => result.degraded.push(item::workspace_degraded(
                    workspace,
                    K::FrictionUntriaged.as_str(),
                    "source_unavailable",
                    error.to_string(),
                )),
            }
        } else if !filter.candidate_read {
            result.degraded.push(item::workspace_degraded(
                workspace,
                K::FrictionUntriaged.as_str(),
                "source_unavailable",
                "Friction records on a replica are not authoritative; read the workspace owner",
            ));
        }
        result
            .timings
            .insert("friction_untriaged".into(), started.elapsed().as_micros());
    }
    // [ORB-25991] The standing-drain keeper's items exist only while it is
    // enabled, and only the owner reports them; off, nothing is read.
    let keeper_kinds = (
        filter.selected(K::DrainPaused),
        filter.selected(K::DrainBreakerOpen),
    );
    if keeper_kinds.0 || keeper_kinds.1 {
        let started = Instant::now();
        match super::super::drain_keeper_sources::drain_keeper_items(
            runtime,
            workspace,
            host,
            keeper_kinds,
            now,
        ) {
            Ok(items) => result.items.extend(items),
            Err(error) => {
                for (selected, kind) in [
                    (keeper_kinds.0, K::DrainPaused),
                    (keeper_kinds.1, K::DrainBreakerOpen),
                ] {
                    if selected {
                        result.degraded.push(item::workspace_degraded(
                            workspace,
                            kind.as_str(),
                            "source_unavailable",
                            error.to_string(),
                        ));
                    }
                }
            }
        }
        result
            .timings
            .insert("standing_drain".into(), started.elapsed().as_micros());
    }
    let task_kinds = filter
        .kinds
        .iter()
        .copied()
        .filter(|kind| AttentionKind::TASK_LEVEL_NAMES.contains(&kind.as_str()))
        .collect::<Vec<_>>();
    if task_kinds.is_empty() {
        return Ok(result);
    }
    if !runtime.coordination_task_reads_visible() {
        // A replica is not authoritative for task, decision or review
        // records. On the aggregator's candidate read the owner reports them,
        // so the replica stays silent; a direct read says where to look.
        if !filter.candidate_read {
            for kind in task_kinds {
                result.degraded.push(item::workspace_degraded(
                    workspace,
                    kind.as_str(),
                    "source_unavailable",
                    "Task records on a replica are not authoritative; read the workspace owner",
                ));
            }
        }
        return Ok(result);
    }
    let started = Instant::now();
    let tasks: &[Task] = open_tasks.get(runtime)?;
    result
        .timings
        .insert("task_index".into(), started.elapsed().as_micros());
    let wants_decisions = filter.selected(K::DecisionOpen);
    let mut elapsed = BTreeMap::<String, Duration>::new();
    for metadata in tasks {
        if metadata.status.is_terminal() {
            continue;
        }
        let candidate_kind = match metadata.status {
            TaskStatus::Proposed => vec![K::PilotWarning, K::PilotHold],
            TaskStatus::Backlog => vec![K::PilotHold],
            TaskStatus::Blocked => vec![K::Blocked],
            TaskStatus::Review => vec![K::ReviewWaiting],
            _ => vec![],
        };
        if !wants_decisions && !candidate_kind.iter().any(|kind| filter.selected(*kind)) {
            continue;
        }
        let selected = candidate_kind
            .into_iter()
            .filter(|kind| filter.selected(*kind) || (wants_decisions && *kind != K::PilotWarning))
            .collect::<Vec<_>>();
        let needs_decisions =
            wants_decisions || selected.iter().any(|kind| *kind != K::PilotWarning);
        let hydrated = (|| {
            // Revision-bound decisions hash description and acceptance criteria;
            // bodyless index metadata cannot establish an effective request.
            // One listed (settled, lightweight) bundle read per candidate task
            // supplies the full task, its comments and its history together,
            // replacing the former canonical `get_task` + pilot-comments +
            // history reads. A missing settled row degrades exactly as the
            // former `get_task` not-found did.
            let row = runtime
                .get_listed_task_row(&metadata.id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, metadata.id.clone()))?;
            let crate::application::task::TaskRow {
                task,
                comments: raw_comments,
                history: row_history,
                ..
            } = row;
            let comments = runtime.resolve_task_pilot_comments(&metadata.id, &raw_comments)?;
            let (history, requests) = if needs_decisions {
                let requests = runtime
                    .task_decision_requests_in(&task, &row_history, &comments, now)?
                    .into_iter()
                    .filter(|request| request.state == DecisionRequestState::Open)
                    .collect::<Vec<_>>();
                (row_history, requests)
            } else {
                (Vec::new(), Vec::new())
            };
            Ok::<_, OrbitError>((task, comments, history, requests))
        })();
        let (task, comments, history, requests) = match hydrated {
            Ok(records) => records,
            Err(error) => {
                for kind in selected
                    .iter()
                    .copied()
                    .chain(wants_decisions.then_some(K::DecisionOpen))
                {
                    result.degraded.push(item::workspace_degraded(
                        workspace,
                        kind.as_str(),
                        "source_unavailable",
                        format!("{}: {error}", metadata.id),
                    ));
                }
                continue;
            }
        };
        let subject = Subject {
            runtime,
            task: &task,
            workspace,
            host,
            now,
        };
        let mut folded = Vec::new();
        for kind in selected {
            let gate = match kind {
                K::PilotHold => Some(DecisionGate::PilotAdmission),
                K::Blocked => Some(DecisionGate::BlockTriage),
                K::ReviewWaiting => Some(DecisionGate::ReviewApproval),
                _ => None,
            };
            let request = requests.iter().find(|request| Some(request.gate) == gate);
            let spent = elapsed.entry(kind.as_str().into()).or_default();
            if *spent >= Duration::from_secs(super::super::ATTENTION_SOURCE_TIMEOUT_SECONDS) {
                if !result
                    .degraded
                    .iter()
                    .any(|entry| entry.kind == kind.as_str() && entry.code == "source_timeout")
                {
                    result.degraded.push(item::workspace_degraded(
                        workspace,
                        kind.as_str(),
                        "source_timeout",
                        "Source read budget exhausted",
                    ));
                }
                continue;
            }
            let started = Instant::now();
            match subject::derive_subject_kind(&subject, kind, &comments, &history, request, tasks)
            {
                Ok(Some(item)) => {
                    if let Some(request) = request {
                        folded.push(request.id.clone());
                    }
                    if filter.selected(kind) {
                        result.items.push(item);
                    }
                }
                Ok(None) => {}
                Err(error) => result.degraded.push(item::workspace_degraded(
                    workspace,
                    kind.as_str(),
                    "source_unavailable",
                    format!("{}: {error}", task.id),
                )),
            }
            *spent += started.elapsed();
        }
        if wants_decisions {
            let started = Instant::now();
            for request in requests
                .iter()
                .filter(|request| !folded.contains(&request.id))
            {
                let item = subject.item_for(
                    K::DecisionOpen,
                    &json!(request.id),
                    request.raised_at,
                    request.summary.as_deref().unwrap_or(&task.title),
                    json!({"gate": request.gate}),
                    subject.show_next(),
                );
                result.items.push(item::finalize_attention_item(
                    item,
                    json!({"request_id": request.id}),
                    Some(request),
                    &subject,
                    false,
                ));
            }
            *elapsed.entry("decision_open".into()).or_default() += started.elapsed();
        }
    }
    result.timings.extend(
        elapsed
            .into_iter()
            .map(|(kind, elapsed)| (kind, elapsed.as_micros())),
    );
    Ok(result)
}
