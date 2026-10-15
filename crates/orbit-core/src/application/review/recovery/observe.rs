//! Authenticate retained claimed evidence, exact old/current meaning and PR lease.
use super::refused;
use crate::OrbitRuntime;
use orbit_common::{
    OrbitError,
    fs::git::{GIT_REMOTE_TIMEOUT, run_git, run_git_within},
    security::release::sha256_hex,
};
use orbit_engine::review_gate;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{ClaimInspection, ClaimSettlementEvidence};
use orbit_types::{
    task::{CANDIDATE_DISCARDED_EVENT, TaskStatus},
    workflow::{
        HeldRecoveryBinding, HeldRecoveryDisposition, HeldRecoveryMeaning,
        REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewEvidenceHold,
    },
};
use serde_json::Value;

pub(super) struct Observation {
    pub binding: HeldRecoveryBinding,
    pub diagnostics: Value,
}

fn encode_digest(value: &impl serde::Serialize) -> Result<String, OrbitError> {
    serde_json::to_vec(value)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|e| refused(e.to_string()))
}

/// Bind only a settled claim with its retained candidate; an operator revoke
/// without a terminal executor settlement cannot prove that execution stopped.
fn source(runtime: &OrbitRuntime, id: &str) -> Result<ClaimInspection, OrbitError> {
    let task = runtime.get_task(id)?;
    let claims = runtime.inspect_execution_claims()?;
    if claims.iter().any(|c| {
        (c.claim.task_id == id
            || c.claim.footprint.iter().any(|scope| {
                task.context_files
                    .iter()
                    .any(|path| orbit_common::fs::selector::overlaps(scope, path))
            }))
            && (c.claim.phase.is_unsettled() || c.unresolved_merge_intent.is_some())
    }) {
        return Err(refused("active claim or unresolved merge intent"));
    }
    let linked = task
        .job_run_machine
        .as_ref()
        .ok_or_else(|| refused("linked source machine missing"))?;
    let claim = claims
        .into_iter()
        .find(|c| {
            c.claim.task_id == id
                && c.bound_run.as_ref().is_some_and(|run| {
                    Some(&run.run_id) == task.job_run_id.as_ref()
                        && run.machine_id == linked.machine_id
                })
        })
        .ok_or_else(|| refused("no authenticated claim for the exact linked machine and run"))?;
    if claim.settlement.is_none() || claim.preserved_candidate.is_none() {
        return Err(refused(
            "source has no terminal settlement and retained candidate",
        ));
    }
    Ok(claim)
}

