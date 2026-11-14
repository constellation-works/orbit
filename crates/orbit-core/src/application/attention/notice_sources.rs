//! Task-level adapters over the existing authoritative predicates.
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::attention::cursor;
use orbit_types::{
    record::{EffectiveStatus, Notice, NoticeKind},
    tool::{
        AttentionHost, AttentionItem, AttentionKind, AttentionNext, AttentionSeverity,
        AttentionTask, AttentionWorkspace,
    },
};
use serde_json::json;

use super::sources::{SharedOpenTasks, Subject, attention_reason, finalize_attention_item};

fn notice_attention_severity(notice: &Notice, now: DateTime<Utc>) -> AttentionSeverity {
    use AttentionSeverity as S;
    let base = match notice.kind {
        NoticeKind::Incident => S::High,
        NoticeKind::Maintenance => S::Medium,
        NoticeKind::Info => S::Low,
    };
    let linked_terminal_not_done = notice
        .linked_task
        .as_ref()
        .is_some_and(|linked| matches!(linked.status.as_deref(), Some("rejected" | "archived")));
    let expiring_soon = (notice.expires_at - now).num_seconds() <= 2 * 3600;
    if linked_terminal_not_done || expiring_soon {
        match base {
            S::Low => S::Medium,
            S::Medium => S::High,
            S::High | S::Critical => S::Critical,
        }
    } else {
        base
    }
}

/// Effective-open notices this owner derives for `workspace`: the notices
/// homed in the runtime's own store partition, plus every global notice.
///
/// The notice record carries the store partition id, which can differ from
/// `workspace.id` (the registry selector), so home is the runtime's
/// partition rather than a comparison with the selector. A global notice is
/// listed by every workspace read, so its item id is independent of the
/// reading workspace and `attention_query` keeps one copy per read.
pub(super) fn notice_attention_items(
    runtime: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    now: DateTime<Utc>,
) -> Result<Vec<AttentionItem>, OrbitError> {
    use crate::application::notice::{NoticeListFilter, NoticeStatusFilter};
    let listing = runtime.list_notices(&NoticeListFilter {
        status: NoticeStatusFilter::Open,
        include_global: true,
        all_workspaces: false,
        limit: None,
        ..NoticeListFilter::default()
    })?;
    let mut items = Vec::new();
    for notice in &listing.notices {
        if notice.status != EffectiveStatus::Open {
            continue;
        }
        let severity = notice_attention_severity(notice, now);
        let mut refs = json!({"notice_id": notice.id});
        let task = notice.linked_task.as_ref().and_then(|linked| {
            refs["task_id"] = json!(linked.id);
            runtime
                .notice_linked_task(&linked.id)
                .map(|task| AttentionTask {
                    status: task.status,
                    tags: task.tags.clone(),
                    crew: task.crew.clone(),
                    orchestrator: task.orchestrator.clone(),
                    title: task.title.clone(),
                })
        });
        let ws = workspace.id.replace('\'', "'\\''");
        let show = AttentionNext {
            summary: "Show the notice".into(),
            tool: Some("orbit.notice.show".into()),
            input: Some(json!({"id": notice.id, "workspace": workspace.id})),
            command: format!("orbit --workspace '{ws}' notice show {}", notice.id),
            needs: Vec::new(),
            also: vec![AttentionNext {
                summary: "Close the notice".into(),
                tool: Some("orbit.notice.close".into()),
                input: Some(json!({"id": notice.id, "workspace": workspace.id})),
                command: format!(
                    "orbit --workspace '{ws}' notice close {} --reason '<reason>'",
                    notice.id
                ),
                needs: vec!["reason".into()],
                also: Vec::new(),
            }],
        };
        let mut item = AttentionItem {
            id: format!(
                "att_notice_open_{}",
                cursor::attention_digest(
                    json!([
                        AttentionKind::NoticeOpen,
                        host.machine_id,
                        notice.workspace.id,
                        notice.id
                    ])
                    .to_string()
                    .as_bytes()
                )
            ),
            kind: AttentionKind::NoticeOpen,
            severity,
            workspace: workspace.clone(),
            host: host.clone(),
            refs,
            task,
            since: Some(notice.opened_at),
            age_seconds: Some((now - notice.opened_at).num_seconds().max(0) as u64),
            expires_at: None,
            reason: attention_reason(&notice.title),
            detail: json!({
                "notice_kind": notice.kind,
                "scope": notice.scope,
                "expires_at": notice.expires_at,
            }),
            next: show,
            fingerprint: String::new(),
        };
        item.fingerprint = cursor::attention_fingerprint(
            &item,
            json!({
                "revision": notice.revision,
                "status": notice.status,
                "severity": severity,
            }),
        );
        items.push(item);
    }
    Ok(items)
}

