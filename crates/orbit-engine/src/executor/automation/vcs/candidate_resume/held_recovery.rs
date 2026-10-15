//! Continue only the exact operator-admitted candidate and existing PR.
use super::super::git::{git_output, git_run};
use super::super::pr::read_pr_status;
use super::adopt::{Adoption, adopt_published_branch};
use super::apply::{Applied, apply, output};
use super::{Candidate, Outcome};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{HeldRecoveryAdmission, HeldRecoveryDisposition, SourceRepairBinding};
use serde_json::{Value, json};
use std::path::Path;

fn refused(reason: impl Into<String>) -> OrbitError {
    OrbitError::CapabilityDenied(format!("held recovery refused: {}", reason.into()))
}

/// Reuse existing publication lease guards without acquiring their distinct
/// cancelled-owner-run authority.
pub(in crate::executor::automation::vcs) fn publication_binding<H: RuntimeHost + ?Sized>(
    host: &H,
    admission: &HeldRecoveryAdmission,
    workspace: &Path,
) -> Result<SourceRepairBinding, OrbitError> {
    let b = &admission.binding;
    let remote = git_output(workspace, &["config", "--get", "remote.origin.url"])?;
    let identity =
        orbit_types::workspace::git_remote_identity(&remote).map_err(|e| refused(e.to_string()))?;
    if !identity.eq_ignore_ascii_case(&format!("github.com/{}", b.repository)) {
        return Err(refused("repository identity changed"));
    }
    Ok(SourceRepairBinding {
        schema_version: 1,
        workspace_id: b.workspace_id.clone(),
        task_id: b.task_id.clone(),
        source_run_id: b.source_run_id.clone(),
        machine_id: host
            .local_machine_id()
            .ok_or_else(|| refused("execution machine missing"))?,
        source_digest: b.claim_digest.clone(),
        task_digest: b.proposed_meaning_digest.clone(),
        repository: b.repository.clone(),
        remote_url: remote,
        branch: b.branch.clone(),
        pull_request: b.pull_request.clone(),
        pull_request_url: b.pull_request_url.clone(),
        head_sha: b.candidate_sha.clone(),
        base_sha: b.base_sha.clone(),
        base_branch: b.base_branch.clone(),
        superseded: None,
    })
}

pub(super) fn continue_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    admission: &HeldRecoveryAdmission,
    workspace: &Path,
    base: &str,
) -> Result<Value, OrbitError> {
    let b = &admission.binding;
    let binding = publication_binding(host, admission, workspace)?;
    if base != b.base_sha
        || git_output(workspace, &["rev-parse", "HEAD"])? != base
        || !git_output(workspace, &["status", "--porcelain"])?.is_empty()
        || git_output(workspace, &["config", "--get", "remote.origin.url"])? != binding.remote_url
    {
        return Err(refused("checkout, base or origin changed"));
    }
    let lease = git_output(
        workspace,
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{}", b.branch),
        ],
    )?;
    let base_lease = git_output(
        workspace,
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{}", b.base_branch),
        ],
    )?;
    if lease.split_whitespace().next() != Some(b.candidate_sha.as_str())
        || base_lease.split_whitespace().next() != Some(base)
    {
        return Err(refused("head or base lease changed"));
    }
    if !super::source_repair::exact_open_pr(
        &binding,
        &read_pr_status(host, workspace, &b.pull_request)?,
        &b.candidate_sha,
    ) {
        return Err(refused("provider identity or merge intent changed"));
    }
    let fetched = git_run(
        workspace,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("refs/heads/{}", b.branch),
        ],
    )?;
    if !fetched.success {
        return Err(refused(format!("candidate fetch: {}", fetched.stderr)));
    }
    let candidate = Candidate {
        run_id: b.source_run_id.clone(),
        branch: b.branch.clone(),
        head_sha: b.candidate_sha.clone(),
        durable_ref: None,
        failed_step_id: "held_recovery".into(),
        needs_review_repair: false,
        held: false,
        claim: None,
        published: true,
        pull_request: Some(b.pull_request.clone()),
        pull_request_head: Some(b.branch.clone()),
        interrupted_review: None,
    };
    let mut repair = json!({"trigger":"continuation","failed_step_id":"held_recovery",
        "held_recovery":admission,"output":admission.reason});
    if b.disposition != HeldRecoveryDisposition::SourceRepair {
        return Err(refused(
            "evidence adjudication uses its read-only continuation",
        ));
    }
    let mut applied_tree = None;
    match apply(&candidate, &b.candidate_sha, workspace, base)? {
        Applied::Clean | Applied::AlreadyPresent => {
            applied_tree = Some(git_output(workspace, &["write-tree"])?);
        }
        Applied::Conflict { paths, output } => {
            if paths.iter().any(|path| {
                !b.repair_selectors
                    .iter()
                    .any(|s| orbit_common::fs::selector::overlaps(s, &format!("file:{path}")))
            }) {
                return Err(refused("conflict falls outside approved repair scope"));
            }
            repair["trigger"] = json!("conflict");
            repair["conflicting_paths"] = json!(paths);
            repair["output"] = json!(format!("{}\n{output}", admission.reason));
        }
        Applied::Refused(reason) => return Err(refused(reason)),
    }
    match adopt_published_branch(workspace, &b.task_id, &candidate)? {
        Adoption::Adopted => {}
        Adoption::Refused(reason) => return Err(refused(reason)),
    }
    let mut result = output(&Outcome::Repair(repair), Some(&candidate), base);
    result["prior_pull_request"] = json!(b.pull_request);
    result["prior_pull_request_head"] = json!(b.branch);
    result["reused_branch"] = json!(b.branch);
    result["reused_head_sha"] = json!(b.candidate_sha);
    result["recovery_applied_tree"] = json!(applied_tree);
    result["recovery_request_key"] = json!(admission.request_key);
    result["run_id"] = json!(run_id);
    Ok(result)
}

