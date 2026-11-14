//! Task-level adapters over the existing authoritative predicates.
use crate::{
    OrbitRuntime,
    application::task::{
        TriageCandidacy,
        pilot::{self, PilotAdmissionHold},
    },
};
use chrono::{DateTime, Utc};
use orbit_common::governance::attention::{cursor, filter::AttentionFilter};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::{
    task::{
        DecisionGate, DecisionRequest, DecisionRequestState, Task, TaskComment, TaskHistoryEntry,
        TaskStatus,
    },
    tool::{
        AttentionDegraded, AttentionHost, AttentionItem, AttentionKind, AttentionNext,
        AttentionSeverity, AttentionTask, AttentionWorkspace,
    },
    workflow::{REVIEW_GATE_ARTIFACT, ReviewAttemptState, ReviewCertificate},
};
use serde_json::{Value, json};
use std::{
    cell::OnceCell,
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(super) struct AttentionCandidates {
    pub items: Vec<AttentionItem>,
    pub degraded: Vec<AttentionDegraded>,
    pub timings: BTreeMap<String, u128>,
}

/// The one open-task metadata listing shared by every attention source in
/// a single `attention_query` for one workspace. Loaded lazily on first
/// use, so a query that selects no task-backed source pays no listing, and
/// every later source reuses the first source's result (or its error,
/// rendered by each source in its own degraded shape).
#[derive(Default)]
pub(super) struct SharedOpenTasks {
    cell: OnceCell<Result<Vec<Task>, String>>,
}

impl SharedOpenTasks {
    pub(super) fn get(&self, runtime: &OrbitRuntime) -> Result<&[Task], OrbitError> {
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
/// The task projection every attention item carries.
pub(super) fn attention_task(task: &Task) -> AttentionTask {
    AttentionTask {
        status: task.status,
        tags: task.tags.clone(),
        crew: task.crew.clone(),
        orchestrator: task.orchestrator.clone(),
        title: task.title.clone(),
    }
}
/// A degraded entry scoped to one workspace.
pub(super) fn workspace_degraded(
    workspace: &AttentionWorkspace,
    kind: &str,
    code: &str,
    message: impl Into<String>,
) -> AttentionDegraded {
    AttentionDegraded {
        scope: format!("workspace:{}", workspace.id),
        kind: kind.into(),
        code: code.into(),
        message: message.into(),
    }
}
pub(super) struct Subject<'a> {
    pub(super) runtime: &'a OrbitRuntime,
    pub(super) task: &'a Task,
    pub(super) workspace: &'a AttentionWorkspace,
    pub(super) host: &'a AttentionHost,
    pub(super) now: DateTime<Utc>,
}
impl Subject<'_> {
    pub(super) fn item(
        &self,
        kind: AttentionKind,
        since: DateTime<Utc>,
        reason: &str,
        detail: Value,
        next: AttentionNext,
    ) -> AttentionItem {
        self.item_for(kind, &json!(self.task.id), since, reason, detail, next)
    }
    /// Like [`Self::item`], with an explicit identity `subject` (a decision
    /// request id) in place of the task id.
    pub(super) fn item_for(
        &self,
        kind: AttentionKind,
        subject: &Value,
        since: DateTime<Utc>,
        reason: &str,
        detail: Value,
        next: AttentionNext,
    ) -> AttentionItem {
        AttentionItem {
            id: cursor::attention_item_id(kind, &self.host.machine_id, &self.workspace.id, subject),
            kind,
            severity: AttentionSeverity::Medium,
            workspace: self.workspace.clone(),
            host: self.host.clone(),
            refs: json!({"task_id": self.task.id}),
            task: Some(attention_task(self.task)),
            since: Some(since),
            age_seconds: Some((self.now - since).num_seconds().max(0) as u64),
            expires_at: None,
            reason: attention_reason(reason),
            detail,
            next,
            fingerprint: String::new(),
        }
    }
    /// A next action naming a standalone tool (not a family operation).
    /// `tool` must be a callable tool name; family operations go through
    /// [`Self::family_tool_next`] so the stored `tool` is the family a
    /// caller invokes.
    pub(super) fn tool_next(&self, tool: &str, summary: &str) -> AttentionNext {
        let input = json!({"id": self.task.id, "workspace": self.workspace.id});
        AttentionNext {
            summary: summary.into(),
            tool: Some(tool.into()),
            input: Some(input),
            command: format!(
                "orbit --workspace '{}' {} {}",
                self.workspace.id,
                tool.strip_prefix("orbit.")
                    .unwrap_or(tool)
                    .replace('.', " "),
                self.task.id
            ),
            needs: Vec::new(),
            also: Vec::new(),
        }
    }
    /// A next action naming one action of a tool family: `tool` is the
    /// callable family, `input` carries its `action`, and `command` keeps
    /// the CLI verb form (`cli_verb`, such as `task requeue`). Arguments
    /// needing operator judgment stay out of `input`, are named in
    /// `needs`, and appear in `command` through `extra`.
    pub(super) fn family_tool_next(
        &self,
        family: &str,
        action: &str,
        cli_verb: &str,
        summary: &str,
        needs: &[&str],
        extra: &str,
    ) -> AttentionNext {
        AttentionNext {
            summary: summary.into(),
            tool: Some(family.into()),
            input: Some(
                json!({"action": action, "id": self.task.id, "workspace": self.workspace.id}),
            ),
            command: format!(
                "orbit --workspace '{}' {} {}{extra}",
                self.workspace.id, cli_verb, self.task.id
            ),
            needs: needs.iter().map(|need| (*need).to_string()).collect(),
            also: Vec::new(),
        }
    }
    fn show_next(&self) -> AttentionNext {
        self.tool_next("orbit.task.show", "Inspect the task")
    }
}
pub(super) fn attention_reason(source: &str) -> String {
    let line = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= 240 {
        line
    } else {
        line.chars().take(239).chain(['…']).collect()
    }
}
fn hold_description(hold: &PilotAdmissionHold) -> (&'static str, Option<String>) {
    match hold {
        PilotAdmissionHold::StaleAssessment(reason) => {
            ("pilot_assessment_stale", Some(reason.clone()))
        }
        PilotAdmissionHold::Duplicate => ("pilot_duplicate", None),
        PilotAdmissionHold::AlreadyLanded => ("pilot_already_landed", None),
        PilotAdmissionHold::OperatorValidation(hold) => {
            ("operator_validation", Some(hold.detail()))
        }
        PilotAdmissionHold::HostOperational(hold) => ("host_operational", Some(hold.detail())),
        PilotAdmissionHold::NativeOs(_) => ("native_os", None),
    }
}
fn decision_answers(subject: &Subject<'_>, request: &DecisionRequest) -> Vec<AttentionNext> {
    let mut answers = request.answers.iter().collect::<Vec<_>>();
    answers.sort_by_key(|answer| request.recommended.as_deref() != Some(answer.id.as_str()));
    answers
        .into_iter()
        .map(|answer| AttentionNext {
            summary: answer.label.clone(),
            tool: None,
            input: None,
            command: format!(
                "orbit --workspace '{}' decision answer {} --answer '{}'",
                subject.workspace.id,
                request.id,
                answer.id.replace('\'', "'\\''")
            ),
            needs: Vec::new(),
            also: Vec::new(),
        })
        .collect()
}
pub(super) fn finalize_attention_item(
    mut item: AttentionItem,
    mut state: Value,
    request: Option<&DecisionRequest>,
    subject: &Subject<'_>,
    high: bool,
) -> AttentionItem {
    use AttentionSeverity as S;
    let base = match item.kind {
        AttentionKind::Blocked => S::High,
        AttentionKind::ReviewWaiting
            if item.detail["waiting_on"] == "approval"
                || item.detail["waiting_on"] == "completion_hold" =>
        {
            S::Low
        }
        _ => S::Medium,
    };
    let bumps = super::ATTENTION_AGE_BUMP_HOURS
        .iter()
        .filter(|hours| {
            item.age_seconds
                .is_some_and(|age| age >= u64::from(**hours) * 3600)
        })
        .count();
    item.severity = if high || base == S::High || bumps >= 2 || (base == S::Medium && bumps >= 1) {
        S::High
    } else if base == S::Low && bumps == 1 {
        S::Medium
    } else {
        base
    };
    if let Some(request) = request {
        item.refs["decision_id"] = json!(request.id);
        item.expires_at = Some(request.expires_at);
        state["decision_id"] = json!(request.id);
        let mut answers = decision_answers(subject, request);
        if item.kind == AttentionKind::DecisionOpen {
            let mut next = subject.show_next();
            next.summary = "Read the decision options".into();
            next.tool = Some("orbit.decision".into());
            next.input = Some(
                json!({"action": "show", "id": request.id, "workspace": subject.workspace.id}),
            );
            next.command = format!(
                "orbit --workspace '{}' decision show {}",
                subject.workspace.id, request.id
            );
            next.also = answers;
            item.next = next;
            if (request.expires_at - subject.now).num_seconds() <= 6 * 3600
                || matches!(
                    request.risk_tier,
                    orbit_types::task::DecisionRiskTier::Irreversible
                        | orbit_types::task::DecisionRiskTier::Authority
                        | orbit_types::task::DecisionRiskTier::Spend
                )
            {
                item.severity = S::High;
            }
        } else if !answers.is_empty() {
            let mut next = answers.remove(0);
            next.also = answers;
            next.also.push(subject.show_next());
            item.next = next;
        }
    }
    item.fingerprint = cursor::attention_fingerprint(&item, state);
    item
}
fn review_hold_attention_item(
    subject: &Subject<'_>,
    kind: AttentionKind,
    hold: &crate::application::job::ReviewHold,
) -> AttentionItem {
    // `next_tool` names the serving operation; store its family call.
    let mut hold_next = AttentionNext {
        summary: hold.next_summary.clone(),
        tool: hold.next_tool().map(str::to_owned),
        input: hold
            .next_tool()
            .map(|_| json!({"id": hold.run_id, "workspace": subject.workspace.id})),
        command: hold.next_command(&subject.workspace.id, &subject.task.id),
        needs: vec![],
        also: vec![subject.show_next()],
    };
    hold_next.name_tool_family();
    let mut item = subject.item(
        kind,
        hold.since,
        &hold.reason,
        json!({
            "waiting_on": hold.kind.as_str(),
            "code": hold.code,
            "rule": hold.rule,
            "field": hold.field,
            "pattern": hold.pattern,
            "matched": hold.matched,
            "pr_number": hold.pr_number,
            "run_id": hold.run_id,
        }),
        hold_next,
    );
    item.refs["run_id"] = json!(hold.run_id);
    if let Some(number) = &hold.pr_number {
        item.refs["pr_number"] = json!(number);
    }
    item
}
fn derive_subject_kind(
    subject: &Subject<'_>,
    kind: AttentionKind,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
    request: Option<&DecisionRequest>,
    tasks: &[Task],
) -> Result<Option<AttentionItem>, OrbitError> {
    let task = subject.task;
    let runtime = subject.runtime;
    let (item, state, high) = match kind {
        AttentionKind::PilotWarning => {
            let Some(mut held) =
                pilot::held_pilot_warnings(task.status, comments, &task.dependencies(), &|id| {
                    runtime.warning_prerequisite_is_done(id)
                })
            else {
                return Ok(None);
            };
            held.findings
                .sort_by(|a, b| (&a.kind, &a.text).cmp(&(&b.kind, &b.text)));
            let since = comments
                .iter()
                .rev()
                .find(|comment| {
                    comment
                        .receipt()
                        .is_some_and(|receipt| receipt.operation_id == held.operation_id)
                })
                .map_or(task.updated_at, |comment| comment.at);
            let mut next = subject.family_tool_next(
                "orbit.task.recover",
                "repilot",
                "task repilot",
                "Re-pilot after fixing the cited warning",
                &[],
                "",
            );
            next.also.push(subject.show_next());
            // Keep the display bounded without truncating fingerprint material.
            let findings = held.findings.iter().take(32).map(|finding| json!({"kind": finding.kind, "text": attention_reason(&finding.text)})).collect::<Vec<_>>();
            let item = subject.item(
                kind,
                since,
                &held.findings[0].text,
                json!({"findings": findings, "findings_truncated": held.findings.len() > findings.len()}),
                next,
            );
            let state = json!({"operation_id": held.operation_id, "findings": held.findings});
            let high = held
                .findings
                .iter()
                .any(|finding| finding.kind == "contract_conflicts");
            (item, state, high)
        }
        AttentionKind::PilotHold => {
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                return Ok(None);
            }
            let Some(hold) =
                runtime.pilot_admission_hold_in(task, comments, &|| Ok(history.to_vec()))?
            else {
                return Ok(None);
            };
            let (classification, reason) = hold_description(&hold);
            let receipt = comments
                .iter()
                .rev()
                .find(|comment| comment.by == "task-pilot" && comment.receipt().is_some());
            let operation_id = receipt.and_then(|comment| {
                comment
                    .receipt()
                    .map(|receipt| receipt.operation_id.to_string())
            });
            let entry = history.iter().rev().find(|entry| {
                operation_id.as_deref().is_some_and(|operation| {
                    entry.operation_id.as_deref() == Some(operation)
                        || entry
                            .note
                            .as_deref()
                            .and_then(|note| serde_json::from_str::<Value>(note).ok())
                            .is_some_and(|note| note["operation_id"].as_str() == Some(operation))
                })
            });
            let since = entry
                .map(|entry| entry.at)
                .or_else(|| receipt.map(|comment| comment.at))
                .or_else(|| request.map(|request| request.raised_at))
                .unwrap_or(task.updated_at);
            let assessment_reason = receipt
                .and_then(|comment| comment.receipt())
                .and_then(|receipt| serde_json::from_str::<Value>(receipt.body).ok())
                .and_then(|audit| {
                    audit["assessment"]["assessment_rationale"]
                        .as_str()
                        .map(str::to_owned)
                });
            let source = reason
                .or(assessment_reason)
                .or_else(|| entry.and_then(|entry| entry.note.clone()))
                .or_else(|| receipt.map(|comment| comment.message.clone()))
                .unwrap_or_else(|| task.title.clone());
            let next = if matches!(hold, PilotAdmissionHold::StaleAssessment(_)) {
                subject.family_tool_next(
                    "orbit.task.recover",
                    "repilot",
                    "task repilot",
                    "Re-pilot the changed requirements",
                    &[],
                    "",
                )
            } else {
                subject.show_next()
            };
            let state = json!({"hold": classification, "operation_id": operation_id.or_else(|| entry.and_then(|entry| entry.operation_id.clone())), "decision_id": null});
            (
                subject.item(kind, since, &source, json!({"hold": classification}), next),
                state,
                tasks
                    .iter()
                    .any(|dependent| dependent.dependencies().contains(&task.id)),
            )
        }
        AttentionKind::Blocked => {
            if task.status != TaskStatus::Blocked {
                return Ok(None);
            }
            let artifacts = runtime.get_task_artifact_manifest(&task.id)?;
            let TriageCandidacy::Qualifies(episode) = crate::application::task::triage_candidacy(
                task,
                history,
                comments,
                &artifacts,
                subject.now,
            ) else {
                return Ok(None);
            };
            let mut next = subject.family_tool_next(
                "orbit.task.recover",
                "requeue",
                "task requeue",
                "Requeue after resolving the block",
                &["reason"],
                " --reason '<reason>'",
            );
            next.also.push(subject.show_next());
            // Put the leaf error before run metadata so the bounded reason
            // shows what the operator needs to resolve.
            let prerequisite = orbit_store::contracts::ClaimFailedNote::parse(&episode.block_note)
                .filter(|note| {
                    note.failure_class.is_some_and(
                        orbit_types::workflow::ClaimFailureClass::requires_operator_decision,
                    )
                });
            let source = prerequisite
                .as_ref()
                .map(|note| note.reason.as_str())
                .or(episode.escalation.as_deref())
                .unwrap_or(&episode.block_note);
            let mut item = subject.item(
                kind,
                episode.blocked_at,
                source,
                json!({"basis": episode.basis}),
                next,
            );
            if let Some(run) = &episode.run_id {
                item.refs["run_id"] = json!(run);
            }
            let escalation_at = comments
                .iter()
                .rev()
                .find(|comment| Some(&comment.message) == episode.escalation.as_ref())
                .map(|comment| comment.at);
            (
                item,
                json!({"blocked_at": episode.blocked_at, "basis": episode.basis, "escalation_at": escalation_at, "decision_id": null}),
                true,
            )
        }
        AttentionKind::ReviewWaiting => {
            if task.status != TaskStatus::Review {
                return Ok(None);
            }
            if let Some(hold) = runtime.current_review_hold(task, history)? {
                let state = json!({
                    "waiting_on": hold.kind.as_str(),
                    "run_id": hold.run_id,
                    "code": hold.code,
                    "rule": hold.rule,
                    "matched": hold.matched,
                });
                (
                    review_hold_attention_item(subject, kind, &hold),
                    state,
                    false,
                )
            } else {
                let Some(artifact) = runtime.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)?
                else {
                    // No certificate yet: the review has not settled (not run, still
                    // running, or in owner landing). A status or approval request alone
                    // cannot prove settlement, so this is not waiting and not a read failure.
                    return Ok(None);
                };
                let certificate: ReviewCertificate = serde_json::from_slice(&artifact.content)
                    .map_err(|error| {
                        OrbitError::Execution(format!("decode review certificate: {error}"))
                    })?;
                let Some(ledger) = runtime
                    .review_store()?
                    .review_ledger(&runtime.workspace_id()?, &certificate.lineage_key)?
                else {
                    return Err(OrbitError::Execution(
                        "review certificate has no lineage ledger; settlement cannot be verified"
                            .into(),
                    ));
                };
                if ledger.attempts.iter().any(|attempt| {
                    matches!(attempt.state, ReviewAttemptState::Open)
                        || attempt.reviewer_running.is_some()
                }) {
                    return Ok(None);
                }
                let Some(latest) = ledger.latest_attempt() else {
                    return Ok(None);
                };
                if latest.attempt_id != certificate.attempt_id
                    || !matches!(latest.state, ReviewAttemptState::Settled { .. })
                {
                    return Ok(None);
                }
                let waiting_on = if !certificate.verdict.passed()
                    && ledger.remaining_at(subject.now).seconds == 0
                {
                    "review_budget"
                } else if certificate.verdict.passed() {
                    "approval"
                } else {
                    return Ok(None);
                };
                let next = if waiting_on == "approval" {
                    AttentionNext {
                        summary: "Approve the settled review".into(),
                        tool: None,
                        input: None,
                        command: format!(
                            "orbit --workspace '{}' task update {} --approve",
                            subject.workspace.id, task.id
                        ),
                        needs: vec![],
                        also: vec![subject.show_next()],
                    }
                } else {
                    AttentionNext {
                        summary: "Reset the exhausted review budget".into(),
                        tool: Some("orbit.task.recover".into()),
                        input: Some(
                            json!({"action": "review_reset", "id": task.id, "workspace": subject.workspace.id}),
                        ),
                        command: format!(
                            "orbit --workspace '{}' task review-reset {} --lineage '<lineage>' --reason '<reason>'",
                            subject.workspace.id, task.id
                        ),
                        needs: vec!["lineage_key".into(), "reason".into()],
                        also: vec![subject.show_next()],
                    }
                };
                let reason = certificate
                    .escalation
                    .as_deref()
                    .unwrap_or_else(|| certificate.verdict.as_str());
                (subject.item(kind, certificate.issued_at, reason, json!({"waiting_on": waiting_on, "lineage": certificate.lineage_key, "review_id": certificate.attempt_id}), next), json!({"waiting_on": waiting_on, "review_id": certificate.attempt_id}), false)
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(finalize_attention_item(
        item, state, request, subject, high,
    )))
}

/// The non-repairing claim inspection's pending-commit refusal, when one
/// is pending. Task hydration reads (`get_task`, comments, history,
/// artifacts) settle a pending commit as a side effect, so an
/// observational attention read must degrade rather than reach them while
/// a commit awaits an ordinary operational read.
pub(super) fn pending_commit_message(runtime: &OrbitRuntime) -> Option<String> {
    if !runtime.coordination_task_reads_visible() {
        return None;
    }
    match runtime.inspect_execution_claims() {
        Err(error) if error.to_string().contains("pending commit") => Some(error.to_string()),
        _ => None,
    }
}

pub(super) fn read_attention_sources(
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
        result.degraded.push(workspace_degraded(
            workspace,
            kind.as_str(),
            "source_unavailable",
            "This source belongs to a later attention slice and is not implemented on this host",
        ));
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
                || AttentionKind::TASK_LEVEL_NAMES.contains(&kind.as_str())
        }) {
            result.degraded.push(workspace_degraded(
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
                Err(error) => result.degraded.push(workspace_degraded(
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
                Err(error) => result.degraded.push(workspace_degraded(
                    workspace,
                    K::NoticeOpen.as_str(),
                    "source_unavailable",
                    error.to_string(),
                )),
            }
        } else if !filter.candidate_read {
            result.degraded.push(workspace_degraded(
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
            Err(message) => result.degraded.push(workspace_degraded(
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
                Err(error) => result.degraded.push(workspace_degraded(
                    workspace,
                    K::FrictionUntriaged.as_str(),
                    "source_unavailable",
                    error.to_string(),
                )),
            }
        } else if !filter.candidate_read {
            result.degraded.push(workspace_degraded(
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
                result.degraded.push(workspace_degraded(
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
                    result.degraded.push(workspace_degraded(
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
            if *spent >= Duration::from_secs(super::ATTENTION_SOURCE_TIMEOUT_SECONDS) {
                if !result
                    .degraded
                    .iter()
                    .any(|entry| entry.kind == kind.as_str() && entry.code == "source_timeout")
                {
                    result.degraded.push(workspace_degraded(
                        workspace,
                        kind.as_str(),
                        "source_timeout",
                        "Source read budget exhausted",
                    ));
                }
                continue;
            }
            let started = Instant::now();
            match derive_subject_kind(&subject, kind, &comments, &history, request, tasks) {
                Ok(Some(item)) => {
                    if let Some(request) = request {
                        folded.push(request.id.clone());
                    }
                    if filter.selected(kind) {
                        result.items.push(item);
                    }
                }
                Ok(None) => {}
                Err(error) => result.degraded.push(workspace_degraded(
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
                result.items.push(finalize_attention_item(
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

use super::notice_sources::{
    friction_attention_items, notice_attention_items, resident_stalled_items,
};
