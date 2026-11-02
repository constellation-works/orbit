//! Enqueue on raise and seal on the owner tick. The model is never called
//! before the decision request is durable, and it is not called on the raise path.
use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::assistance::AssistanceAdmission;
use orbit_types::assistance::{self, AssistanceEvidencePacket, AssistanceRequest};
use orbit_types::task::{DecisionRequest, DecisionRequestState};
use serde_json::json;

use super::contract::{
    BUILDER_VERSION, CONTEXT_BYTES, PRECEDENT_DECISION_USE_CASE, PRECEDENT_K, bound_text,
    options_admissible, precedent_decision_question_template, question_text, request_category,
    risk_tier_name,
};
use super::ledger::{
    PrecedentDecisionReceipt, read_receipt, receipt_dir, receipt_path, with_receipt, write_receipt,
};
use crate::OrbitRuntime;
use crate::application::assistance::AssistanceService;
use crate::application::task::precedent_provider::{
    self, PRECEDENT_PROVIDER_CONTRACT, PrecedentProviderRequest, PrecedentQuery,
};

const RECONCILE_BOUND: usize = 32;
const PROCESS_BOUND: usize = 32;

impl OrbitRuntime {
    /// Best-effort enqueue for a request this raise left open. A consumer
    /// failure does not fail the human-visible raise.
    pub(crate) fn note_open_precedent_decision(&self, request: &DecisionRequest) {
        if request.state != DecisionRequestState::Open {
            return;
        }
        if let Err(error) = self.consider_precedent_decision(request) {
            tracing::debug!(%error, "precedent decision shadow enqueue deferred");
        }
    }

    /// Enqueue one shadow assessment when this workspace's grant is shadow.
    /// Off and advisory return before any precedent-provider call.
    pub fn consider_precedent_decision(&self, request: &DecisionRequest) -> Result<(), OrbitError> {
        let service = self.precedent_decision_service()?;
        self.consider_precedent_decision_with_service(request, &service)
    }

    /// Same admission as [`Self::consider_precedent_decision`], with the
    /// caller's service so a fixture can seal through a synthetic adapter.
    pub fn consider_precedent_decision_with_service(
        &self,
        request: &DecisionRequest,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        if request.state != DecisionRequestState::Open {
            return Ok(());
        }
        let workspace = self.workspace_id()?;
        let Some(epoch) =
            service.assistance_shadow_policy_epoch(&workspace, PRECEDENT_DECISION_USE_CASE)?
        else {
            return Ok(());
        };
        let path = receipt_path(self, &request.id)?;
        with_receipt(&path, || {
            if let Some(existing) = read_receipt(&path)?
                && (existing.work_id.is_some()
                    || existing.sealed_at.is_some()
                    || matches!(
                        existing.admission_gap.as_deref(),
                        Some("options_not_admissible" | "enqueue_refused")
                    ))
            {
                return Ok(());
            }
            let category = request_category(request, &workspace);
            let mut receipt = PrecedentDecisionReceipt {
                schema_version: 1,
                request_id: request.id.clone(),
                binding_key: request.binding.key(),
                category,
                raised_at: request.raised_at,
                policy_epoch: epoch,
                source_digest: assistance::assistance_digest(request.id.as_bytes()),
                work_id: None,
                sealed_at: None,
                admission_gap: None,
            };
            if !options_admissible(request) {
                receipt.admission_gap = Some("options_not_admissible".into());
                receipt.sealed_at = Some(Utc::now());
                write_receipt(&path, &receipt)?;
                return Ok(());
            }
            let built = self.build_precedent_packet(request, &workspace)?;
            receipt.source_digest = built.source_digest.clone();
            match service.enqueue_expanded_assistance(
                built.request,
                &[],
                &precedent_decision_question_template(),
            ) {
                Ok(AssistanceAdmission::Enqueued(id) | AssistanceAdmission::Existing(id)) => {
                    receipt.work_id = Some(id);
                    receipt.admission_gap = None;
                    write_receipt(&path, &receipt)
                }
                Ok(AssistanceAdmission::Disabled) => Ok(()),
                Ok(AssistanceAdmission::QueueFull) => {
                    receipt.admission_gap = Some("queue_full".into());
                    write_receipt(&path, &receipt)
                }
                Err(error) => {
                    tracing::debug!(%error, "precedent decision enqueue refused");
                    receipt.admission_gap = Some("enqueue_refused".into());
                    receipt.sealed_at = Some(Utc::now());
                    write_receipt(&path, &receipt)
                }
            }
        })
    }

    /// Owner reconciliation. Shadow only: no open-request scan and no provider
    /// call unless the grant is shadow. Replicas do not write.
    pub fn run_precedent_decision_tick(&self) -> Result<(), OrbitError> {
        if self.coordination_write_owner().is_some() {
            return Ok(());
        }
        let service = self.precedent_decision_service()?;
        self.run_precedent_decision_with_service(&service)
    }

