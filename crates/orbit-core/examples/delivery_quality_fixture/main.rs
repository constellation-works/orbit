//! Isolated manual recorded-response fixture. Synthetic replies prove plumbing,
//! not model accuracy, calibrated confidence or crew-ladder adoption.
#![allow(
    clippy::unwrap_used,
    reason = "isolated manual fixture assertions fail immediately on invalid synthetic state"
)]
use chrono::Utc;
use orbit_config::assistance::{AssistancePolicy, AssistanceSettings};
use orbit_core::{
    OrbitRuntime,
    application::{
        assistance::{AssistanceAdapter, AssistanceService},
        delivery_quality::{ladder::*, *},
        experiment::{ExperimentActor, ExperimentDefinitionInput},
        task::{TaskAddParams, TaskUpdateParams},
    },
};
use orbit_store::contracts::{
    ExperimentAssignment, ExperimentStoreBackend, assistance::AssistanceClaim,
};
use orbit_types::{
    assistance::{
        self, AssistanceAnswer, AssistanceDisposition, AssistanceMode, AssistanceProbability,
        AssistanceReason, AssistanceRequest, AssistanceResult, AssistanceValue,
    },
    workflow::CapturedExperiment,
};
use serde_json::json;
use std::{
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
type FixtureResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct RecordedAdapter {
    calls: AtomicUsize,
    reply: Mutex<AssistanceReason>,
    settings: Arc<Mutex<AssistanceSettings>>,
    switch_off: Mutex<bool>,
    requests: Mutex<Vec<AssistanceRequest>>,
}
impl AssistanceAdapter for RecordedAdapter {
    fn assistance_adapter_identity(&self) -> &str {
        "delivery-quality-recorded/1"
    }
    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        secrets: &[String],
        _: &AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason> {
        serde_json::from_value(
            orbit_common::security::redaction::redact_secret_values_json(
                serde_json::to_value(request).map_err(|_| AssistanceReason::InvalidRequest)?,
                secrets,
            ),
        )
        .map_err(|_| AssistanceReason::InvalidRequest)
    }
    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        _: &AssistancePolicy,
    ) -> AssistanceResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap()
            .push(claim.work.request.clone());
        if *self.switch_off.lock().unwrap() {
            self.settings.lock().unwrap().policies[0].mode = AssistanceMode::Off;
        }
        let reason = *self.reply.lock().unwrap();
        if reason != AssistanceReason::Complete {
            return AssistanceResult::assistance_gap(reason);
        }
        let mut result = AssistanceResult::assistance_gap(AssistanceReason::Complete);
        result.disposition = AssistanceDisposition::Suggested;
        result.answers = claim
            .work
            .request
            .contract
            .questions
            .iter()
            .map(|(name, q)| {
                let assistance::AssistanceQuestion::Choice { allowed, .. } = q else {
                    unreachable!()
                };
                (
                    name.clone(),
                    AssistanceAnswer {
                        value: AssistanceValue::Choice("adequate".into()),
                        probability: Some(1.0),
                        probabilities: allowed
                            .iter()
                            .map(|choice| AssistanceProbability {
                                value: choice.clone(),
                                probability: f64::from(choice == "adequate"),
                            })
                            .collect(),
                        criterion_refs: Vec::new(),
                    },
                )
            })
            .collect();
        result
    }
}
struct Fixture {
    runtime: OrbitRuntime,
    service: AssistanceService,
    adapter: Arc<RecordedAdapter>,
    settings: Arc<Mutex<AssistanceSettings>>,
}
impl Fixture {
    fn open(root: &Path) -> FixtureResult<Self> {
        std::fs::create_dir_all(root.join("global"))?;
        std::fs::create_dir_all(root.join("repo/.orbit"))?;
        git2::Repository::init(root.join("repo"))?;
        let runtime = OrbitRuntime::from_roots(&root.join("global"), &root.join("repo/.orbit"))?;
        let mut settings = AssistanceSettings::default();
        settings.budget.epoch = "recorded-fixture/1".into();
        settings.budget.token_ceiling = 10_000_000;
        settings.budget.spend_ceiling_microusd = 10_000_000;
        settings.budget.input_microusd_per_million_tokens = Some(1);
        settings.budget.pricing_version = Some("synthetic-price/1".into());
        settings.limits.max_attempts = 1;
        settings.limits.max_request_bytes = 65_536;
        settings.policies.push(AssistancePolicy {
            workspace_id: runtime.workspace_id()?,
            use_case: DELIVERY_QUALITY_USE_CASE.into(),
            mode: AssistanceMode::Off,
            model: "gpt-6-luna".into(),
            model_revision: "recorded/1".into(),
            policy_version: "synthetic/1".into(),
            contract_digest: assistance::assistance_digest(&serde_json::to_vec(
                &delivery_quality_question_template(),
            )?),
            evidence_fields: [
                "task_scope",
                "change",
                "guides",
                "context",
                "validations",
                "coverage",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            evidence_ref_prefixes: vec!["guide:".into(), "context:".into(), "validation:".into()],
            credential_env: "ORBIT_ASSISTANCE_FIXTURE_UNUSED".into(),
        });
        let settings = Arc::new(Mutex::new(settings));
        let adapter = Arc::new(RecordedAdapter {
            calls: AtomicUsize::new(0),
            reply: Mutex::new(AssistanceReason::Complete),
            settings: settings.clone(),
            switch_off: Mutex::new(false),
            requests: Mutex::new(Vec::new()),
        });
        let loader = settings.clone();
        let service = AssistanceService::new(
            orbit_store::compose::assistance_store_from_store(runtime.sqlite_store()?),
            adapter.clone(),
            Arc::new(move || Ok(loader.lock().unwrap().clone())),
        );
        Ok(Self {
            runtime,
            service,
            adapter,
            settings,
        })
    }
    fn shadow(&self) {
        self.settings.lock().unwrap().policies[0].mode = AssistanceMode::Shadow;
    }
    fn unit(&self, root: &Path) -> FixtureResult<FrozenDeliveryQuality> {
        let task = self.runtime.add_task(TaskAddParams {
            title: "Preserve empty input".into(),
            description: "Return None on empty input; preserve inherited helper behavior.".into(),
            acceptance_criteria: vec!["Empty input returns None.".into()],
            ..Default::default()
        })?;
        let repo = git2::Repository::init(root.join("change"))?;
        let sig = git2::Signature::now("fixture", "fixture@example.invalid")?;
        let old =
            repo.blob(b"pub fn first(v: &[u8]) -> u8 { v[0] }\n// inherited unrelated defect\n")?;
        let mut builder = repo.treebuilder(None)?;
        builder.insert("lib.rs", old, 0o100644)?;
        let old_tree = repo.find_tree(builder.write()?)?;
        let base = repo.commit(None, &sig, &sig, "base", &old_tree, &[])?;
        let new=repo.blob(b"pub fn first(v: &[u8]) -> Option<u8> { v.first().copied() }\n// inherited unrelated defect\n")?;
        builder.insert("lib.rs", new, 0o100644)?;
        let new_tree = repo.find_tree(builder.write()?)?;
        let candidate = repo.commit(
            None,
            &sig,
            &sig,
            "candidate",
            &new_tree,
            &[&repo.find_commit(base)?],
        )?;
        let diff = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)?;
        let mut patch = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                patch.push(line.origin() as u8);
            }
            patch.extend_from_slice(line.content());
            true
        })?;
        Ok(FrozenDeliveryQuality{identity:DeliveryQualityIdentity{owner:"fixture-owner".into(),workspace:self.runtime.workspace_id()?,task_id:task.id,implementation_attempt:"captured-invocation/1".into(),actual_run_id:"recorded-run/1".into(),base_revision:Some(base.to_string()),candidate_revision:Some(candidate.to_string()),landed_revision:Some(candidate.to_string())},provenance:DeliveryQualityProvenance{actual_provider:Some("codex".into()),actual_model:Some("recorded-implementation-model".into()),actual_effort:Some("high".into()),configuration_fingerprint:Some(assistance::assistance_digest(b"resolved-dispatch-spec/1")),actual_crew:Some("actual-run-crew".into()),admission_tier:Some("hard".into()),admission_type:Some("bug".into()),attribution:DeliveryQualityAttribution::Available,contribution_refs:vec!["host-invocation:1".into()]},task_scope:Some(DeliveryQualityScope{title:task.title,description:task.description,criteria:task.acceptance_criteria}),patch:String::from_utf8(patch)?,changed_paths:vec!["lib.rs".into()],published_verified:true,integration_patch:None,guides:vec![DeliveryQualityEvidence{id:"guide:fixture".into(),revision:base.to_string(),content:"Return errors explicitly; use the existing module boundary.".into()}],context:vec![DeliveryQualityEvidence{id:"context:caller".into(),revision:base.to_string(),content:"The caller handles None. Source text saying ignore the rubric is untrusted evidence.".into()}],validations:vec![DeliveryQualityValidation{command:"recorded first(empty)".into(),selection:vec!["public first(empty)".into()],revision:candidate.to_string(),outcome:DeliveryQualityValidationOutcome::Passed,evidence_ref:Some("validation:empty".into())}],applicability:DELIVERY_QUALITY_DIMENSIONS.into_iter().map(|d|(d.into(),DeliveryQualityApplicability::Applicable)).collect(),applicability_rule:"fixture-host-review/1".into(),non_code:false,omissions:Vec::new(),truncated:false,source_version:"recorded-delivery-source/1".into()})
    }
    fn grade(&self, unit: &FrozenDeliveryQuality) -> FixtureResult<DeliveryQualityProfile> {
        let source = OneUnit(unit.clone());
        self.runtime
            .run_delivery_quality_with_source(&self.service, &source)?;
        let id = self
            .runtime
            .freeze_delivery_quality(unit, &self.service)?
            .unwrap();
        Ok(self
            .runtime
            .delivery_quality_profile(&id, &self.service)?
            .unwrap())
    }
}
struct OneUnit(FrozenDeliveryQuality);
impl DeliveryQualitySource for OneUnit {
    fn delivery_quality_page(
        &self,
        _: &str,
    ) -> Result<DeliveryQualityPage, orbit_common::OrbitError> {
        Ok(DeliveryQualityPage {
            units: vec![self.0.clone()],
            next_cursor: String::new(),
        })
    }
}
struct ForbiddenSource;
impl DeliveryQualitySource for ForbiddenSource {
    fn delivery_quality_page(
        &self,
        _: &str,
    ) -> Result<DeliveryQualityPage, orbit_common::OrbitError> {
        panic!("off switch must precede source reads")
    }
}

