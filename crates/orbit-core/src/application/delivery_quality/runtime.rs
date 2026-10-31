use super::super::assistance::AssistanceService;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::assistance::AssistanceAdmission;
use orbit_types::assistance::{AssistanceReason, AssistanceResult, AssistanceValue};
use std::collections::BTreeMap;

use super::contract::{
    DELIVERY_QUALITY_DIMENSIONS, DELIVERY_QUALITY_GRADES, DELIVERY_QUALITY_USE_CASE,
    DeliveryQualityApplicability, FrozenDeliveryQuality, delivery_quality_question_template,
};
use super::ledger::QualityReceipt;
use super::profile::{DeliveryQualityDimension, DeliveryQualityProfile, DeliveryQualitySource};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Owner-only periodic composition; the service checks the global grant
    /// before source reads, consumer locks, queue writes or adapter work.
    pub fn run_delivery_quality_tick(&self) -> Result<(), OrbitError> {
        let service = self.delivery_quality_service()?;
        let Some(epoch) = service
            .assistance_shadow_policy_epoch(&self.workspace_id()?, DELIVERY_QUALITY_USE_CASE)?
        else {
            return Ok(());
        };
        if self.coordination_write_owner().is_some() {
            return Ok(());
        }
        let path = super::ledger::quality_root(self)?.join(format!(
            "window-{}.json",
            &super::contract::quality_digest(&epoch)?[7..]
        ));
        let since = super::ledger::quality_locked(&path, || {
            if let Some(since) = super::ledger::quality_read::<DateTime<Utc>>(&path)? {
                return Ok(since);
            }
            let since = Utc::now();
            super::ledger::quality_write(&path, &since)?;
            Ok(since)
        })?;
        self.run_delivery_quality_with_source(
            &service,
            &super::source::RuntimeDeliveryQualitySource(self, since),
        )
    }
    pub fn run_delivery_quality_with_source(
        &self,
        service: &AssistanceService,
        source: &dyn DeliveryQualitySource,
    ) -> Result<(), OrbitError> {
        let workspace = self.workspace_id()?;
        if self.coordination_write_owner().is_some()
            || service
                .assistance_shadow_policy_epoch(&workspace, DELIVERY_QUALITY_USE_CASE)?
                .is_none()
        {
            return Ok(());
        }
        let cursor = super::ledger::quality_root(self)?.join("scan.json");
        super::ledger::quality_locked(&cursor, || {
            if service
                .assistance_shadow_policy_epoch(&workspace, DELIVERY_QUALITY_USE_CASE)?
                .is_none()
            {
                return Ok(());
            }
            let after = super::ledger::quality_read::<String>(&cursor)?.unwrap_or_default();
            let page = source.delivery_quality_page(&after)?;
            if page.units.len() > 32 {
                return Err(OrbitError::InvalidInput(
                    "delivery quality page exceeds bound".into(),
                ));
            }
            for unit in page.units {
                self.capture_delivery_quality(&unit, service, true)?;
            }
            // Advance only after all receipts exist. A crash replays the same
            // units; assistance's immutable ID makes admission idempotent.
            super::ledger::quality_write(&cursor, &page.next_cursor)
        })?;
        service.process_next_assistance_checked(
            &workspace,
            DELIVERY_QUALITY_USE_CASE,
            |request| {
                let path =
                    super::ledger::quality_receipt_path(self, &request.evidence_packet.subject_id)?;
                Ok(
                    super::ledger::quality_read::<QualityReceipt>(&path)?.is_some_and(|r| {
                        r.schema_version == 1
                            && service
                                .assistance_shadow_policy_epoch(
                                    &workspace,
                                    DELIVERY_QUALITY_USE_CASE,
                                )
                                .ok()
                                .flatten()
                                .as_ref()
                                == Some(&r.policy_epoch)
                            && r.source_digest == request.source_digest
                            && r.gaps.is_empty()
                            && !r.non_code
                    }),
                )
            },
        )?;
        Ok(())
    }

    /// Public application/manual-fixture boundary. The caller is the evidence
    /// builder, not an external agent/tool; it owns mapping and applicability.
    /// No policy means no receipt, lock, redaction, credentials or queue write.
    pub fn freeze_delivery_quality(
        &self,
        unit: &FrozenDeliveryQuality,
        service: &AssistanceService,
    ) -> Result<Option<String>, OrbitError> {
        self.capture_delivery_quality(unit, service, false)
    }
    pub(super) fn capture_delivery_quality(
        &self,
        unit: &FrozenDeliveryQuality,
        service: &AssistanceService,
        admit: bool,
    ) -> Result<Option<String>, OrbitError> {
        let workspace = self.workspace_id()?;
        let Some(epoch) =
            service.assistance_shadow_policy_epoch(&workspace, DELIVERY_QUALITY_USE_CASE)?
        else {
            return Ok(None);
        };
        if unit.identity.workspace != workspace
            || unit.identity.owner.is_empty()
            || unit.identity.task_id.is_empty()
            || unit.identity.actual_run_id.is_empty()
        {
            return Err(OrbitError::InvalidInput(
                "delivery quality ownership unavailable".into(),
            ));
        }
        // The host-only metadata has its own bounded surface. No arbitrary
        // model rationale or unbounded diagnostics can enter a receipt.
        if super::contract::quality_encode(&unit.identity)?.len() > 4096
            || super::contract::quality_encode(&unit.provenance)?.len() > 8192
            || unit.omissions.len() > 128
        {
            return Err(OrbitError::InvalidInput(
                "delivery quality metadata exceeds bound".into(),
            ));
        }
        let unit_id = unit.delivery_quality_unit_id()?;
        let id = super::contract::quality_digest(&(
            &unit_id,
            &epoch,
            service.assistance_adapter_revision(),
            delivery_quality_question_template(),
            &unit.source_version,
        ))?;
        let path = super::ledger::quality_receipt_path(self, &id)?;
        super::ledger::quality_locked(&path, || {
            if service
                .assistance_shadow_policy_epoch(&workspace, DELIVERY_QUALITY_USE_CASE)?
                .as_ref()
                != Some(&epoch)
            {
                return Ok(None);
            }
            let mut receipt =
                if let Some(receipt) = super::ledger::quality_read::<QualityReceipt>(&path)? {
                    receipt
                } else {
                    QualityReceipt {
                        schema_version: 1,
                        id: id.clone(),
                        unit_id,
                        identity: unit.identity.clone(),
                        provenance: unit.provenance.clone(),
                        policy_epoch: epoch.clone(),
                        grader_adapter: service.assistance_adapter_revision().into(),
                        frozen_at: Utc::now(),
                        evidence_digest: String::new(),
                        source_digest: String::new(),
                        gaps: unit.delivery_quality_gaps(),
                        non_code: unit.non_code,
                        work_id: None,
                        admission_gap: None,
                    }
                };
            let (request, redacted) = super::ledger::quality_prepared_request(
                unit.quality_request(receipt.frozen_at, &id)?,
            )?;
            let digest = super::contract::quality_digest(&request.evidence_packet)?;
            if !receipt.evidence_digest.is_empty() && receipt.evidence_digest != digest {
                // Same immutable identity, different evidence is a conflict,
                // never a silent replacement or a best-retry selection.
                return Err(OrbitError::InvalidInput(
                    "frozen delivery evidence changed; use a new source version".into(),
                ));
            }
            receipt.source_digest = request.source_digest.clone();
            receipt.evidence_digest = digest;
            if redacted {
                receipt.gaps.push("redacted_evidence".into());
            }
            if super::contract::quality_encode(&request)?.len() > 65_536 {
                receipt.gaps.push("oversize".into());
            }
            receipt.gaps.sort();
            receipt.gaps.dedup();
            // Freeze before admission. Source text is not persisted here; only
            // the assistance store holds its sanitized packet under retention.
            super::ledger::quality_write(&path, &receipt)?;
            if admit && receipt.work_id.is_none() && receipt.gaps.is_empty() && !receipt.non_code {
                match service.enqueue_assistance(request, &[]) {
                    Ok(
                        AssistanceAdmission::Enqueued(work) | AssistanceAdmission::Existing(work),
                    ) => {
                        receipt.work_id = Some(work);
                        receipt.admission_gap = None;
                    }
                    Ok(AssistanceAdmission::Disabled) => return Ok(None),
                    Ok(AssistanceAdmission::QueueFull) => {
                        receipt.admission_gap = Some(AssistanceReason::QueueFull)
                    }
                    Err(_) => receipt.admission_gap = Some(AssistanceReason::InvalidRequest),
                }
                super::ledger::quality_write(&path, &receipt)?;
            }
            Ok(Some(id.clone()))
        })
    }

    /// Reads are policy-fenced too. A switched-off profile is absent, and
    /// retention never falls back to a cached answer in the consumer ledger.
    pub fn delivery_quality_profile(
        &self,
        id: &str,
        service: &AssistanceService,
    ) -> Result<Option<DeliveryQualityProfile>, OrbitError> {
        let workspace = self.workspace_id()?;
        let Some(epoch) =
            service.assistance_shadow_policy_epoch(&workspace, DELIVERY_QUALITY_USE_CASE)?
        else {
            return Ok(None);
        };
        let Some(receipt) = super::ledger::quality_read::<QualityReceipt>(
            &super::ledger::quality_receipt_path(self, id)?,
        )?
        else {
            return Ok(None);
        };
        let record = receipt
            .work_id
            .as_ref()
            .map(|w| service.assistance_assessment(w, &receipt.source_digest))
            .transpose()?
            .flatten();
        let mut result = record.as_ref().and_then(|r| r.result.clone());
        let mut gaps = receipt.gaps.clone();
        if let Some(record) = &record {
            gaps.extend(
                record
                    .work
                    .request
                    .evidence_packet
                    .omissions
                    .iter()
                    .cloned(),
            );
            if record.work.request.evidence_packet.truncated {
                gaps.push("service_scope_truncated".into());
            }
        }

        if receipt.policy_epoch != epoch
            || receipt.grader_adapter != service.assistance_adapter_revision()
        {
            result = Some(AssistanceResult::assistance_gap(
                AssistanceReason::Superseded,
            ));
            gaps.push("policy_changed".into());
        }
        if let Some(gap) = receipt.admission_gap {
            gaps.push(format!("{gap:?}"));
        }
        if result.is_none() && !receipt.non_code && gaps.is_empty() {
            gaps.push(
                if record.is_some() {
                    "pending"
                } else {
                    "retention_or_unavailable"
                }
                .into(),
            );
        }
        let template = delivery_quality_question_template();
        let applicability = record
            .as_ref()
            .and_then(|r| r.work.request.evidence_packet.fields.get("coverage"))
            .and_then(|v| v.get("applicability"))
            .cloned()
            .and_then(|v| {
                serde_json::from_value::<BTreeMap<String, DeliveryQualityApplicability>>(v).ok()
            })
            .unwrap_or_default();
        let dimensions = DELIVERY_QUALITY_DIMENSIONS
            .into_iter()
            .map(|name| {
                let choice = result
                    .as_ref()
                    .and_then(|r| r.answers.get(name))
                    .and_then(|a| {
                        if let AssistanceValue::Choice(c) = &a.value {
                            Some(c.clone())
                        } else {
                            None
                        }
                    });
                let app = applicability
                    .get(name)
                    .cloned()
                    .unwrap_or(if receipt.non_code {
                        DeliveryQualityApplicability::NotApplicable
                    } else {
                        DeliveryQualityApplicability::Unavailable
                    });
                let ordinal = if app == DeliveryQualityApplicability::Applicable && gaps.is_empty()
                {
                    choice
                        .as_ref()
                        .and_then(|c| DELIVERY_QUALITY_GRADES.iter().position(|g| g == c))
                        .map(|i| i as u8 + 1)
                } else {
                    None
                };
                (
                    name.into(),
                    DeliveryQualityDimension {
                        ordinal,
                        choice,
                        disposition: result.as_ref().map(|r| r.disposition),
                        applicability: app,
                    },
                )
            })
            .collect();
        // Recheck after store reads before returning a publishable view.
        if service
            .assistance_shadow_policy_epoch(&workspace, DELIVERY_QUALITY_USE_CASE)?
            .as_ref()
            != Some(&epoch)
        {
            return Ok(None);
        }
        Ok(Some(DeliveryQualityProfile {
            schema_version: 1,
            assessment_id: id.into(),
            unit_id: receipt.unit_id,
            identity: receipt.identity,
            provenance: receipt.provenance,
            frozen_at: receipt.frozen_at,
            evidence_digest: record
                .as_ref()
                .map(|r| r.work.evidence_digest.clone())
                .unwrap_or(receipt.evidence_digest),
            work_id: receipt.work_id,
            evidence_refs: record
                .as_ref()
                .map(|r| r.work.request.evidence_packet.evidence_refs.clone())
                .unwrap_or_default(),
            policy_epoch: receipt.policy_epoch,
            question_version: template.question_set_version,
            rubric_version: template.rubric_version,
            builder_version: template.evidence_builder_version,
            grader: Some(receipt.grader_adapter),
            grader_model: record.as_ref().map(|r| r.work.requested_model.clone()),
            grader_revision: record.as_ref().map(|r| r.work.model_revision.clone()),
            gaps,
            non_code: receipt.non_code,
            result,
            dimensions,
        }))
    }
}