/// Enforce the admitted amendment scope and original base before publication.
pub(in crate::executor::automation::vcs) fn validate_publication<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace: &Path,
) -> Result<(), OrbitError> {
    let Some(run_id) = input
        .get("run_id")
        .or_else(|| input.get("job_run_id"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let Some(admission) = host.held_recovery_admission(run_id)? else {
        return Ok(());
    };
    let b = &admission.binding;
    let base = git_output(
        workspace,
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{}", b.base_branch),
        ],
    )?;
    if base.split_whitespace().next() != Some(b.base_sha.as_str()) {
        return Err(refused("approved base lease moved"));
    }
    let head = git_output(workspace, &["rev-parse", "HEAD"])?;
    let changed = git_output(
        workspace,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            &b.base_sha,
            &head,
            "--",
        ],
    )?;
    if b.disposition == HeldRecoveryDisposition::SourceRepair {
        let state = host
            .read_run_state(run_id)?
            .ok_or_else(|| refused("source repair checkpoint missing"))?;
        let resumed = state
            .pipeline
            .get("resume_candidate")
            .ok_or_else(|| refused("authenticated source application missing"))?;
        if state.run_id != run_id
            || state.job_id != "task_claimed_pr_pipeline"
            || resumed["recovery_request_key"] != admission.request_key
            || !state.step_outputs.iter().any(|(index, out)| {
                out == resumed
                    && state.step_states.get(index)
                        == Some(&orbit_types::workflow::JobRunState::Success)
            })
        {
            return Err(refused("source application checkpoint identity changed"));
        }
        if let Some(tree) = resumed["recovery_applied_tree"].as_str() {
            if git_output(workspace, &["diff", "--name-only", tree, &head, "--"])?.is_empty() {
                return Err(refused(
                    "implementation made no actual repair after restoring the retained source",
                ));
            }
        } else if resumed["repair"]["conflicting_paths"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            return Err(refused(
                "source application tree or scoped conflict evidence missing",
            ));
        }
    }
    match b.disposition {
        HeldRecoveryDisposition::AdjudicateEvidence
            if head != b.candidate_sha || !changed.is_empty() =>
        {
            Err(refused("adjudication changed the exact held source"))
        }
        HeldRecoveryDisposition::SourceRepair if changed.is_empty() => {
            Err(refused("source repair produced no actual source amendment"))
        }
        HeldRecoveryDisposition::SourceRepair
            if changed.lines().any(|path| {
                !b.repair_selectors.iter().any(|selector| {
                    orbit_common::fs::selector::overlaps(selector, &format!("file:{path}"))
                })
            }) =>
        {
            Err(refused(
                "source repair changed a path outside the explicitly approved selectors",
            ))
        }
        _ => Ok(()),
    }
}
