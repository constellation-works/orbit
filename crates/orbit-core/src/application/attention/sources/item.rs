use orbit_common::governance::attention::cursor;
use orbit_types::{
    task::{DecisionRequest, Task},
    tool::{
        AttentionDegraded, AttentionItem, AttentionKind, AttentionNext, AttentionSeverity,
        AttentionTask, AttentionWorkspace,
    },
};
use serde_json::{Value, json};

use super::subject::{self, Subject};

/// The task projection every attention item carries.
pub(in super::super) fn attention_task(task: &Task) -> AttentionTask {
    AttentionTask {
        status: task.status,
        tags: task.tags.clone(),
        crew: task.crew.clone(),
        orchestrator: task.orchestrator.clone(),
        title: task.title.clone(),
    }
}
/// A degraded entry scoped to one workspace.
pub(in super::super) fn workspace_degraded(
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

pub(in super::super) fn attention_reason(source: &str) -> String {
    let line = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= 240 {
        line
    } else {
        line.chars().take(239).chain(['…']).collect()
    }
}

pub(in super::super) fn finalize_attention_item(
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
    let bumps = super::super::ATTENTION_AGE_BUMP_HOURS
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
        let mut answers = subject::decision_answers(subject, request);
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
pub(super) fn review_hold_attention_item(
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