pub(super) fn friction_attention_items(
    runtime: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    now: DateTime<Utc>,
) -> Result<Vec<AttentionItem>, OrbitError> {
    use orbit_store::contracts::FrictionListFilter;
    use orbit_types::record::FrictionStatus;
    let store = crate::runtime::friction::store_for(runtime)?;
    let records = store.list(&FrictionListFilter {
        statuses: vec![FrictionStatus::Open],
        ..Default::default()
    })?;
    let mut items = Vec::new();
    for stored in records {
        let record = stored.record;
        if record.status != FrictionStatus::Open {
            continue;
        }
        let title = orbit_common::governance::friction::effective_title(
            record.title.as_deref(),
            &record.body,
            &record.id,
        );
        let ws = workspace.id.replace('\'', "'\\''");
        let next = AttentionNext {
            summary: "Triage the friction record".into(),
            tool: Some("orbit.friction".into()),
            input: Some(
                json!({"action": "update", "id": record.id, "workspace": workspace.id, "status": "triaged"}),
            ),
            command: format!(
                "orbit --workspace '{ws}' friction update {} --status triaged",
                record.id
            ),
            needs: Vec::new(),
            also: Vec::new(),
        };
        let age = (now - record.created_at).num_seconds().max(0) as u64;
        let bumps = super::ATTENTION_AGE_BUMP_HOURS
            .iter()
            .filter(|hours| age >= u64::from(**hours) * 3600)
            .count();
        let severity = if bumps >= 1 {
            AttentionSeverity::Medium
        } else {
            AttentionSeverity::Low
        };
        let mut item = AttentionItem {
            id: format!(
                "att_friction_untriaged_{}",
                cursor::attention_digest(
                    json!([
                        AttentionKind::FrictionUntriaged,
                        host.machine_id,
                        workspace.id,
                        record.id
                    ])
                    .to_string()
                    .as_bytes()
                )
            ),
            kind: AttentionKind::FrictionUntriaged,
            severity,
            workspace: workspace.clone(),
            host: host.clone(),
            refs: json!({"friction_id": record.id}),
            task: None,
            since: Some(record.created_at),
            age_seconds: Some(age),
            expires_at: None,
            reason: attention_reason(&title),
            detail: json!({"status": "open", "tags": record.tags}),
            next,
            fingerprint: String::new(),
        };
        item.fingerprint = cursor::attention_fingerprint(
            &item,
            json!({"friction_id": record.id, "status": "open"}),
        );
        items.push(item);
    }
    Ok(items)
}

/// `resident_stalled`: every active resident mandate that has not
/// checkpointed for twice its heartbeat, whose last three cycles were all
/// `incomplete`, or whose workspace cycle routine is disabled (no cycle
/// can fire at all; the next action is to enable it or stop the mandate).
/// Paused, taken-over, handed-off and ended mandates are meant
/// to wait and are never stalled.
pub(super) fn resident_stalled_items(
    runtime: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    now: DateTime<Utc>,
    open_tasks: &SharedOpenTasks,
) -> Result<Vec<AttentionItem>, OrbitError> {
    let cycle_routine_disabled = runtime.resident_cycle_routine_disabled();
    let mut items = Vec::new();
    for metadata in open_tasks.get(runtime)? {
        if metadata.status.is_terminal()
            || !metadata
                .tags
                .iter()
                .any(|tag| tag == orbit_types::workflow::RESIDENT_TAG)
        {
            continue;
        }
        let task = runtime.get_task(&metadata.id)?;
        let history = runtime.get_task_history(&task.id)?;
        let view = runtime.resident_view(task.clone(), &history)?;
        let Some(stall) =
            runtime.resident_stall_of(&view, now, cycle_routine_disabled.as_deref())?
        else {
            continue;
        };
        let subject = Subject {
            runtime,
            task: &task,
            workspace,
            host,
            now,
        };
        let mut show = subject.tool_next("orbit.resident.show", "Inspect the resident");
        show.input = Some(json!({"task_id": task.id, "workspace": workspace.id}));
        // With the cycle routine disabled, `wake` cannot work: no cycle
        // follows it. The next steps that can work are enabling the
        // routine or stopping the mandate.
        let mut stop = subject.tool_next("orbit.resident.control", "Stop the mandate");
        stop.input = Some(json!({
            "task_id": task.id,
            "workspace": workspace.id,
            "action": "stop",
        }));
        stop.needs = vec!["reason".into()];
        stop.command = format!(
            "orbit --workspace '{}' resident control {} stop --reason '<reason>'",
            workspace.id, task.id
        );
        let mut next = if let Some(routine_name) = &cycle_routine_disabled {
            let mut enable = subject.tool_next(
                "orbit.routine.control",
                "Enable the resident cycle routine or stop the mandate",
            );
            enable.input = Some(json!({
                "workspace": workspace.id,
                "action": "toggle",
                "name": routine_name,
                "expected_enabled": false,
                "enabled": true,
                "target": crate::application::resident::RESIDENT_CYCLE_ROUTINE_TARGET,
            }));
            enable.needs = Vec::new();
            enable.command = format!(
                "orbit tool run orbit.routine.control --input '{{\"workspace\":\"{}\",\"action\":\"toggle\",\"name\":\"{}\",\"expected_enabled\":false,\"enabled\":true,\"target\":\"{}\"}}'",
                workspace.id,
                routine_name,
                crate::application::resident::RESIDENT_CYCLE_ROUTINE_TARGET,
            );
            enable
        } else {
            let mut wake = subject.tool_next(
                "orbit.resident.control",
                "Ask the resident for a status checkpoint",
            );
            wake.input = Some(json!({
                "task_id": task.id,
                "workspace": workspace.id,
                "action": "wake",
                "status_request": true,
            }));
            wake.needs = vec!["reason".into()];
            wake.command = format!(
                "orbit --workspace '{}' resident control {} wake --status-request --reason '<reason>'",
                workspace.id, task.id
            );
            wake
        };
        next.also = if cycle_routine_disabled.is_some() {
            vec![stop, show]
        } else {
            vec![show]
        };
        let detail = json!({
            "stall": stall.reason,
            "last_checkpoint_at": stall.last_checkpoint_at,
            "cycle_routine_disabled": cycle_routine_disabled.is_some(),
            "cycle_routine_name": cycle_routine_disabled,
        });
        let item = subject.item(
            AttentionKind::ResidentStalled,
            stall.since,
            &format!("Resident stalled: {}", stall.reason),
            detail.clone(),
            next,
        );
        items.push(finalize_attention_item(item, detail, None, &subject, false));
    }
    Ok(items)
}
