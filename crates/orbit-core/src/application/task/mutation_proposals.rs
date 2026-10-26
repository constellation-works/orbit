//! Mutation proposals for mutating operator-granted tools [granted-tools §4].
//!
//! A mutating granted tool never runs live in the implementing activity. The
//! writer records the intended call as a `mutation-proposals/` artifact,
//! which admission validates and binds to an idempotency key derived from the
//! task and the canonical tool + input. The review gate lists every unapplied
//! proposal in its manifest, bound to the exact bytes it read; after that
//! attempt passes, settlement applies exactly those inputs once, recording a
//! system-only `mutation-receipts/` artifact per key. An applied key never
//! applies again, a changed proposal is a new key needing a new review, and a
//! failure stops delivery with the receipt and a task comment naming it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{self, GrantedToolEffect, ToolGrantability};
use orbit_common::security::release::sha256_hex;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::{
    ArtifactWriter, MUTATION_PROPOSAL_SCHEMA_VERSION, MutationProposal, MutationProposalDraft,
    MutationReceipt, MutationReceiptState, ReviewedMutationProposal, Task, TaskArtifact,
    is_mutation_proposal_artifact, is_mutation_receipt_artifact, mutation_receipt_artifact_path,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

use super::TaskUpdateParams;
use crate::OrbitRuntime;
use crate::runtime::tool_exec::CapabilityEnforcement;

/// Contract label folded into every proposal idempotency key.
const IDEMPOTENCY_CONTRACT: &str = "granted_mutation_proposal_v1";

/// The idempotency key of one proposed call: the task plus the canonical
/// tool and input. The rationale is excluded, so rewording it never makes
/// the same change apply twice.
pub(crate) fn mutation_proposal_idempotency_key(
    task_id: &str,
    tool: &str,
    input: &Value,
) -> String {
    let mut value = json!({
        "contract": IDEMPOTENCY_CONTRACT,
        "task_id": task_id,
        "tool": tool,
        "input": input,
    });
    value.sort_all_objects();
    sha256_hex(value.to_string().as_bytes())
}

/// The session a proposal is judged under, at admission and application: the
/// agent capability alone. A grant lifts only a deny-list entry, so neither
/// the writer's nor the host's authority may satisfy an inner field-level
/// operator gate such as `expected_enabled`.
fn agent_tool_context() -> ToolContext {
    ToolContext {
        session_context: ToolSessionContext {
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            ..ToolSessionContext::default()
        },
        ..ToolContext::default()
    }
}

/// A stored proposal with the digest of its bytes and its receipt, if any.
struct StoredProposal {
    reviewed: ReviewedMutationProposal,
    receipt: Option<MutationReceipt>,
}

impl OrbitRuntime {
    /// Admit a caller-written `mutation-proposals/` artifact for `task`.
    ///
    /// The content is the writer's `{tool, input, rationale}`; the stored
    /// artifact is the canonical [`MutationProposal`] with the derived task id
    /// and idempotency key. Refused unless the task grants the tool, the tool
    /// is a grantable mutation, and an agent session could make the call.
    pub(crate) fn admit_mutation_proposal(
        &self,
        task: &Task,
        artifact: &mut TaskArtifact,
    ) -> Result<(), OrbitError> {
        let path = artifact.path.clone();
        let refuse = |reason: String| {
            OrbitError::InvalidInput(format!(
                "{path} is not an admissible mutation proposal: {reason}"
            ))
        };
        let draft: MutationProposalDraft =
            serde_json::from_slice(&artifact.content).map_err(|error| {
                refuse(format!(
                    "expected a JSON object with `tool`, `input` and `rationale` ({error})"
                ))
            })?;
        if draft
            .schema_version
            .is_some_and(|version| version != MUTATION_PROPOSAL_SCHEMA_VERSION)
        {
            return Err(refuse(format!(
                "schema_version must be {MUTATION_PROPOSAL_SCHEMA_VERSION}"
            )));
        }
        if draft.task_id.as_deref().is_some_and(|id| id != task.id) {
            return Err(refuse(format!("it names another task, not '{}'", task.id)));
        }
        if draft.rationale.trim().is_empty() {
            return Err(refuse(
                "`rationale` must say why the change is needed".into(),
            ));
        }
        self.check_granted_mutation(task, &draft.tool, &draft.input)
            .map_err(refuse)?;
        let idempotency_key =
            mutation_proposal_idempotency_key(&task.id, &draft.tool, &draft.input);
        if draft
            .idempotency_key
            .as_deref()
            .is_some_and(|key| key != idempotency_key)
        {
            return Err(refuse(
                "`idempotency_key` is derived from the task, tool and input; omit it".into(),
            ));
        }
        let proposal = MutationProposal {
            schema_version: MUTATION_PROPOSAL_SCHEMA_VERSION,
            task_id: task.id.to_string(),
            tool: draft.tool,
            input: draft.input,
            rationale: draft.rationale.trim().to_string(),
            idempotency_key,
        };
        artifact.content = serde_json::to_vec_pretty(&proposal)
            .map_err(|error| OrbitError::Execution(format!("serialize proposal: {error}")))?;
        artifact.media_type = "application/json".to_string();
        Ok(())
    }

    /// Every rule a proposed call must meet, at admission and again right
    /// before application: the task still grants the tool, the tool is still
    /// a grantable mutation, and an agent session passes its admission,
    /// capability and input-dependent field gates.
    fn check_granted_mutation(&self, task: &Task, tool: &str, input: &Value) -> Result<(), String> {
        if !task.granted_tools.iter().any(|granted| granted == tool) {
            return Err(format!(
                "task '{}' does not grant `{tool}`; only an operator's `granted_tools` admits a proposal",
                task.id
            ));
        }
        let classification = authorization::classify_agent_tool_grant(tool);
        if classification.grantability == ToolGrantability::AlwaysOperatorOnly {
            return Err(format!("`{tool}` is always operator-only"));
        }
        if classification.effect != GrantedToolEffect::Mutating {
            return Err(format!(
                "`{tool}` is classified read/repeat-safe; its grant makes it callable directly"
            ));
        }
        if !input.is_object() {
            return Err("`input` must be a JSON object of the tool's arguments".into());
        }
        self.authorize_registered_tool(
            tool,
            input,
            &agent_tool_context(),
            CapabilityEnforcement::McpSessionOnly,
        )
        .map_err(|error| format!("a grant does not satisfy `{tool}`'s own gates: {error}"))
    }

    /// The task's proposals in path order, each with its digest and receipt.
    /// Only a receipt the system writer stored counts.
    fn stored_mutation_proposals(&self, task_id: &str) -> Result<Vec<StoredProposal>, OrbitError> {
        let manifest = self.get_task_artifact_manifest(task_id)?;
        let system_receipts = manifest
            .iter()
            .filter(|file| {
                is_mutation_receipt_artifact(&file.path)
                    && file.writer == Some(ArtifactWriter::System)
            })
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        let mut proposals = BTreeMap::new();
        for file in manifest
            .iter()
            .filter(|file| is_mutation_proposal_artifact(&file.path))
        {
            let Some(artifact) = self.get_task_artifact(task_id, &file.path)? else {
                continue;
            };
            let proposal: MutationProposal =
                serde_json::from_slice(&artifact.content).map_err(|error| {
                    OrbitError::Execution(format!(
                        "task '{task_id}' mutation proposal {} is unreadable: {error}",
                        file.path
                    ))
                })?;
            let receipt_path = mutation_receipt_artifact_path(&proposal.idempotency_key);
            let receipt = if system_receipts.contains(&receipt_path) {
                self.get_task_artifact(task_id, &receipt_path)?
                    .map(|receipt| serde_json::from_slice::<MutationReceipt>(&receipt.content))
                    .transpose()
                    .map_err(|error| {
                        OrbitError::Execution(format!(
                            "task '{task_id}' receipt {receipt_path} is unreadable: {error}"
                        ))
                    })?
            } else {
                None
            };
            proposals.insert(
                file.path.clone(),
                StoredProposal {
                    reviewed: ReviewedMutationProposal {
                        artifact: file.path.clone(),
                        sha256: sha256_hex(&artifact.content),
                        proposal,
                    },
                    receipt,
                },
            );
        }
        Ok(proposals.into_values().collect())
    }

    /// The task's proposals no applied receipt covers, bound to their bytes:
    /// what the review gate names and its reviewer judges.
    pub(crate) fn unapplied_mutation_proposals(
        &self,
        task_id: &str,
    ) -> Result<Vec<ReviewedMutationProposal>, OrbitError> {
        Ok(self
            .stored_mutation_proposals(task_id)?
            .into_iter()
            .filter(|stored| !applied(stored))
            .map(|stored| stored.reviewed)
            .collect())
    }

    /// Apply the proposals review attempt `attempt_id` approved, once each.
    ///
    /// `reviewed` is the gate's manifest record. A task proposal applies only
    /// when that record holds it with the same path, digest and content; its
    /// reviewed input is what the host calls, under the agent capability, and
    /// a receipt is recorded before and after the call. An applied key is
    /// skipped, a pending one (outcome unknown) is never re-applied, and any
    /// unreviewed or failed proposal fails the whole call after the others
    /// ran, with a task comment naming each.
    pub(crate) fn apply_reviewed_mutation_proposals(
        &self,
        run_id: &str,
        attempt_id: &str,
        task_ids: &[String],
        reviewed: &[ReviewedMutationProposal],
    ) -> Result<Vec<MutationReceipt>, OrbitError> {
        let mut receipts = Vec::new();
        for task_id in task_ids {
            let task = self.get_task(task_id)?;
            let mut failures = Vec::new();
            for stored in self.stored_mutation_proposals(task_id)? {
                if applied(&stored) {
                    continue;
                }
                let current = &stored.reviewed;
                let approved = reviewed.iter().any(|record| {
                    record.artifact == current.artifact
                        && record.sha256 == current.sha256
                        && record.proposal == current.proposal
                });
                if !approved {
                    failures.push(format!(
                        "{} was added or changed after review attempt {attempt_id} read it; it applies only after a review that names it",
                        current.artifact
                    ));
                    continue;
                }
                if stored
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.state == MutationReceiptState::Pending)
                {
                    failures.push(format!(
                        "{} has a pending receipt from an interrupted application, so its outcome is unknown; an operator must check the live state before it is applied again",
                        current.artifact
                    ));
                    continue;
                }
                let receipt = self.apply_mutation_proposal(&task, current, run_id, attempt_id)?;
                if let Some(error) = &receipt.error {
                    failures.push(format!("{} failed to apply: {error}", current.artifact));
                }
                receipts.push(receipt);
            }
            if !failures.is_empty() {
                let message = format!(
                    "mutation_application_failed: task '{task_id}' has mutation proposals that were not applied: {}. Delivery stops here; nothing claims these changes happened.",
                    failures.join("; ")
                );
                self.update_task_as_system(
                    task_id,
                    TaskUpdateParams {
                        comment: Some(message.clone()),
                        ..TaskUpdateParams::default()
                    },
                    Some(run_id.to_string()),
                )?;
                return Err(OrbitError::Execution(message));
            }
        }
        Ok(receipts)
    }

    /// Apply one approved proposal: re-check its rules, record a pending
    /// receipt, call the tool with the reviewed input, and record the outcome.
    /// A refused or failed call is a `failed` receipt, not an error; only a
    /// receipt write failure is.
    fn apply_mutation_proposal(
        &self,
        task: &Task,
        approved: &ReviewedMutationProposal,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<MutationReceipt, OrbitError> {
        let proposal = &approved.proposal;
        let mut receipt = MutationReceipt {
            schema_version: MUTATION_PROPOSAL_SCHEMA_VERSION,
            task_id: task.id.to_string(),
            idempotency_key: proposal.idempotency_key.clone(),
            tool: proposal.tool.clone(),
            proposal_artifact: approved.artifact.clone(),
            proposal_sha256: approved.sha256.clone(),
            applied_input: proposal.input.clone(),
            state: MutationReceiptState::Pending,
            result: None,
            error: None,
            review_attempt_id: attempt_id.to_string(),
            run_id: run_id.to_string(),
            recorded_at: Utc::now(),
        };
        let derived = mutation_proposal_idempotency_key(&task.id, &proposal.tool, &proposal.input);
        let refusal = if proposal.task_id != task.id || proposal.idempotency_key != derived {
            Some("its task id or idempotency key does not match its content".to_string())
        } else {
            self.check_granted_mutation(task, &proposal.tool, &proposal.input)
                .err()
        };
        if let Some(refusal) = refusal {
            receipt.state = MutationReceiptState::Failed;
            receipt.error = Some(refusal);
            self.record_mutation_receipt(&receipt)?;
            return Ok(receipt);
        }
        self.record_mutation_receipt(&receipt)?;
        let outcome = self.run_tool_with_context_and_role_and_capability(
            &proposal.tool,
            proposal.input.clone(),
            Role::Admin,
            agent_tool_context(),
            CapabilityEnforcement::McpSessionOnly,
        );
        match outcome {
            Ok(result) => {
                receipt.state = MutationReceiptState::Applied;
                receipt.result = Some(result);
            }
            Err(error) => {
                receipt.state = MutationReceiptState::Failed;
                receipt.error = Some(error.to_string());
            }
        }
        receipt.recorded_at = Utc::now();
        self.record_mutation_receipt(&receipt)?;
        Ok(receipt)
    }

    fn record_mutation_receipt(&self, receipt: &MutationReceipt) -> Result<(), OrbitError> {
        let content = serde_json::to_vec_pretty(receipt)
            .map_err(|error| OrbitError::Execution(format!("serialize receipt: {error}")))?;
        self.update_task_as_system(
            &receipt.task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: mutation_receipt_artifact_path(&receipt.idempotency_key),
                    content,
                    media_type: "application/json".to_string(),
                    created_by: None,
                }],
                ..TaskUpdateParams::default()
            },
            Some(receipt.run_id.clone()),
        )?;
        Ok(())
    }

    /// Refuse completion while any proposal is unapplied: a task cannot be
    /// done while a reviewed change it carries never happened, or while an
    /// unreviewed one waits. A claimed worker completes nothing itself; its
    /// owner's completion is the one held.
    pub(crate) fn ensure_mutation_proposals_applied(&self, task: &Task) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            return Ok(());
        }
        let unapplied = self.unapplied_mutation_proposals(&task.id)?;
        if unapplied.is_empty() {
            return Ok(());
        }
        Err(OrbitError::PolicyDenied(format!(
            "completion requires every mutation proposal to be applied after review; unapplied: {}",
            unapplied
                .iter()
                .map(|proposal| proposal.artifact.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

fn applied(stored: &StoredProposal) -> bool {
    stored
        .receipt
        .as_ref()
        .is_some_and(|receipt| receipt.state == MutationReceiptState::Applied)
}