    /// Reconcile open requests, run a bounded number of assessments, and seal
    /// each settled result. Does not call the provider when shadow is off.
    pub fn run_precedent_decision_with_service(
        &self,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        let workspace = self.workspace_id()?;
        if self.coordination_write_owner().is_some()
            || service
                .assistance_shadow_policy_epoch(&workspace, PRECEDENT_DECISION_USE_CASE)?
                .is_none()
        {
            return Ok(());
        }
        let open = self.list_decision_requests(None, None, false)?;
        let mut reconciled = 0;
        for request in &open {
            if reconciled >= RECONCILE_BOUND {
                break;
            }
            let path = receipt_path(self, &request.id)?;
            let pending = read_receipt(&path)?
                .is_none_or(|receipt| receipt.work_id.is_none() && receipt.sealed_at.is_none());
            if pending {
                self.consider_precedent_decision_with_service(request, service)?;
                reconciled += 1;
            }
        }
        for _ in 0..PROCESS_BOUND {
            let epoch =
                service.assistance_shadow_policy_epoch(&workspace, PRECEDENT_DECISION_USE_CASE)?;
            let Some(epoch) = epoch else {
                break;
            };
            let processed = service.process_next_assistance_checked(
                &workspace,
                PRECEDENT_DECISION_USE_CASE,
                |enqueued| self.precedent_work_is_current(enqueued, &epoch),
            )?;
            if processed.is_none() {
                break;
            }
        }
        self.seal_settled_precedent_decisions(service)
    }

    /// One private receipt, including a sealed prediction. Decision surfaces
    /// do not call this.
    pub fn precedent_decision_receipt(
        &self,
        request_id: &str,
    ) -> Result<Option<PrecedentDecisionReceipt>, OrbitError> {
        read_receipt(&receipt_path(self, request_id)?)
    }

    fn precedent_work_is_current(
        &self,
        request: &AssistanceRequest,
        epoch: &str,
    ) -> Result<bool, OrbitError> {
        let Some(receipt) =
            read_receipt(&receipt_path(self, &request.evidence_packet.subject_id)?)?
        else {
            return Ok(false);
        };
        Ok(receipt.schema_version == 1
            && receipt.sealed_at.is_none()
            && receipt.source_digest == request.source_digest
            && receipt.policy_epoch == epoch)
    }

    fn seal_settled_precedent_decisions(
        &self,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        let workspace = assistance::assistance_digest(self.workspace_id()?.as_bytes());
        let dir = receipt_dir(self, &workspace)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut sealed = 0;
        for entry in entries {
            if sealed >= PROCESS_BOUND {
                break;
            }
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let did_seal = with_receipt(&path, || {
                let Some(mut receipt) = read_receipt(&path)? else {
                    return Ok(false);
                };
                if receipt.sealed_at.is_some() {
                    return Ok(false);
                }
                let Some(work_id) = receipt.work_id.clone() else {
                    return Ok(false);
                };
                let Some(record) =
                    service.assistance_assessment(&work_id, &receipt.source_digest)?
                else {
                    return Ok(false);
                };
                if !record.settled || record.result.is_none() {
                    return Ok(false);
                }
                receipt.sealed_at = Some(Utc::now());
                write_receipt(&path, &receipt)?;
                Ok(true)
            })?;
            if did_seal {
                sealed += 1;
            }
        }
        Ok(())
    }