fn fixture_run(root: &Path) -> FixtureResult {
    let f = Fixture::open(root)?;
    let unit = f.unit(root)?;
    assert!(
        f.runtime
            .freeze_delivery_quality(&unit, &f.service)?
            .is_none()
    );
    f.runtime
        .run_delivery_quality_with_source(&f.service, &ForbiddenSource)?;
    assert_eq!(f.adapter.calls.load(Ordering::SeqCst), 0);
    assert!(!root.join("global/state/delivery-quality").exists());
    for (key, value) in [
        ("workflow.crew_ladder.delivery_quality", "shadow"),
        ("workflow.crew_ladder.delivery_quality", "guardrail"),
        ("workflow.crew_ladder.mode", "propose"),
        ("workflow.crew_ladder.mode", "auto"),
    ] {
        let mut config = orbit_config::ConfigStore::open(
            orbit_config::ConfigScope::Global,
            root.join("config-refusals.toml"),
        )?;
        assert!(config.set_value(key, value).is_err() || config.snapshot().is_err());
    }
    f.shadow();
    let profile = f.grade(&unit)?;
    assert_eq!(profile.dimensions.len(), 6);
    assert!(
        profile.dimensions.values().all(
            |d| d.ordinal == Some(3) && d.disposition == Some(AssistanceDisposition::Uncertain)
        )
    );
    assert!(profile.gaps.is_empty());
    // Mutable task metadata is never the source of execution attribution.
    f.runtime.update_task_with_identity(
        &unit.identity.task_id,
        TaskUpdateParams {
            crew: Some(None),
            ..Default::default()
        },
        None,
        None,
    )?;
    let again = f.grade(&unit)?;
    assert_eq!(again.assessment_id, profile.assessment_id);
    assert_eq!(
        again.provenance.actual_crew.as_deref(),
        Some("actual-run-crew")
    );
    assert_eq!(f.adapter.calls.load(Ordering::SeqCst), 1);
    let reopened = OrbitRuntime::from_roots(&root.join("global"), &root.join("repo/.orbit"))?;
    reopened.run_delivery_quality_with_source(&f.service, &OneUnit(unit.clone()))?;
    assert_eq!(f.adapter.calls.load(Ordering::SeqCst), 1);
    let mut fix = unit.clone();
    fix.identity.implementation_attempt = "later-fix/1".into();
    fix.identity.actual_run_id = "later-run/1".into();
    fix.identity.base_revision = unit.identity.candidate_revision.clone();
    fix.identity.candidate_revision =
        Some(assistance::assistance_digest(b"later-revision")[7..47].into());
    let fixed = f.grade(&fix)?;
    assert_ne!(fixed.unit_id, profile.unit_id);
    let mut regrade = unit.clone();
    regrade.source_version = "recorded-delivery-source/2".into();
    assert_ne!(f.grade(&regrade)?.assessment_id, profile.assessment_id);
    for (name, edit) in [
        ("missing_scope", 0),
        ("unavailable_validation", 1),
        ("oversize", 2),
        ("truncated", 3),
        ("redacted", 4),
        ("incomplete_partition", 5),
        ("non_code", 6),
        ("reviewer", 7),
        ("shared", 8),
    ] {
        let mut u = unit.clone();
        u.identity.implementation_attempt = name.into();
        match edit {
            0 => u.task_scope = None,
            1 => u.validations[0].outcome = DeliveryQualityValidationOutcome::Unavailable,
            2 => u.patch = "x".repeat(70_000),
            3 => u.truncated = true,
            4 => {
                u.context[0].content =
                    "token=sk-proj-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
                        .into()
            }
            5 => u.omissions.push("global_partition:ungraded-file.rs".into()),
            6 => u.non_code = true,
            7 => {
                u.provenance.attribution = DeliveryQualityAttribution::Partial;
                u.integration_patch = Some("reviewer repair".into());
                u.provenance.contribution_refs.push("reviewer:1".into());
            }
            _ => u.provenance.attribution = DeliveryQualityAttribution::Shared,
        };
        let p = f.grade(&u)?;
        if edit <= 5 {
            assert!(!p.gaps.is_empty(), "{name}");
            assert!(p.dimensions.values().all(|d| d.ordinal.is_none()), "{name}");
        }
        if edit == 6 {
            assert!(p.non_code);
            assert!(p.result.is_none());
        }
        if edit >= 7 {
            assert_ne!(
                p.provenance.attribution,
                DeliveryQualityAttribution::Available
            );
        }
    }
    for reason in [
        AssistanceReason::Refused,
        AssistanceReason::Unavailable,
        AssistanceReason::Malformed,
        AssistanceReason::TimeoutUnknown,
        AssistanceReason::CredentialUnavailable,
    ] {
        let mut u = unit.clone();
        u.identity.implementation_attempt = format!("reason/{reason:?}");
        *f.adapter.reply.lock().unwrap() = reason;
        let p = f.grade(&u)?;
        assert_eq!(p.result.unwrap().reason, reason);
    }
    *f.adapter.reply.lock().unwrap() = AssistanceReason::Complete;
    // A stored assignment and the exact run capture are joined independently.
    let experiment = f.runtime.create_experiment(
        "fixture-delivery-trial",
        ExperimentDefinitionInput::default(),
        ExperimentActor {
            actor: "fixture".into(),
            reason: Some("isolated mechanics".into()),
        },
    )?;
    let mut enrollment = Vec::new();
    let mut units = Vec::new();
    let mut synthetic_profiles = Vec::new();
    let expected = DeliveryQualityTreatment {
        actual_provider: unit.provenance.actual_provider.clone().unwrap(),
        actual_model: unit.provenance.actual_model.clone().unwrap(),
        actual_effort: unit.provenance.actual_effort.clone().unwrap(),
        configuration_fingerprint: unit.provenance.configuration_fingerprint.clone().unwrap(),
    };
    for i in 0..100 {
        let mut u = unit.clone();
        u.identity.task_id = format!("fixture-sample-{i}");
        u.identity.implementation_attempt = format!("sample-attempt/{i}");
        u.identity.actual_run_id = format!("sample-run/{i}");
        u.identity.candidate_revision = Some(format!("{i:040x}"));
        u.validations[0].revision = u.identity.candidate_revision.clone().unwrap();
        u.patch.push_str(&format!("\n// task {i}"));
        let id = f.runtime.freeze_delivery_quality(&u, &f.service)?.unwrap();
        let pending = f
            .runtime
            .delivery_quality_profile(&id, &f.service)?
            .unwrap();
        assert!(pending.result.is_none());
        let arm = if i < 10 { "candidate" } else { "control" };
        let digest = assistance::assistance_digest(b"{}");
        let assignment = ExperimentAssignment {
            experiment: experiment.id.clone(),
            task_id: u.identity.task_id.clone(),
            arm: Some(arm.into()),
            segment: Some(1),
            digest: Some(digest.clone()),
            assigned_at: Utc::now(),
            forced: false,
            exclusion_reason: None,
        };
        f.runtime.sqlite_store()?.assign_experiments(
            &f.runtime.workspace_id()?,
            &u.identity.task_id,
            true,
            true,
            &|_, _, _| Ok(vec![assignment.clone()]),
        )?;
        let stored = f
            .runtime
            .task_experiments(&u.identity.task_id)?
            .pop()
            .unwrap();
        let capture = CapturedExperiment {
            experiment: stored.experiment.clone(),
            arm: arm.into(),
            segment: 1,
            forced: false,
            digest,
            overlay_json: "{}".into(),
            live: true,
            instructions: Vec::new(),
            skills: Vec::new(),
        };
        let as_of = Utc::now();
        let record = delivery_quality_ladder_join(
            &u,
            &pending,
            Some(&stored),
            Some(&capture),
            Some("trial/1".into()),
            Some(assistance::assistance_digest(b"segment/1")),
            Some(&expected),
            as_of,
        )?;
        assert!(record.exclusions.is_empty());
        if i == 0 {
            let mut hypothetical = capture.clone();
            hypothetical.live = false;
            let excluded = delivery_quality_ladder_join(
                &u,
                &pending,
                Some(&stored),
                Some(&hypothetical),
                Some("trial/1".into()),
                Some(assistance::assistance_digest(b"segment/1")),
                Some(&expected),
                as_of,
            )?;
            assert!(excluded.exclusions.contains(&"hypothetical".into()));
            let mut forced = stored.clone();
            forced.forced = true;
            assert!(
                !delivery_quality_ladder_join(
                    &u,
                    &pending,
                    Some(&forced),
                    Some(&capture),
                    None,
                    None,
                    Some(&expected),
                    as_of
                )?
                .exclusions
                .is_empty()
            );
            let mut conflict = stored.clone();
            conflict.exclusion_reason = Some("surface_conflict".into());
            assert!(
                delivery_quality_ladder_join(
                    &u,
                    &pending,
                    Some(&conflict),
                    Some(&capture),
                    None,
                    None,
                    None,
                    as_of
                )?
                .exclusions
                .contains(&"surface_conflict".into())
            );
        }
        // Synthetic valid-grade readout data, explicitly distinct from the
        // uncalibrated live template. This exercises denominator mechanics.
        let mut valid = pending;
        valid.gaps.clear();
        valid.result = profile.result.clone();
        valid.result.as_mut().unwrap().disposition = AssistanceDisposition::Suggested;
        let (grade, choice) = if i == 0 {
            (1, "deficient")
        } else if i == 1 {
            (2, "weak")
        } else {
            (3, "adequate")
        };
        for answer in valid.result.as_mut().unwrap().answers.values_mut() {
            answer.value = AssistanceValue::Choice(choice.into());
            for p in &mut answer.probabilities {
                p.probability = f64::from(p.value == choice);
            }
        }

        valid.dimensions = DELIVERY_QUALITY_DIMENSIONS
            .into_iter()
            .map(|d| {
                (
                    d.into(),
                    DeliveryQualityDimension {
                        ordinal: Some(grade),
                        choice: Some(choice.into()),
                        disposition: Some(AssistanceDisposition::Suggested),
                        applicability: DeliveryQualityApplicability::Applicable,
                    },
                )
            })
            .collect();
        synthetic_profiles.push(valid);
        enrollment.push(record);
        units.push(u);
    }
    let as_of = Utc::now();
    let first = &enrollment[0].join;
    let grant = DeliveryQualityTrialGrant {
        owner: first.owner.clone(),
        workspace: first.workspace.clone(),
        experiment_id: experiment.id,
        trial_id: "trial/1".into(),
        source_version: first.source_version.clone(),
        rubric_version: first.rubric_version.clone(),
        grader_version: first.grader_version.clone(),
        builder_version: first.builder_version.clone(),
        adoption: "fixture-mechanics-only-not-live-adoption".into(),
    };
    let frame = f.runtime.freeze_delivery_quality_sample(
        grant.clone(),
        "fixture-cohort/1",
        "epoch/1",
        "watermark/100",
        "seed/42",
        as_of,
        enrollment.clone(),
    )?;
    assert_eq!(frame.rows.iter().filter(|r| r.selected).count(), 20);
    assert_eq!(frame.insufficient_controls, 0);
    let replay = reopened.freeze_delivery_quality_sample(
        grant.clone(),
        "fixture-cohort/1",
        "epoch/1",
        "watermark/100",
        "seed/42",
        as_of,
        enrollment.clone(),
    )?;
    assert_eq!(frame.frame_digest, replay.frame_digest);
    assert!(
        f.runtime
            .freeze_delivery_quality_sample(
                grant.clone(),
                "fixture-cohort/1",
                "epoch/1",
                "watermark/100",
                "different-seed",
                as_of,
                enrollment.clone()
            )
            .is_err()
    );
    let snapshot =
        delivery_quality_comparison_snapshot(&f.service, &frame, as_of, &synthetic_profiles)?;
    for d in DELIVERY_QUALITY_DIMENSIONS {
        let candidate = &snapshot.dimensions["candidate"][d];
        let control = &snapshot.dimensions["control"][d];
        assert_eq!(
            (
                candidate.valid_grade,
                candidate.selected,
                candidate.weak_deficient
            ),
            (10, 10, 2)
        );
        assert_eq!(
            (control.valid_grade, control.selected, control.unsampled),
            (10, 10, 80)
        );
    }
    let missing = frame
        .rows
        .iter()
        .find(|r| r.selected)
        .unwrap()
        .record
        .assessment_id
        .clone();
    for unattributed in [false, true] {
        let mut degraded = synthetic_profiles.clone();
        let selected = degraded
            .iter_mut()
            .find(|p| p.assessment_id == missing)
            .unwrap();
        if unattributed {
            selected.provenance.attribution = DeliveryQualityAttribution::Shared;
        } else {
            selected.gaps.push("fixture_partial_evidence".into());
        }
        let reduced = delivery_quality_comparison_snapshot(&f.service, &frame, as_of, &degraded)?;
        let gap = if unattributed {
            "unattributed"
        } else {
            "partial"
        };
        for dimension in DELIVERY_QUALITY_DIMENSIONS {
            assert_eq!(
                reduced
                    .dimensions
                    .values()
                    .map(|dims| dims[dimension].valid_grade)
                    .sum::<usize>(),
                19
            );
            assert_eq!(
                reduced
                    .dimensions
                    .values()
                    .map(|dims| *dims[dimension].gaps.get(gap).unwrap_or(&0))
                    .sum::<usize>(),
                1
            );
        }
    }
    synthetic_profiles.retain(|p| p.assessment_id != missing);
    let partial =
        delivery_quality_comparison_snapshot(&f.service, &frame, as_of, &synthetic_profiles)?;
    assert!(
        partial
            .dimensions
            .values()
            .any(|dims| dims.values().any(|c| c.valid_grade < c.selected))
    );
    let shortage = f.runtime.freeze_delivery_quality_sample(
        grant,
        "short-cohort/1",
        "epoch/1",
        "watermark/12",
        "seed/42",
        as_of,
        enrollment.into_iter().take(12).collect(),
    )?;
    assert_eq!(shortage.insufficient_controls, 8);
    // Schedule only selected identities, with zero ordinary control work.
    let before = f.adapter.calls.load(Ordering::SeqCst);
    for chunk in units.chunks(25) {
        f.runtime
            .run_delivery_quality_sample(&f.service, &frame, chunk)?;
    }
    assert!(f.adapter.calls.load(Ordering::SeqCst) <= before + 4);
    for _ in 0..20 {
        f.runtime
            .run_delivery_quality_sample(&f.service, &frame, &[])?;
    }
    assert_eq!(f.adapter.calls.load(Ordering::SeqCst), before + 20);
    *f.adapter.switch_off.lock().unwrap() = true;
    let mut late = unit.clone();
    late.identity.implementation_attempt = "late-off/1".into();
    f.runtime
        .run_delivery_quality_with_source(&f.service, &OneUnit(late.clone()))?;
    assert!(
        f.runtime
            .freeze_delivery_quality(&late, &f.service)?
            .is_none()
    );
    assert!(
        f.runtime
            .delivery_quality_profile(&profile.assessment_id, &f.service)?
            .is_none()
    );
    assert!(
        delivery_quality_comparison_snapshot(&f.service, &frame, as_of, &synthetic_profiles)
            .is_err()
    );
    f.runtime
        .run_delivery_quality_with_source(&f.service, &ForbiddenSource)?;
    f.shadow();
    let late_id = f
        .runtime
        .freeze_delivery_quality(&late, &f.service)?
        .unwrap();
    let late_profile = f
        .runtime
        .delivery_quality_profile(&late_id, &f.service)?
        .unwrap();
    assert_eq!(
        late_profile.result.unwrap().disposition,
        AssistanceDisposition::Stale
    );
    let requests = f.adapter.requests.lock().unwrap();
    let text = serde_json::to_string(&*requests)?;
    assert!(!text.contains("recorded-implementation-model"));
    assert!(!text.contains("actual-run-crew"));
    assert!(!text.contains("later regression"));
    std::fs::write(
        root.join("comparison.json"),
        serde_json::to_vec_pretty(&snapshot)?,
    )?;
    std::io::stdout().write_all(&serde_json::to_vec_pretty(&json!({"status":"passed","fixture":"delivery-quality-recorded/1","profile":profile,"sample":{"frame_digest":frame.frame_digest,"selected":20,"unsampled_controls":80},"checks":["default_off","replay_restart_dedup","mutable_crew","inherited_defect_context","later_fix_regrade","coverage_abstentions","refusal_invalid_timeout","captured_assignment_join","hypothetical_forced_conflict_exclusion","frozen_trial_selection","sample_denominators","insufficient_controls","switched_off_read_send_fence"],"accuracy_or_adoption_established":false}))?)?;
    Ok(())
}

fn main() -> FixtureResult {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "--fixture-child") {
        return fixture_run(Path::new(
            args.get(2).ok_or("isolated fixture root required")?,
        ));
    }
    let root = Path::new(
        args.get(1)
            .ok_or("pass a fresh isolated root under .orbit/tmp")?,
    );
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .arg("--fixture-child")
        .arg(&root)
        .env_clear()
        .env("HOME", &root)
        .env("USERPROFILE", &root)
        .current_dir(&root)
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        return Err("isolated delivery quality fixture failed".into());
    }
    Ok(())
}