pub(super) fn observe(
    runtime: &OrbitRuntime,
    id: &str,
    input: &Value,
) -> Result<Observation, OrbitError> {
    let task = runtime.get_task(id)?;
    if !matches!(
        task.status,
        TaskStatus::InProgress | TaskStatus::Blocked | TaskStatus::Backlog
    ) {
        return Err(refused("task is not a stopped held or rejected delivery"));
    }
    let disposition: HeldRecoveryDisposition = serde_json::from_value(input["disposition"].clone())
        .map_err(|_| refused("disposition must be adjudicate_evidence or source_repair"))?;
    let source = source(runtime, id)?;
    let preserved = source
        .preserved_candidate
        .as_ref()
        .ok_or_else(|| refused("candidate missing"))?;
    let candidate = &preserved.candidate;
    let run = source
        .bound_run
        .as_ref()
        .ok_or_else(|| refused("bound run missing"))?;
    if candidate.source_run_id.as_deref() != Some(run.run_id.as_str()) || !candidate.durable() {
        return Err(refused(
            "retained source identity or durability is unavailable",
        ));
    }
    let recorded = chrono::DateTime::parse_from_rfc3339(&preserved.recorded_at)
        .map_err(|e| refused(e.to_string()))?;
    if runtime
        .get_task_history(id)?
        .iter()
        .any(|e| e.event == CANDIDATE_DISCARDED_EVENT && e.at >= recorded)
    {
        return Err(refused("retained candidate was discarded"));
    }
    let artifact = runtime
        .get_task_artifact(id, REVIEW_GATE_ARTIFACT)?
        .ok_or_else(|| refused("certificate unavailable"))?;
    let certificate: ReviewCertificate =
        serde_json::from_slice(&artifact.content).map_err(|e| refused(e.to_string()))?;
    if certificate.task_ids != vec![id.to_string()]
        || certificate.verdict.passed()
        || certificate.final_candidate.commit != candidate.head_sha
    {
        return Err(refused(
            "certificate does not bind this rejected retained candidate",
        ));
    }
    let proposed = HeldRecoveryMeaning::from_task(&task);
    let original: HeldRecoveryMeaning = match input.get("original") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| refused(format!("original contract: {e}")))?,
        None if disposition == HeldRecoveryDisposition::AdjudicateEvidence => proposed.clone(),
        None => {
            return Err(refused(
                "source_repair requires the original retained contract",
            ));
        }
    };
    if original.required_tools != proposed.required_tools {
        return Err(refused("recovery cannot amend tool authority"));
    }
    if original.id != id {
        return Err(refused("original task identity differs"));
    }
    let old_task = original.apply_to(&task);
    let meaning = |task| {
        orbit_automation::review::task_meaning_digest(task).map_err(|e| refused(e.to_string()))
    };
    let original_meaning_digest = meaning(&old_task)?;
    let combined = orbit_automation::review::combined_task_meaning_digest(&[(
        id.into(),
        original_meaning_digest.clone(),
    )])
    .map_err(|e| refused(e.to_string()))?;
    if combined != certificate.task_meaning_digest
        || !old_task.spec_digest_matches(&preserved.task_spec_digest)
    {
        return Err(refused(
            "original contract is not the retained candidate's authenticated meaning",
        ));
    }
    let delta = original
        .delta(&proposed)
        .map_err(|e| refused(e.to_string()))?;
    let mut repair_selectors: Vec<String> = match input.get("repair_selectors") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| refused(format!("repair selectors: {e}")))?,
        None => Vec::new(),
    };
    repair_selectors.sort();
    repair_selectors.dedup();
    for selector in &repair_selectors {
        let canonical = orbit_common::fs::selector::canonical_selector_in_workspace(
            selector,
            &runtime.paths().repo_root,
        );
        if canonical.as_ref().is_err() {
            return Err(refused("repair scope contains an invalid selector"));
        }
    }

    if repair_selectors
        .iter()
        .any(|selector| !proposed.context_files.contains(selector))
    {
        return Err(refused(
            "repair selectors must be explicitly present in the current task scope",
        ));
    }
    let hold_artifact = runtime.get_task_artifact(id, REVIEW_EVIDENCE_HOLD_ARTIFACT)?;
    let hold: Option<ReviewEvidenceHold> = hold_artifact
        .as_ref()
        .map(|a| serde_json::from_slice(&a.content).map_err(|e| refused(format!("hold: {e}"))))
        .transpose()?;
    match disposition {
        HeldRecoveryDisposition::AdjudicateEvidence => {
            let hold = hold
                .as_ref()
                .ok_or_else(|| refused("adjudication requires an evidence-only hold"))?;
            if !delta.is_empty()
                || !repair_selectors.is_empty()
                || hold.attempt_id != certificate.attempt_id
                || hold.run_id != run.run_id
                || hold.candidate != certificate.final_candidate
                || source
                    .settlement
                    .as_ref()
                    .is_none_or(|s| s.evidence != ClaimSettlementEvidence::EvidenceHold)
                || !super::super::evidence::evidence_only(&certificate, &hold.requirements)
            {
                return Err(refused(
                    "failed assertions, findings or changed meaning require source_repair",
                ));
            }
        }
        HeldRecoveryDisposition::SourceRepair => {
            if delta.is_empty()
                || repair_selectors.is_empty()
                || old_task.spec_digest() == task.spec_digest()
            {
                return Err(refused(
                    "source_repair requires an explicit material amendment and nonempty repair scope",
                ));
            }
        }
    }
    let number = candidate
        .pull_request
        .as_deref()
        .ok_or_else(|| refused("retained candidate has no PR"))?;
    let branch = candidate
        .pull_request_head
        .as_deref()
        .unwrap_or(&candidate.branch);
    let repo = &runtime.paths().repo_root;
    let git = |args: &[&str]| -> Result<String, OrbitError> {
        let out = if args.first() == Some(&"ls-remote") {
            run_git_within(repo, args, GIT_REMOTE_TIMEOUT)?
        } else {
            run_git(repo, args)?
        };
        if !out.success {
            return Err(refused(format!("Git observation: {}", out.stderr)));
        }
        Ok(out.stdout.trim().into())
    };
    let remote_url = git2::Repository::open(repo)
        .and_then(|repository| repository.config()?.get_string("remote.origin.url"))
        .map_err(|e| refused(format!("origin configuration unavailable: {e}")))?;
    let identity = orbit_types::workspace::git_remote_identity(&remote_url)
        .map_err(|e| refused(e.to_string()))?;
    let repository = identity
        .strip_prefix("github.com/")
        .ok_or_else(|| refused("exact GitHub identity required"))?
        .to_string();
    if certificate.repository != repository {
        return Err(refused("certificate belongs to a different repository"));
    }
    let url = format!("https://github.com/{repository}/pull/{number}");
    if !task.external_refs.iter().any(|r| {
        r.system == orbit_types::task::GITHUB_PR_EXTERNAL_REF_SYSTEM
            && r.id == number
            && r.url.as_deref().is_none_or(|u| u == url)
    }) {
        return Err(refused("task has no authenticated PR association"));
    }
    let pr = runtime.read_pull_request_status(number)?;
    if pr.number.map(|n| n.to_string()).as_deref() != Some(number)
        || pr.url.as_deref() != Some(url.as_str())
        || pr.state.as_deref() != Some("OPEN")
        || pr.head_branch.as_deref() != Some(branch)
        || pr.head_sha.as_deref() != Some(candidate.head_sha.as_str())
        || pr.auto_merge_enabled != Some(false)
        || pr.merge_commit.is_some()
        || pr.merged_at.is_some()
    {
        return Err(refused(
            "provider identity, head lease or merge intent changed or is uncertain",
        ));
    }
    let base_branch = pr
        .base_branch
        .as_deref()
        .ok_or_else(|| refused("base branch missing"))?;
    if git(&[
        "ls-remote",
        "--exit-code",
        "origin",
        &format!("refs/heads/{branch}"),
    ])?
    .split_whitespace()
    .next()
        != Some(candidate.head_sha.as_str())
    {
        return Err(refused("published candidate lease moved"));
    }
    let target = git(&[
        "ls-remote",
        "--exit-code",
        "origin",
        &format!("refs/heads/{base_branch}"),
    ])?;
    let target_base = target
        .split_whitespace()
        .next()
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| refused("target base lease unavailable"))?
        .to_string();
    if review_gate::revision(repo, &candidate.head_sha).is_err() {
        let fetched = run_git_within(
            repo,
            &[
                "fetch",
                "--no-tags",
                "origin",
                &format!("refs/heads/{branch}"),
            ],
            GIT_REMOTE_TIMEOUT,
        )?;
        if !fetched.success {
            return Err(refused(format!(
                "retained source is unavailable: {}",
                fetched.stderr
            )));
        }
    }
    let revision = review_gate::revision(repo, &candidate.head_sha)?;
    if revision != certificate.final_candidate {
        return Err(refused("candidate object/tree mismatch"));
    }
    let mut capture = serde_json::json!({"task_ids":[id]});
    super::super::install_review_admission(runtime, "task_pr_pipeline", &mut capture, None, false)?;
    let mut review: orbit_types::workflow::ReviewAdmission =
        serde_json::from_value(capture[orbit_types::workflow::REVIEW_ADMISSION_KEY].clone())
            .map_err(|e| refused(e.to_string()))?;
    review.captured_at = chrono::DateTime::parse_from_rfc3339(&source.created_at)
        .map_err(|e| refused(e.to_string()))?
        .with_timezone(&chrono::Utc);
    if !review.gates_merge()
        || review
            .required_validation_commands
            .as_ref()
            .is_none_or(Vec::is_empty)
        || certificate
            .required_validation_commands
            .as_ref()
            .is_none_or(|old| {
                old.iter().any(|command| {
                    !review
                        .required_validation_commands
                        .as_ref()
                        .is_some_and(|new| new.contains(command))
                })
            })
    {
        return Err(refused(
            "independent review and all original configured native gates must remain required",
        ));
    }
    let artifact_manifest = runtime.get_task_artifact_manifest(id)?;
    let binding = HeldRecoveryBinding {
        schema_version: 1,
        workspace_id: runtime.workspace_id()?,
        task_id: id.into(),
        disposition,
        claim_id: source.claim.claim_id.clone(),
        machine_id: run.machine_id.clone(),
        source_run_id: run.run_id.clone(),
        claim_digest: TaskCommitBoundary::recovery_claim_digest(&source)?,
        attempt_id: certificate.attempt_id.clone(),
        certificate_digest: sha256_hex(&artifact.content),
        diagnostics_digest: encode_digest(&artifact_manifest)?,
        hold_digest: hold_artifact.as_ref().map(|a| sha256_hex(&a.content)),
        repository,
        remote_url,
        branch: branch.into(),
        pull_request: number.into(),
        pull_request_url: url,
        candidate_sha: revision.commit,
        candidate_tree: revision.tree,
        base_sha: target_base,
        source_base_sha: certificate.base.commit.clone(),
        base_branch: base_branch.into(),
        original,
        proposed,
        original_meaning_digest,
        proposed_meaning_digest: meaning(&task)?,
        original_spec_digest: preserved.task_spec_digest.clone(),
        proposed_spec_digest: task.spec_digest(),
        delta,
        repair_selectors,
        review,
    };
    let diagnostics = serde_json::json!({"claim":source,"certificate":certificate,"hold":hold,
        "artifact_manifest":artifact_manifest});
    Ok(Observation {
        binding,
        diagnostics,
    })
}
