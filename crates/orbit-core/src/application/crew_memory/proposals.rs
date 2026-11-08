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
        let proposals = self.open_crew_memory_proposals()?;
        let tokens: Vec<_> = proposals
            .iter()
            .map(|row| proposal_tokens(&row.text))
            .collect();
        let rows = proposals
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
                let evidence: Vec<String> =
                    serde_json::from_str(&proposal.evidence_json).map_err(|error| {
                        OrbitError::Store(format!("invalid proposal evidence: {error}"))
                    })?;
                Ok(json!({
                    "id": proposal.id,
                    "key": proposal.proposing_key,
                    "run_id": proposal.proposing_run_id,
                    "text": proposal.text,
                    "evidence": evidence,
                    "status": proposal.status,
                    "created_at": proposal.created_at,
                    "expires_at": proposal.created_at + Duration::weeks(4),
                    "similar_keys": similar_keys.len(),
                }))
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        Ok(json!({ "count": rows.len(), "proposals": rows }))
    }

    /// Accept an open proposal, appending its note to the current global
    /// text, or replacing the whole global text with the contents of `edit`.
    /// Both forms pass the operator's global write rules before deciding.
    pub fn crew_memory_proposal_accept(
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
    pub fn crew_memory_proposal_reject(&self, id: &str, reason: &str) -> Result<Value, OrbitError> {
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
        store.expire_crew_memory_proposals(&workspace_id, now - Duration::weeks(4), now)?;
        store.crew_memory_open_proposals(&workspace_id)
    }
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
