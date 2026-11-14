//! Operator review of proposed global notes. Similarity is display-only;
//! expiry, decisions and revisions are audited in the owner store.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{Duration, SubsecRound, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    CrewMemoryEventRow, CrewMemoryProposalDecision, CrewMemoryProposalRow,
};
use serde_json::{Value, json};

use super::operations::{self, WriteAction};
use super::{MemoryFile, MemoryKey, MemoryWrite};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// List open proposals with evidence and the number of other proposing
    /// keys whose open notes have token Jaccard overlap of at least 0.5.
    /// Similarity never changes status or selects a decision.
    pub fn crew_memory_proposals(&self) -> Result<Value, OrbitError> {
        let rows = open_proposal_views(&self.open_crew_memory_proposals()?)?;
        Ok(json!({ "count": rows.len(), "proposals": rows }))
    }

    /// The dashboard's read of the proposals, which writes nothing: the open
    /// ones as [`Self::crew_memory_proposals`] lists them, and the most
    /// recently closed ones with their decisions. An open proposal past its
    /// four weeks is shown closed as `expired` at its `expires_at`; a
    /// decision is closed at `decided_at`. Those close times are one list,
    /// newest first, with the proposal id descending when they tie, and only
    /// the twenty most recent remain. Only an operator's list or decision
    /// records a withdrawal. While `crew_memory.enabled` is false both lists
    /// are empty, as the operator CLI reads no proposals then.
    pub fn crew_memory_proposal_board(&self) -> Result<Value, OrbitError> {
        if !self.current_crew_memory_enabled() {
            return Ok(json!({
                "enabled": false,
                "enabled_key": operations::ENABLED_KEY,
                "open": [],
                "recently_closed": [],
            }));
        }
        let now = Utc::now();
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let (open, lapsed): (Vec<_>, Vec<_>) = store
            .crew_memory_open_proposals(&workspace_id)?
            .into_iter()
            .partition(|row| !proposal_lapsed(row, now));
        let mut closed = lapsed
            .iter()
            .map(|row| {
                let mut view = proposal_view(row)?;
                view["status"] = json!("expired");
                view["decided_by"] = Value::Null;
                view["decided_at"] = Value::Null;
                view["decision_reason"] = json!("expired after 4 weeks");
                Ok(ClosedBoardEntry {
                    closed_at: Some(row.created_at + Duration::weeks(PROPOSAL_OPEN_WEEKS)),
                    id: row.id.clone(),
                    view,
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        for row in store.crew_memory_closed_proposals(&workspace_id, RECENTLY_CLOSED_LIMIT)? {
            let mut view = proposal_view(&row.proposal)?;
            view["decided_by"] = json!(row.decided_by);
            view["decided_at"] = json!(row.decided_at);
            view["decision_reason"] = json!(row.decision_reason);
            closed.push(ClosedBoardEntry {
                closed_at: row.decided_at,
                id: row.proposal.id.clone(),
                view,
            });
        }
        closed.sort_by(recently_closed_order);
        closed.truncate(RECENTLY_CLOSED_LIMIT);
        let closed: Vec<Value> = closed.into_iter().map(|entry| entry.view).collect();
        Ok(json!({
            "enabled": true,
            "enabled_key": operations::ENABLED_KEY,
            "open": open_proposal_views(&open)?,
            "recently_closed": closed,
        }))
    }

    /// Open proposals still inside their four weeks, read without expiring
    /// the rest.
    pub(super) fn unexpired_crew_memory_proposals(
        &self,
    ) -> Result<Vec<CrewMemoryProposalRow>, OrbitError> {
        let now = Utc::now();
        Ok(self
            .crew_memory_store()?
            .crew_memory_open_proposals(&self.workspace_id()?)?
            .into_iter()
            .filter(|row| !proposal_lapsed(row, now))
            .collect())
    }

    /// Accept an open proposal, appending its note to the current global
    /// text, or replacing the whole global text with the contents of `edit`.
    /// Both forms pass the operator's global write rules before deciding.
    pub(crate) fn crew_memory_proposal_accept(
        &self,
        id: &str,
        edit: Option<&Path>,
    ) -> Result<Value, OrbitError> {
        let proposal = self
            .open_crew_memory_proposals()?
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| proposal_not_open(id))?;
        let key = MemoryKey::global();
        let source = self.crew_memory_store()?.crew_memory_revision(
            &self.workspace_id()?,
            key.as_str(),
            None,
        )?;
        let mut evidence = source
            .as_ref()
            .map(operations::crew_memory_revision_evidence)
            .transpose()?
            .unwrap_or_default();
        let proposed_evidence: Vec<String> = serde_json::from_str(&proposal.evidence_json)
            .map_err(|error| OrbitError::Store(format!("invalid proposal evidence: {error}")))?;
        evidence.extend(proposed_evidence);
        evidence.sort();
        evidence.dedup();
        let text = if let Some(path) = edit {
            std::fs::read_to_string(path).map_err(|error| {
                OrbitError::InvalidInput(format!("cannot read --edit {}: {error}", path.display()))
            })?
        } else {
            let files = source
                .as_ref()
                .map(operations::crew_memory_revision_files)
                .transpose()?
                .unwrap_or_default();
            let mut text = files
                .first()
                .map(|file| file.text.clone())
                .unwrap_or_default();
            if !text.is_empty() && !proposal.text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&proposal.text);
            text
        };
        let reason = format!(
            "accepted proposal {id}{}",
            if edit.is_some() {
                " with edited global text"
            } else {
                ""
            }
        );
        let mut view = self.write_crew_memory_revision(
            &key,
            MemoryWrite {
                files: vec![MemoryFile {
                    path: "global.md".to_string(),
                    text,
                }],
                evidence,
            },
            &self.operator_memory_writer(),
            WriteAction::Accept {
                proposal_id: id.to_string(),
                expected_revision: source.as_ref().map_or(0, |row| row.revision),
            },
            Some(reason),
            None,
        )?;
        view["proposal_id"] = json!(id);
        view["status"] = json!("accepted");
        Ok(view)
    }

    /// Reject an open proposal with a nonempty reason, preserving its text
    /// and evidence and atomically appending the decision's audit event.
    pub(crate) fn crew_memory_proposal_reject(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Value, OrbitError> {
        self.open_crew_memory_proposals()?
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| proposal_not_open(id))?;
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(OrbitError::InvalidInput(
                "proposal rejection requires a nonempty reason".to_string(),
            ));
        }
        let writer = self.operator_memory_writer();
        let now = Utc::now().trunc_subsecs(6);
        let workspace_id = self.workspace_id()?;
        self.crew_memory_store()?.decide_crew_memory_proposal(
            &CrewMemoryProposalDecision {
                workspace_id: workspace_id.clone(),
                id: id.to_string(),
                status: "rejected".to_string(),
                actor: writer.actor().to_string(),
                at: now,
                reason: reason.to_string(),
            },
            &CrewMemoryEventRow {
                workspace_id,
                memory_key: "global".to_string(),
                at: now,
                action: "proposal_rejected".to_string(),
                actor: writer.actor().to_string(),
                revision: None,
                detail_json: json!({ "proposal_id": id, "reason": reason }).to_string(),
            },
            None,
        )?;
        Ok(
            json!({ "id": id, "status": "rejected", "reason": reason, "decided_by": writer.actor(), "decided_at": now }),
        )
    }

    /// Expiration is lazy: every list or decision first withdraws proposals
    /// older than four weeks. Disabled workspaces refuse before any access.
    fn open_crew_memory_proposals(&self) -> Result<Vec<CrewMemoryProposalRow>, OrbitError> {
        self.ensure_crew_memory_writes_enabled("proposals", &MemoryKey::global())?;
        self.ensure_coordination_task_write_permitted()?;
        let now = Utc::now().trunc_subsecs(6);
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        store.expire_crew_memory_proposals(
            &workspace_id,
            now - Duration::weeks(PROPOSAL_OPEN_WEEKS),
            now,
        )?;
        store.crew_memory_open_proposals(&workspace_id)
    }
}

