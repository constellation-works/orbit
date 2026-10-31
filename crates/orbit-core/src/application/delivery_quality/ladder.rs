//! Read-only producer port for a separately adopted ladder consumer. Merely
//! shipping this module does not relax any phase-1 configuration refusal.
use super::*;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::ExperimentAssignment;
use orbit_types::assistance::AssistanceDisposition;
use orbit_types::workflow::CapturedExperiment;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const DELIVERY_QUALITY_JOIN_VERSION: &str = "delivery-quality-join/1";
pub const DELIVERY_QUALITY_SAMPLE_VERSION: &str = "delivery-quality-producer-sample/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualityChangeUnit {
    pub base_revision: String,
    pub candidate_revision: String,
    pub landed_revision: Option<String>,
    pub patch_digest: String,
}

/// Canonical field set from delivery_quality_sampling.py. Every unavailable
/// historical fact remains an explicit null, not current configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualityLadderJoin {
    pub owner: String,
    pub workspace: String,
    pub experiment_id: Option<String>,
    pub trial_id: Option<String>,
    pub assignment_arm: Option<String>,
    pub segment_digest: Option<String>,
    pub overlay_digest: Option<String>,
    pub excluded: Option<bool>,
    pub forced: Option<bool>,
    pub live: Option<bool>,
    pub task_id: String,
    pub implementation_attempt: Option<String>,
    pub change_unit: Option<DeliveryQualityChangeUnit>,
    pub admission_tier: Option<String>,
    pub admission_type: Option<String>,
    pub actual_run_id: Option<String>,
    pub actual_provider: Option<String>,
    pub actual_model: Option<String>,
    pub actual_effort: Option<String>,
    pub configuration_fingerprint: Option<String>,
    pub source_version: String,
    pub rubric_version: String,
    pub grader_version: String,
    pub builder_version: String,
    pub as_of: DateTime<Utc>,
    pub attribution: DeliveryQualityAttribution,
    pub non_code: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualityTreatment {
    pub actual_provider: String,
    pub actual_model: String,
    pub actual_effort: String,
    pub configuration_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualityLadderRecord {
    pub version: String,
    pub join: DeliveryQualityLadderJoin,
    pub actual_crew: Option<String>,
    pub exclusions: Vec<String>,
    pub assessment_id: String,
    pub applicability: BTreeMap<String, DeliveryQualityApplicability>,
    pub applicability_rule: String,
}

/// Match a stored assignment to the run's actual capture and dispatch. The
/// caller must supply the frozen trial/segment identity, not a current pool.
/// Hypothetical assignments remain inspectable but never treatment evidence.
#[allow(
    clippy::too_many_arguments,
    reason = "the read-only join binds explicit immutable assignment, capture and treatment evidence"
)]
pub fn delivery_quality_ladder_join(
    unit: &FrozenDeliveryQuality,
    profile: &DeliveryQualityProfile,
    assignment: Option<&ExperimentAssignment>,
    capture: Option<&CapturedExperiment>,
    trial_id: Option<String>,
    segment_digest: Option<String>,
    expected: Option<&DeliveryQualityTreatment>,
    as_of: DateTime<Utc>,
) -> Result<DeliveryQualityLadderRecord, OrbitError> {
    if unit.delivery_quality_unit_id()? != profile.unit_id
        || unit.identity != profile.identity
        || profile.frozen_at > as_of
    {
        return Err(OrbitError::InvalidInput(
            "delivery quality join identity or as-of mismatch".into(),
        ));
    }
    let mut exclusions = Vec::new();
    let matched = assignment.zip(capture).is_some_and(|(a, c)| {
        a.task_id == unit.identity.task_id
            && a.experiment == c.experiment
            && a.arm.as_deref() == Some(&c.arm)
            && a.segment == Some(c.segment)
            && a.digest.as_deref() == Some(&c.digest)
            && a.forced == c.forced
            && orbit_types::assistance::assistance_digest(c.overlay_json.as_bytes()) == c.digest
            && a.assigned_at <= as_of
    });
    if !matched {
        exclusions.push("mismatched_or_unavailable_treatment".into());
    }
    if let Some(a) = assignment {
        if let Some(reason) = &a.exclusion_reason {
            exclusions.push(
                match reason.as_str() {
                    "explicit_crew" => "explicit_crew",
                    "surface_conflict" => "surface_conflict",
                    _ => "assignment_excluded",
                }
                .into(),
            );
        }
        if a.forced {
            exclusions.push("forced".into());
        }
    }
    if capture.is_some_and(|c| !c.live) {
        exclusions.push("hypothetical".into());
    }
    let p = &unit.provenance;
    let actual_matches = expected.is_some_and(|e| {
        p.actual_provider.as_deref() == Some(&e.actual_provider)
            && p.actual_model.as_deref() == Some(&e.actual_model)
            && p.actual_effort.as_deref() == Some(&e.actual_effort)
            && p.configuration_fingerprint.as_deref() == Some(&e.configuration_fingerprint)
    });
    if !actual_matches {
        exclusions.push("actual_treatment_mismatch_or_unavailable".into());
    }
    if p.attribution != DeliveryQualityAttribution::Available {
        exclusions.push("unattributed".into());
    }
    if unit.non_code {
        exclusions.push("non_code".into());
    }
    if trial_id.is_none()
        || segment_digest.is_none()
        || p.admission_tier.is_none()
        || p.admission_type.is_none()
    {
        exclusions.push("capture_unavailable".into());
    }
    let grader_version = contract::quality_digest(&(&profile.policy_epoch, &profile.grader))?;
    let join = DeliveryQualityLadderJoin {
        owner: unit.identity.owner.clone(),
        workspace: unit.identity.workspace.clone(),
        experiment_id: assignment.map(|a| a.experiment.clone()),
        trial_id,
        assignment_arm: assignment.and_then(|a| a.arm.clone()),
        segment_digest,
        overlay_digest: capture.map(|c| c.digest.clone()),
        excluded: Some(!exclusions.is_empty()),
        forced: assignment.map(|a| a.forced),
        live: capture.map(|c| c.live),
        task_id: unit.identity.task_id.clone(),
        implementation_attempt: Some(unit.identity.implementation_attempt.clone()),
        change_unit: unit
            .identity
            .base_revision
            .clone()
            .zip(unit.identity.candidate_revision.clone())
            .map(
                |(base_revision, candidate_revision)| DeliveryQualityChangeUnit {
                    base_revision,
                    candidate_revision,
                    landed_revision: unit.identity.landed_revision.clone(),
                    patch_digest: orbit_types::assistance::assistance_digest(unit.patch.as_bytes()),
                },
            ),
        admission_tier: p.admission_tier.clone(),
        admission_type: p.admission_type.clone(),
        actual_run_id: Some(unit.identity.actual_run_id.clone()),
        actual_provider: p.actual_provider.clone(),
        actual_model: p.actual_model.clone(),
        actual_effort: p.actual_effort.clone(),
        configuration_fingerprint: p.configuration_fingerprint.clone(),
        source_version: unit.source_version.clone(),
        rubric_version: profile.rubric_version.clone(),
        grader_version,
        builder_version: profile.builder_version.clone(),
        as_of,
        attribution: p.attribution.clone(),
        non_code: Some(unit.non_code),
    };
    Ok(DeliveryQualityLadderRecord {
        version: DELIVERY_QUALITY_JOIN_VERSION.into(),
        join,
        actual_crew: p.actual_crew.clone(),
        exclusions,
        assessment_id: profile.assessment_id.clone(),
        applicability: unit.applicability.clone(),
        applicability_rule: unit.applicability_rule.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualityTrialGrant {
    pub owner: String,
    pub workspace: String,
    pub experiment_id: String,
    pub trial_id: String,
    pub source_version: String,
    pub rubric_version: String,
    pub grader_version: String,
    pub builder_version: String,
    /// Separately recorded use-specific adoption/evaluation identity. Generic
    /// grader presence or calibration is not a ladder authorization.
    pub adoption: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualitySampleRow {
    pub record: DeliveryQualityLadderRecord,
    pub selected: bool,
    pub eligible: bool,
    pub sampling_probability: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualitySample {
    pub version: String,
    pub cohort_id: String,
    pub epoch: String,
    pub eligibility_watermark: String,
    pub frozen_at: DateTime<Utc>,
    pub seed: String,
    pub algorithm: String,
    pub grant: DeliveryQualityTrialGrant,
    pub frame_digest: String,
    pub rows: Vec<DeliveryQualitySampleRow>,
    pub insufficient_controls: usize,
}
impl DeliveryQualityLadderJoin {
    fn task_key(&self) -> (String, String, String) {
        (
            self.owner.clone(),
            self.workspace.clone(),
            self.task_id.clone(),
        )
    }
    fn stratum(&self) -> Result<String, OrbitError> {
        contract::quality_digest(&(
            &self.owner,
            &self.workspace,
            &self.experiment_id,
            &self.trial_id,
            &self.segment_digest,
            &self.admission_tier,
            &self.admission_type,
            &self.source_version,
            &self.rubric_version,
            &self.grader_version,
            &self.builder_version,
        ))
    }
    fn selection_eligible(&self) -> bool {
        self.excluded == Some(false)
            && self.forced == Some(false)
            && self.live == Some(true)
            && self.non_code == Some(false)
            && matches!(
                self.assignment_arm.as_deref(),
                Some("candidate" | "control")
            )
    }
}

impl OrbitRuntime {
    /// Schedule a bounded page from a separately frozen trial-only frame.
    /// Unselected and out-of-trial units never reach the assistance queue.
    pub fn run_delivery_quality_sample(
        &self,
        service: &AssistanceService,
        sample: &DeliveryQualitySample,
        units: &[FrozenDeliveryQuality],
    ) -> Result<(), OrbitError> {
        if service
            .assistance_shadow_policy_epoch(&self.workspace_id()?, DELIVERY_QUALITY_USE_CASE)?
            .is_none()
        {
            return Ok(());
        }
        validate_quality_sample(sample)?;
        if units.len() > 32 {
            return Err(OrbitError::InvalidInput(
                "sample scheduling page exceeds bound".into(),
            ));
        }
        for unit in units {
            let selected = sample.rows.iter().find(|r| {
                r.record.join.task_key()
                    == (
                        unit.identity.owner.clone(),
                        unit.identity.workspace.clone(),
                        unit.identity.task_id.clone(),
                    )
            });
            let Some(row) = selected.filter(|r| r.selected && r.eligible) else {
                continue;
            };
            let id = self.capture_delivery_quality(unit, service, false)?;
            if id.as_deref() != Some(&row.record.assessment_id) {
                return Err(OrbitError::InvalidInput(
                    "sample assessment epoch changed".into(),
                ));
            }
            self.capture_delivery_quality(unit, service, true)?;
        }
        service.process_next_assistance_checked(
            &self.workspace_id()?,
            DELIVERY_QUALITY_USE_CASE,
            |request| {
                Ok(sample.rows.iter().any(|r| {
                    r.selected && r.record.assessment_id == request.evidence_packet.subject_id
                }))
            },
        )?;
        Ok(())
    }

    /// Persist selection before any grading. This API requires a separate
    /// trial-only host grant; it never scans ordinary workspace/control tasks.
    /// The caller supplies the enrolled frame with outcomes/grades omitted.
    #[allow(
        clippy::too_many_arguments,
        reason = "frozen sampling identities are explicit inputs rather than ambient mutable configuration"
    )]
    pub fn freeze_delivery_quality_sample(
        &self,
        grant: DeliveryQualityTrialGrant,
        cohort_id: &str,
        epoch: &str,
        watermark: &str,
        seed: &str,
        as_of: DateTime<Utc>,
        mut records: Vec<DeliveryQualityLadderRecord>,
    ) -> Result<DeliveryQualitySample, OrbitError> {
        if grant.workspace != self.workspace_id()?
            || grant.adoption.is_empty()
            || records.len() > 10_000
            || [cohort_id, epoch, watermark, seed]
                .iter()
                .any(|s| s.is_empty() || s.len() > 128)
        {
            return Err(OrbitError::InvalidInput(
                "trial grading grant or frame bounds invalid".into(),
            ));
        }
        records.sort_by_key(|r| r.join.task_key());
        let mut treatment_epochs = BTreeMap::new();
        let mut tasks = BTreeSet::new();
        let mut candidates: BTreeMap<String, usize> = BTreeMap::new();
        let mut controls: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        for (i, r) in records.iter().enumerate() {
            let j = &r.join;
            if r.version != DELIVERY_QUALITY_JOIN_VERSION
                || j.owner != grant.owner
                || j.workspace != grant.workspace
                || j.experiment_id.as_deref() != Some(&grant.experiment_id)
                || j.trial_id.as_deref() != Some(&grant.trial_id)
                || j.source_version != grant.source_version
                || j.rubric_version != grant.rubric_version
                || j.grader_version != grant.grader_version
                || j.builder_version != grant.builder_version
                || j.as_of > as_of
                || !tasks.insert(j.task_key())
            {
                return Err(OrbitError::InvalidInput(
                    "trial frame mixes identities, epochs or task votes".into(),
                ));
            }
            if !j.selection_eligible() {
                continue;
            }
            let treatment = contract::quality_digest(&(
                &j.actual_provider,
                &j.actual_model,
                &j.actual_effort,
                &j.configuration_fingerprint,
                &r.actual_crew,
            ))?;
            if treatment_epochs
                .insert(j.assignment_arm.clone(), treatment.clone())
                .is_some_and(|old| old != treatment)
            {
                return Err(OrbitError::InvalidInput(
                    "trial frame mixes actual treatment epochs".into(),
                ));
            }
            let stratum = j.stratum()?;
            if j.assignment_arm.as_deref() == Some("candidate") {
                *candidates.entry(stratum).or_default() += 1;
            } else {
                let hash = contract::quality_digest(&serde_json::json!([
                    "sha256-sort/1",
                    seed,
                    cohort_id,
                    epoch,
                    watermark,
                    j.task_key()
                ]))?;
                controls.entry(stratum).or_default().push((hash, i));
            }
        }
        let mut selected = BTreeSet::new();
        let mut probabilities = BTreeMap::new();
        let mut insufficient_controls = 0;
        for (stratum, n) in &candidates {
            let members = controls.entry(stratum.clone()).or_default();
            members.sort();
            let count = (*n).min(members.len());
            insufficient_controls += n - count;
            for (_, i) in members.iter() {
                probabilities.insert(
                    *i,
                    if members.is_empty() {
                        0.0
                    } else {
                        count as f64 / members.len() as f64
                    },
                );
            }
            selected.extend(members.iter().take(count).map(|(_, i)| *i));
        }
        let rows: Vec<_> = records
            .into_iter()
            .enumerate()
            .map(|(i, record)| {
                let eligible = record.join.selection_eligible();
                let candidate = record.join.assignment_arm.as_deref() == Some("candidate");
                DeliveryQualitySampleRow {
                    selected: eligible && (candidate || selected.contains(&i)),
                    sampling_probability: if eligible {
                        Some(if candidate {
                            1.0
                        } else {
                            *probabilities.get(&i).unwrap_or(&0.0)
                        })
                    } else {
                        None
                    },
                    eligible,
                    record,
                }
            })
            .collect();
        let frame_digest = contract::quality_digest(&(
            &grant,
            cohort_id,
            epoch,
            watermark,
            seed,
            as_of,
            &rows,
            insufficient_controls,
        ))?;
        let sample = DeliveryQualitySample {
            version: DELIVERY_QUALITY_SAMPLE_VERSION.into(),
            cohort_id: cohort_id.into(),
            epoch: epoch.into(),
            eligibility_watermark: watermark.into(),
            frozen_at: as_of,
            seed: seed.into(),
            algorithm: "sha256-sort/1".into(),
            grant,
            frame_digest,
            rows,
            insufficient_controls,
        };
        let key = contract::quality_digest(&(cohort_id, epoch))?;
        let path = ledger::quality_root(self)?.join(format!("sample-{}.json", &key[7..]));
        ledger::quality_locked(&path, || {
            if let Some(previous) = ledger::quality_read::<DeliveryQualitySample>(&path)? {
                if previous.frame_digest != sample.frame_digest {
                    return Err(OrbitError::InvalidInput(
                        "frozen sample changed; create a new cohort epoch".into(),
                    ));
                }
                return Ok(previous);
            }
            ledger::quality_write(&path, &sample)?;
            Ok(sample)
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeliveryQualityDimensionCounts {
    pub enrolled: usize,
    pub selected: usize,
    pub unsampled: usize,
    pub applicable: usize,
    pub valid_grade: usize,
    pub weak_deficient: usize,
    pub gaps: BTreeMap<String, usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryQualityComparisonSnapshot {
    pub version: String,
    pub cohort_id: String,
    pub epoch: String,
    pub frame_digest: String,
    pub as_of: DateTime<Utc>,
    pub dimensions: BTreeMap<String, BTreeMap<String, DeliveryQualityDimensionCounts>>,
}

/// Per-arm per-dimension counts over the frozen selection, with no aggregate
/// grade. A sample-bound assessment is the only vote: no best retry, duplicate
/// task, shared patch, subunit average or changed epoch can enter the counts.
pub fn delivery_quality_comparison_snapshot(
    service: &AssistanceService,
    sample: &DeliveryQualitySample,
    as_of: DateTime<Utc>,
    profiles: &[DeliveryQualityProfile],
) -> Result<DeliveryQualityComparisonSnapshot, OrbitError> {
    validate_quality_sample(sample)?;
    let epoch = service
        .assistance_shadow_policy_epoch(&sample.grant.workspace, DELIVERY_QUALITY_USE_CASE)?;
    if epoch.is_none()
        || contract::quality_digest(&(&epoch, Some(service.assistance_adapter_revision())))?
            != sample.grant.grader_version
    {
        return Err(OrbitError::InvalidInput(
            "trial grading policy is disabled or changed".into(),
        ));
    }
    if sample.frozen_at > as_of {
        return Err(OrbitError::InvalidInput(
            "comparison predates frozen sample".into(),
        ));
    }
    let mut by_id = BTreeMap::new();
    for p in profiles {
        if by_id.insert(&p.assessment_id, p).is_some() {
            return Err(OrbitError::InvalidInput("duplicate assessment vote".into()));
        }
    }
    let mut patch_counts = BTreeMap::new();
    for row in &sample.rows {
        if let Some(unit) = &row.record.join.change_unit {
            *patch_counts
                .entry(contract::quality_digest(unit)?)
                .or_insert(0) += 1;
        }
    }
    let mut dimensions: BTreeMap<String, BTreeMap<String, DeliveryQualityDimensionCounts>> =
        BTreeMap::new();
    for row in &sample.rows {
        let j = &row.record.join;
        let arm = j
            .assignment_arm
            .clone()
            .unwrap_or_else(|| "unassigned".into());
        let profile = by_id.get(&row.record.assessment_id);
        for d in DELIVERY_QUALITY_DIMENSIONS {
            let c = dimensions
                .entry(arm.clone())
                .or_default()
                .entry(d.into())
                .or_default();
            c.enrolled += 1;
            if !row.eligible {
                for reason in &row.record.exclusions {
                    *c.gaps.entry(reason.clone()).or_default() += 1;
                }
                if row.record.exclusions.is_empty() {
                    *c.gaps.entry("ineligible".into()).or_default() += 1;
                }
                continue;
            }
            if !row.selected {
                c.unsampled += 1;
                continue;
            }
            c.selected += 1;
            match row.record.applicability.get(d) {
                Some(DeliveryQualityApplicability::NotApplicable) => {
                    *c.gaps.entry("not_applicable".into()).or_default() += 1;
                    continue;
                }
                Some(DeliveryQualityApplicability::Applicable)
                    if !row.record.applicability_rule.is_empty() => {}
                _ => {
                    *c.gaps
                        .entry("applicability_unavailable".into())
                        .or_default() += 1;
                    continue;
                }
            }
            let Some(p) = profile else {
                c.applicable += 1;
                *c.gaps.entry("missing".into()).or_default() += 1;
                continue;
            };
            let state = if p.frozen_at > as_of
                || p.identity.owner != j.owner
                || p.identity.workspace != j.workspace
                || p.provenance.actual_provider != j.actual_provider
                || p.provenance.actual_model != j.actual_model
                || p.provenance.actual_effort != j.actual_effort
                || p.provenance.configuration_fingerprint != j.configuration_fingerprint
                || p.identity.task_id != j.task_id
                || p.identity.actual_run_id != j.actual_run_id.clone().unwrap_or_default()
                || p.rubric_version != j.rubric_version
                || p.builder_version != j.builder_version
                || contract::quality_digest(&(&p.policy_epoch, &p.grader))? != j.grader_version
            {
                Some("stale_or_mixed_epoch")
            } else if j
                .change_unit
                .as_ref()
                .and_then(|u| contract::quality_digest(u).ok())
                .and_then(|key| patch_counts.get(&key))
                .is_some_and(|count| *count > 1)
                || j.attribution != DeliveryQualityAttribution::Available
                || p.provenance.attribution != DeliveryQualityAttribution::Available
            {
                Some("unattributed")
            } else if p.gaps.iter().any(|gap| gap == "pending") {
                Some("pending")
            } else if !p.gaps.is_empty() {
                Some("partial")
            } else {
                None
            };
            if let Some(state) = state {
                c.applicable += 1;
                *c.gaps.entry(state.into()).or_default() += 1;
                continue;
            }
            let Some(value) = p.dimensions.get(d) else {
                c.applicable += 1;
                *c.gaps.entry("partial".into()).or_default() += 1;
                continue;
            };
            if value.applicability == DeliveryQualityApplicability::NotApplicable {
                c.applicable += 1;
                *c.gaps
                    .entry("unapproved_not_applicable".into())
                    .or_default() += 1;
                continue;
            }
            c.applicable += 1;
            if value.applicability == DeliveryQualityApplicability::Unavailable {
                *c.gaps
                    .entry("applicability_unavailable".into())
                    .or_default() += 1;
                continue;
            }
            match (value.ordinal, value.disposition) {
                (Some(grade @ 1..=5), Some(AssistanceDisposition::Suggested))
                    if p.result.as_ref().is_some_and(|r| {
                        r.disposition == AssistanceDisposition::Suggested
                            && r.reason == AssistanceReason::Complete
                    }) && value.choice.as_deref()
                        == Some(DELIVERY_QUALITY_GRADES[grade as usize - 1]) =>
                {
                    c.valid_grade += 1;
                    c.weak_deficient += usize::from(grade <= 2);
                }
                _ => {
                    let state = p
                        .result
                        .as_ref()
                        .map(|r| {
                            if r.reason == AssistanceReason::Complete {
                                format!("{:?}", r.disposition).to_ascii_lowercase()
                            } else {
                                serde_json::to_string(&r.reason)
                                    .unwrap_or_else(|_| "unavailable".into())
                                    .trim_matches('"')
                                    .to_owned()
                            }
                        })
                        .unwrap_or_else(|| "pending".into());
                    *c.gaps.entry(state).or_default() += 1;
                }
            }
        }
    }
    if service.assistance_shadow_policy_epoch(&sample.grant.workspace, DELIVERY_QUALITY_USE_CASE)?
        != epoch
    {
        return Err(OrbitError::InvalidInput(
            "trial grading policy changed during snapshot".into(),
        ));
    }
    Ok(DeliveryQualityComparisonSnapshot {
        version: "delivery-quality-comparison/1".into(),
        cohort_id: sample.cohort_id.clone(),
        epoch: sample.epoch.clone(),
        frame_digest: sample.frame_digest.clone(),
        as_of,
        dimensions,
    })
}

fn validate_quality_sample(sample: &DeliveryQualitySample) -> Result<(), OrbitError> {
    let expected = contract::quality_digest(&(
        &sample.grant,
        &sample.cohort_id,
        &sample.epoch,
        &sample.eligibility_watermark,
        &sample.seed,
        sample.frozen_at,
        &sample.rows,
        sample.insufficient_controls,
    ))?;
    if sample.version != DELIVERY_QUALITY_SAMPLE_VERSION
        || sample.algorithm != "sha256-sort/1"
        || sample.frame_digest != expected
    {
        return Err(OrbitError::InvalidInput(
            "invalid frozen trial sample digest or version".into(),
        ));
    }
    Ok(())
}
