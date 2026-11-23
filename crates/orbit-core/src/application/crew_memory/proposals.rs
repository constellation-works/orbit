//! The read of proposed global notes. Similarity is display-only. Crew
//! memory is dormant, so no surface decides a proposal: an open one lapses
//! after four weeks, and the read shows it closed then.

use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::CrewMemoryProposalRow;
use serde_json::{Value, json};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// The read of the proposals, which writes nothing: the open ones with
    /// their similarity counts, and the most recently closed ones with their
    /// decisions. An open proposal past its
    /// four weeks is shown closed as `expired` at its `expires_at`; a
    /// decision is closed at `decided_at`. Those close times are one list,
    /// newest first, with the proposal id descending when they tie, and only
    /// the twenty most recent remain. Nothing records a withdrawal. Unless a
    /// running experiment has a memory arm both lists are empty.
    pub fn crew_memory_proposal_board(&self) -> Result<Value, OrbitError> {
        if !self.crew_memory_experiment_running() {
            return Ok(json!({
                "enabled": false,
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
}

/// How many closed proposals the read returns after merging
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

/// One proposal as the read shows it, without the similarity count.
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

fn proposal_tokens(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn similar_proposal_tokens(left: &BTreeSet<String>, right: &BTreeSet<String>) -> bool {
    !left.is_empty() && left.intersection(right).count() * 2 >= left.union(right).count()
}