/// How many closed proposals the dashboard's read returns after merging
/// virtual expiries and decisions by effective close time.
const RECENTLY_CLOSED_LIMIT: usize = 20;

/// One recently closed board row before the limit is applied.
struct ClosedBoardEntry {
    /// `expires_at` for a still-open lapsed proposal, `decided_at` for a
    /// decision. A missing decision time sorts after every timestamp.
    closed_at: Option<chrono::DateTime<Utc>>,
    id: String,
    view: Value,
}

/// Newest effective close first. A missing decision time sorts last because
/// `None` is less than `Some`. Id descending matches the store's
/// `ORDER BY decided_at DESC, id DESC`.
fn recently_closed_order(left: &ClosedBoardEntry, right: &ClosedBoardEntry) -> std::cmp::Ordering {
    right
        .closed_at
        .cmp(&left.closed_at)
        .then_with(|| right.id.cmp(&left.id))
}

/// The weeks a proposal stays open before it expires.
const PROPOSAL_OPEN_WEEKS: i64 = 4;

fn proposal_lapsed(row: &CrewMemoryProposalRow, now: chrono::DateTime<Utc>) -> bool {
    row.created_at < now - Duration::weeks(PROPOSAL_OPEN_WEEKS)
}

/// One proposal as the operator list shows it, without the similarity
/// count.
fn proposal_view(proposal: &CrewMemoryProposalRow) -> Result<Value, OrbitError> {
    let evidence: Vec<String> = serde_json::from_str(&proposal.evidence_json)
        .map_err(|error| OrbitError::Store(format!("invalid proposal evidence: {error}")))?;
    Ok(json!({
        "id": proposal.id,
        "key": proposal.proposing_key,
        "run_id": proposal.proposing_run_id,
        "text": proposal.text,
        "evidence": evidence,
        "status": proposal.status,
        "created_at": proposal.created_at,
        "expires_at": proposal.created_at + Duration::weeks(PROPOSAL_OPEN_WEEKS),
    }))
}

/// Open proposals with the number of other proposing keys whose open notes
/// have token Jaccard overlap of at least 0.5. Similarity never changes
/// status or selects a decision.
fn open_proposal_views(proposals: &[CrewMemoryProposalRow]) -> Result<Vec<Value>, OrbitError> {
    let tokens: Vec<_> = proposals
        .iter()
        .map(|row| proposal_tokens(&row.text))
        .collect();
    proposals
        .iter()
        .enumerate()
        .map(|(index, proposal)| {
            let similar_keys: BTreeSet<_> = proposals
                .iter()
                .enumerate()
                .filter(|(other, row)| {
                    row.proposing_key != proposal.proposing_key
                        && similar_proposal_tokens(&tokens[index], &tokens[*other])
                })
                .map(|(_, row)| &row.proposing_key)
                .collect();
            let mut view = proposal_view(proposal)?;
            view["similar_keys"] = json!(similar_keys.len());
            Ok(view)
        })
        .collect()
}

fn proposal_not_open(id: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "crew memory proposal `{id}` is not open or does not exist"
    ))
}

fn proposal_tokens(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn similar_proposal_tokens(left: &BTreeSet<String>, right: &BTreeSet<String>) -> bool {
    !left.is_empty() && left.intersection(right).count() * 2 >= left.union(right).count()
}
