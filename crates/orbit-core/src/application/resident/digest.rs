//! Read-only manager view; task comments and ledger occurrences stay canonical.
use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::ResidentComment;
use serde_json::{Value, json};

use super::ResidentMandateState;
use super::due::checkpoints;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Assemble resident state, escalations, vetoable decisions, window costs,
    /// proposed evolution tasks and the metrics headline in this workspace.
    pub fn resident_digest(&self, since: Option<&str>, top: usize) -> Result<Value, OrbitError> {
        if !(1..=100).contains(&top) {
            return Err(OrbitError::InvalidInput(
                "top must be between 1 and 100".into(),
            ));
        }
        let default_window = format!("{}d", crate::application::evolution::EVOLUTION_WINDOW_DAYS);
        let since = since.unwrap_or(&default_window);
        let now = Utc::now();
        let start = super::metrics::parse_window_start(Some(since), now)?;
        let mut residents = Vec::new();
        let mut escalations = Vec::new();
        let mut escalation_ids = BTreeSet::new();
        let mut decisions = Vec::new();
        let mandates = self.list_resident_mandates(false)?;
        for row in mandates["mandates"].as_array().into_iter().flatten() {
            let Some(id) = row["task_id"].as_str() else {
                continue;
            };
            let mut resident = self.show_resident_mandate(id)?;
            let comments = self.get_task_comments(id)?;
            let history = self.get_task_history(id)?;
            let task = self.get_task(id)?;
            let view = self.resident_view(task, &history)?;
            let checkpoints = checkpoints(id, &comments);
            let reversed: BTreeSet<String> = comments
                .iter()
                .filter_map(
                    |comment| match ResidentComment::parse(&comment.message).ok()? {
                        ResidentComment::Answer(answer) => answer.reverses,
                        _ => None,
                    },
                )
                .collect();
            let requests = if let Ok(mandate) = &view.mandate {
                self.resident_open_requests(id, mandate, checkpoints.last(), &comments)?
            } else {
                Vec::new()
            };
            let mut open = Vec::new();
            for mut request in requests
                .into_iter()
                .filter(|request| request["state"] == "open")
            {
                if !request["raised_at"].is_null() {
                    request["created_at"] = request["raised_at"].clone();
                }
                if request["created_at"].is_null() {
                    let at = comments.iter().find_map(|comment| {
                        match ResidentComment::parse(&comment.message).ok()? {
                            ResidentComment::Request(record)
                                if record.id
                                    == request["request_id"].as_str().unwrap_or_default() =>
                            {
                                Some(comment.at)
                            }
                            _ => None,
                        }
                    });
                    request["created_at"] = json!(at);
                }
                request["mandate"] = json!(id);
                if let Some(request_id) = request["id"].as_str()
                    && escalation_ids.insert(request_id.to_string())
                {
                    escalations.push(request.clone());
                }
                open.push(request);
            }
            // Include authority requests even for an invalid mandate.
            for mut request in view
                .open_requests
                .iter()
                .map(crate::application::task::decision_request_json)
            {
                request["mandate"] = json!(id);
                request["created_at"] = request["raised_at"].clone();
                if let Some(request_id) = request["id"].as_str()
                    && escalation_ids.insert(request_id.to_string())
                {
                    escalations.push(request.clone());
                    open.push(request);
                }
            }
            resident["last_outcome"] = json!(
                checkpoints
                    .last()
                    .map(|checkpoint| checkpoint.outcome.as_str())
            );
            resident["open_request_count"] = json!(open.len());
            resident["open_requests"] = json!(open);
            let stalled = self.resident_stall_of(&view, now, None)?.is_some();
            resident["stalled"] = json!(stalled);
            // An ended mandate is no longer a resident and cannot take a veto.
            // Its open escalations above stay: a person may still answer them.
            if view.state().0 == ResidentMandateState::Ended {
                continue;
            }
            residents.push(resident);
            for comment in &comments {
                if comment.at < start {
                    continue;
                }
                let Ok(ResidentComment::Checkpoint(checkpoint)) =
                    ResidentComment::parse(&comment.message)
                else {
                    continue;
                };
                if checkpoint.mandate != id {
                    continue;
                }
                for decision in checkpoint
                    .decisions
                    .iter()
                    .filter(|decision| decision.reversible && !reversed.contains(&decision.id))
                {
                    decisions.push(json!({"mandate": id, "cycle": checkpoint.cycle, "at": comment.at, "decision": decision.id, "summary": decision.summary, "class": decision.class}));
                }
            }
        }
        escalations.sort_by(|a, b| {
            let raised_at = |request: &Value| {
                request["created_at"]
                    .as_str()
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            };
            raised_at(a)
                .cmp(&raised_at(b))
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });
        let records = self.list_evolution_records(None, Some(start))?;
        let pains: Vec<Value> = records["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|record| record["state"] != "resolved")
            .take(top)
            .cloned()
            .map(|mut record| {
                record["summed_cost_minutes"] = json!(
                    record["minutes"]
                        .as_u64()
                        .unwrap_or_default()
                        .saturating_add(record["slot_minutes"].as_u64().unwrap_or_default())
                );
                record
            })
            .collect();
        let all_records = self.list_evolution_records(None, None)?;
        let linked: BTreeSet<&str> = all_records["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|record| record["state"] == "proposed")
            .filter_map(|record| record["proposal_task"].as_str())
            .collect();
        let proposals: Vec<Value> = self
            .list_task_metadata()?
            .into_iter()
            .filter(|task| {
                task.status == TaskStatus::Proposed
                    && (linked.contains(task.id.as_str())
                        || task
                            .tags
                            .iter()
                            .any(|tag| tag.starts_with("evolution-record:")))
            })
            .map(|task| json!({"task_id": task.id, "title": task.title, "status": task.status}))
            .collect();
        let metrics = self.resident_metrics(None, Some(since), None)?;
        Ok(
            json!({"workspace": self.workspace_id()?, "since": start, "residents": residents, "open_escalations": escalations, "decisions_awaiting_veto": decisions, "top_pains": pains, "open_proposals": proposals, "metrics": metrics["totals"]}),
        )
    }
}
