//! `drain_paused` and `drain_breaker_open` (standing drain phase 2,
//! ORB-25991). Both exist only while `workflow.standing_drain.enabled` is set:
//! with the keeper off (the default) this source reads nothing more and
//! reports nothing. Only the workspace owner reports them; a replica's drain
//! intent and drain runs are not the workspace's.

use super::sources::attention_reason;
use crate::OrbitRuntime;
use chrono::{DateTime, TimeDelta, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::attention::cursor;
use orbit_store::contracts::DrainIntentState;
use orbit_types::tool::{
    AttentionHost, AttentionItem, AttentionKind, AttentionNext, AttentionSeverity,
    AttentionWorkspace,
};
use serde_json::{Value, json};

/// How long a pause must last, with work waiting, before it is reported.
const PAUSED_ATTENTION_AFTER: TimeDelta = TimeDelta::hours(12);

pub(super) fn drain_keeper_items(
    runtime: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    kinds: (bool, bool),
    now: DateTime<Utc>,
) -> Result<Vec<AttentionItem>, OrbitError> {
    let (want_paused, want_breaker) = kinds;
    let settings = runtime.current_standing_drain();
    if !settings.enabled || runtime.coordination_write_owner().is_some() {
        return Ok(Vec::new());
    }
    let Some(intent) = runtime.workspace_drain_intent()? else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    match intent.intent {
        DrainIntentState::Paused => {
            if want_paused
                && now.signed_duration_since(intent.updated_at) > PAUSED_ATTENTION_AFTER
                && runtime.standing_drain_work_waiting()?
            {
                let reason = intent
                    .reason
                    .as_deref()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default();
                items.push(keeper_item(
                    workspace,
                    host,
                    now,
                    KeeperItem {
                        kind: AttentionKind::DrainPaused,
                        severity: AttentionSeverity::Medium,
                        since: intent.updated_at,
                        reason: format!(
                            "Drain paused by {} from {} for over 12 hours while work waits{reason}",
                            intent.actor, intent.source
                        ),
                        detail: json!({
                            "paused_by": intent.actor,
                            "source": intent.source,
                            "reason": intent.reason,
                        }),
                        summary: "Start a drain to lift the pause, or leave it paused",
                    },
                ));
            }
        }
        DrainIntentState::On => {
            if want_breaker {
                let breaker =
                    runtime.standing_drain_keeper_breaker(&settings, Some(&intent), now)?;
                if breaker.open() {
                    items.push(keeper_item(
                        workspace,
                        host,
                        now,
                        KeeperItem {
                            kind: AttentionKind::DrainBreakerOpen,
                            severity: AttentionSeverity::High,
                            since: breaker.last_failed_at.unwrap_or(now),
                            reason: format!(
                                "Standing-drain keeper stopped after {} of its drains failed or \
                                 were interrupted within an hour",
                                breaker.failed
                            ),
                            detail: json!({
                                "failed_in_last_hour": breaker.failed,
                                "threshold": breaker.threshold,
                            }),
                            summary: "Find why the drains failed, then start a drain by hand to close the breaker",
                        },
                    ));
                }
            }
        }
    }
    Ok(items)
}

struct KeeperItem {
    kind: AttentionKind,
    severity: AttentionSeverity,
    since: DateTime<Utc>,
    reason: String,
    detail: Value,
    summary: &'static str,
}

fn keeper_item(
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    now: DateTime<Utc>,
    keeper: KeeperItem,
) -> AttentionItem {
    let next = AttentionNext {
        summary: keeper.summary.into(),
        tool: None,
        input: None,
        command: "orbit run auto --for 24h".into(),
        needs: Vec::new(),
        also: vec![AttentionNext {
            summary: "Show the drain intent and the keeper's last decision".into(),
            tool: None,
            input: None,
            command: "orbit run readiness".into(),
            needs: Vec::new(),
            also: Vec::new(),
        }],
    };
    let mut item = AttentionItem {
        id: cursor::attention_item_id(
            keeper.kind,
            &host.machine_id,
            &workspace.id,
            &json!("standing_drain"),
        ),
        kind: keeper.kind,
        severity: keeper.severity,
        workspace: workspace.clone(),
        host: host.clone(),
        refs: json!({}),
        task: None,
        since: Some(keeper.since),
        age_seconds: Some((now - keeper.since).num_seconds().max(0) as u64),
        expires_at: None,
        reason: attention_reason(&keeper.reason),
        detail: keeper.detail,
        next,
        fingerprint: String::new(),
    };
    item.fingerprint = cursor::attention_fingerprint(&item, json!({"since": keeper.since}));
    item
}