    fn build_precedent_packet(
        &self,
        request: &DecisionRequest,
        workspace: &str,
    ) -> Result<BuiltPacket, OrbitError> {
        let question = question_text(request);
        let option_ids: Vec<&str> = request
            .answers
            .iter()
            .map(|answer| answer.id.as_str())
            .collect();
        let retrieval = self.retrieve_decision_precedents(&PrecedentProviderRequest {
            contract: PRECEDENT_PROVIDER_CONTRACT.into(),
            workspace_id: workspace.into(),
            as_of: request.raised_at,
            k: PRECEDENT_K,
            request: PrecedentQuery {
                id: request.id.clone(),
                gate: request.gate.as_str().into(),
                category: request_category(request, workspace),
                question: question.clone(),
                options: request
                    .answers
                    .iter()
                    .map(|answer| precedent_provider::PrecedentOption {
                        id: answer.id.clone(),
                        label: answer.label.clone(),
                        consequence: Some(answer.consequence.clone()),
                    })
                    .collect(),
                topic: request.topic.clone(),
                raised_at: request.raised_at,
            },
        })?;
        let precedents: Vec<_> = retrieval
            .precedents
            .iter()
            .take(PRECEDENT_K)
            .map(|precedent| {
                let age_days = (request.raised_at - precedent.decided_at).num_days().max(0);
                json!({
                    "id": precedent.id,
                    "chosen_option": precedent.chosen_option,
                    "reversed": precedent.reversed,
                    "age_days": age_days,
                    "category": precedent.scope.category,
                    "topic": precedent.scope.topic,
                    "decided_at": precedent.decided_at.to_rfc3339(),
                })
            })
            .collect();
        let task = self.get_task(&request.task_id)?;
        let (description, description_truncated) = bound_text(&task.description, CONTEXT_BYTES);
        let (criteria_text, criteria_truncated) =
            bound_text(&task.acceptance_criteria.join("\n"), CONTEXT_BYTES);
        let truncated = retrieval.truncated
            || description_truncated
            || criteria_truncated
            || !retrieval.enabled;
        let mut omissions = retrieval.omissions.clone();
        if !retrieval.enabled {
            omissions.push("provider_disabled".into());
        }
        let gaps: Vec<String> = retrieval
            .provider_gaps
            .iter()
            .map(|gap| {
                let reason = serde_json::to_value(gap.reason)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unavailable".into());
                format!("{}:{reason}", gap.provider)
            })
            .collect();
        let precedent_identity: Vec<_> = retrieval
            .precedents
            .iter()
            .take(PRECEDENT_K)
            .map(|precedent| {
                (
                    precedent.id.as_str(),
                    precedent.chosen_option.as_str(),
                    precedent.reversed,
                    precedent.decided_at,
                )
            })
            .collect();
        let source_digest = assistance::assistance_digest(
            &serde_json::to_vec(&(
                "precedent-decision-source/1",
                &request.id,
                request.raised_at,
                request.binding.key(),
                &option_ids,
                &question,
                &precedent_identity,
                &omissions,
                &gaps,
                truncated,
            ))
            .map_err(super::ledger::ledger_json_error)?,
        );
        let mut contract = precedent_decision_question_template();
        let supports = contract
            .questions
            .get("precedent_supports")
            .cloned()
            .ok_or_else(|| OrbitError::Store("precedent decision template is incomplete".into()))?;
        let choice = contract
            .questions
            .get("decision_choice")
            .cloned()
            .ok_or_else(|| OrbitError::Store("precedent decision template is incomplete".into()))?;
        let conflict = contract
            .questions
            .get("precedents_conflict")
            .cloned()
            .ok_or_else(|| OrbitError::Store("precedent decision template is incomplete".into()))?;
        contract.questions.clear();
        contract.questions.insert("decision_choice".into(), choice);
        for index in 1..=precedents.len() {
            contract
                .questions
                .insert(format!("precedent_supports/{index}"), supports.clone());
        }
        contract
            .questions
            .insert("precedents_conflict".into(), conflict);
        let provenance = json!({
            "builder": BUILDER_VERSION,
            "provider_contract": PRECEDENT_PROVIDER_CONTRACT,
            "provider_ids": retrieval.precedents.iter().take(PRECEDENT_K).map(|precedent| &precedent.citation.source).collect::<Vec<_>>(),
            "omissions": omissions,
            "gaps": gaps,
            "truncated": truncated,
            "as_of": request.raised_at.to_rfc3339(),
        });
        let packet = AssistanceRequest {
            workspace_id: workspace.into(),
            owner_id: workspace.into(),
            use_case: PRECEDENT_DECISION_USE_CASE.into(),
            source_digest: source_digest.clone(),
            contract,
            evidence_packet: AssistanceEvidencePacket {
                packet_id: source_digest.clone(),
                subject_id: request.id.clone(),
                collected_at: Utc::now(),
                evidence_refs: vec![
                    format!("task:{}", request.task_id),
                    format!("decision:{}", request.id),
                ],
                omissions: Vec::new(),
                truncated: false,
                fields: BTreeMap::from([
                    ("option_ids".into(), json!(option_ids)),
                    (
                        "request".into(),
                        json!({
                            "id": request.id,
                            "workspace": workspace,
                            "gate": request.gate.as_str(),
                            "category": request_category(request, workspace),
                            "risk_tier": risk_tier_name(request.risk_tier),
                            "question": question,
                            "answers": request.answers.iter().map(|answer| json!({
                                "id": answer.id,
                                "label": answer.label,
                                "consequence": answer.consequence,
                            })).collect::<Vec<_>>(),
                            "recommended": request.recommended,
                            "topic": request.topic,
                            "raised_at": request.raised_at.to_rfc3339(),
                        }),
                    ),
                    (
                        "task_context".into(),
                        json!({
                            "title": task.title,
                            "description": description,
                            "acceptance_criteria": criteria_text,
                            "description_truncated": description_truncated,
                            "criteria_truncated": criteria_truncated,
                        }),
                    ),
                    ("precedents".into(), json!(precedents)),
                    ("provenance".into(), provenance),
                ]),
            },
        };
        Ok(BuiltPacket {
            request: packet,
            source_digest,
        })
    }
}

struct BuiltPacket {
    request: AssistanceRequest,
    source_digest: String,
}
