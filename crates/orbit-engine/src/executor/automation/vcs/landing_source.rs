//! Owner-derived source recovery and the scope a fresh leaf must preserve.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::{ci_repair_path_is_safe, claim_new_path_is_safe};
use orbit_store::contracts::ClaimRepair;
use orbit_types::workflow::handoff::{
    HandoffDelivery, LandingFailedCheck, LandingSourceCause, LandingSourceRepair,
};
use serde_json::json;

use super::super::ci::log_signature::{ERROR_MARKERS, error_signature};
use super::freshness::remote_branch_sha;
use super::git::{git_output, git_output_paths};
use super::landing_checks::{answers, commit_jobs, transient_evidence, verified_base_update};
use super::operations;
use super::pr::{
    BlockedVerdict, PrMergeState, PullRequestStatus, blocked_verdict, classify_pr_state,
    read_pr_status, read_required_checks,
};
use crate::context::{HandoffLandingContext, RuntimeHost};

fn refused(detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("landing source repair refused: {detail}"))
}

/// Settled failures owned by branch protection or the accepted certificate.
/// A mergeable provider state is consistent only when protected checks are green.
fn required_failures<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    status: &PullRequestStatus,
) -> Result<Vec<(Option<String>, String)>, OrbitError> {
    let state = classify_pr_state(status);
    if !matches!(state, PrMergeState::Blocked(_) | PrMergeState::Mergeable) {
        return Err(refused(
            "mergeability is unknown, contradictory or not settled",
        ));
    }
    let failed = match blocked_verdict(status, 0, false) {
        BlockedVerdict::RepairChecks(failed) => failed,
        BlockedVerdict::Stop(reason) => return Err(refused(reason)),
        BlockedVerdict::Wait => return Err(refused("checks have not settled")),
    };
    let pr = status
        .number
        .ok_or_else(|| refused("missing PR identity"))?;
    let policy = read_required_checks(host, &context.workspace_path, &pr.to_string())?;
    if status.head_sha.as_deref() != Some(policy.head_sha.as_str()) {
        return Err(refused("required checks name another head"));
    }
    if matches!(state, PrMergeState::Mergeable)
        && failed
            .iter()
            .any(|(workflow, name)| policy.requires(workflow.as_deref(), name))
    {
        return Err(refused(
            "mergeable provider state contradicts a failed protected check",
        ));
    }
    let required: Vec<_> = failed
        .into_iter()
        .filter(|(workflow, name)| {
            policy.requires(workflow.as_deref(), name)
                || owner_requirement(context, workflow.as_deref(), name).is_some()
        })
        .collect();
    if required.is_empty() {
        return Err(refused(
            "no failed protected or certificate-bound owner-owed check",
        ));
    }
    Ok(required)
}

/// The complete captured command and native requirement, not a plain check name.
fn owner_requirement<'a>(
    context: &'a HandoffLandingContext,
    workflow: Option<&str>,
    name: &str,
) -> Option<&'a orbit_types::workflow::ReviewEvidenceRequirement> {
    let qualified = format!("{} / {name}", workflow?);
    let certificate = context.owner_review_certificate.as_ref()?;
    if certificate.task_ids != [context.task_id.clone()]
        || certificate.repository != context.candidate.repository
        || certificate.final_candidate != context.candidate.candidate
        || certificate.base != context.candidate.base
        || !certificate.validation_complete
        || !matches!(
            certificate.verdict,
            orbit_types::workflow::ReviewVerdict::Accept
                | orbit_types::workflow::ReviewVerdict::AcceptWithFixes
        )
    {
        return None;
    }
    let mut matches = certificate
        .owner_owed_evidence
        .iter()
        .filter(|requirement| {
            requirement.hosted_check.as_deref() == Some(qualified.as_str())
                && !requirement.command.trim().is_empty()
                && !requirement.name.trim().is_empty()
                && !requirement.artifact.trim().is_empty()
        });
    let matched = matches.next()?;
    matches.next().is_none().then_some(matched)
}

pub(super) fn source_repair_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
    recovery: &orbit_types::workflow::handoff::LandingCheckRecovery,
) -> Result<LandingSourceRepair, OrbitError> {
    let required = required_failures(host, context, status)?;
    // No optional failure may turn an administrative BLOCKED rollup into authority.
    if required.iter().any(|check| !failed.contains(check)) {
        return Err(refused(
            "required failure missing from the classified evidence",
        ));
    }
    let source = collect(
        host,
        &context.workspace_path,
        status,
        &required,
        &context.repair_footprint,
        context,
    )?;
    if source.checks.iter().any(|check| {
        !recovery.source_signatures.contains(&(
            check.workflow.clone(),
            check.name.clone(),
            check.signature.clone(),
        ))
    }) {
        return Err(refused(
            "the source diagnostic changed or lacks the original inherited failure evidence",
        ));
    }
    Ok(source)
}

fn collect<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
    footprint: &[String],
    context: &HandoffLandingContext,
) -> Result<LandingSourceRepair, OrbitError> {
    let head = status
        .head_sha
        .as_deref()
        .ok_or_else(|| refused("missing failed head"))?;
    let jobs = commit_jobs(host, workspace, head)?;
    let mut checks = Vec::new();
    let mut paths = BTreeSet::new();
    for check in failed {
        let matching: Vec<_> = jobs
            .iter()
            .filter(|job| answers(job, check) && job["conclusion"] == "failure")
            .collect();
        if matching.len() != 1 {
            return Err(refused("missing or ambiguous exact-head failed job"));
        }
        let job = matching[0];
        if job["head_sha"] != head || job["status"] != "completed" {
            return Err(refused("foreign or unfinished job"));
        }
        let run_id = job["run_id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| refused("missing run identity"))?;
        let job_id = job["job_id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| refused("missing job identity"))?;
        let step = job["failed_step"]
            .as_str()
            .filter(|step| !step.is_empty())
            .ok_or_else(|| refused("missing failed step"))?;
        if !job["steps"].as_array().is_some_and(|steps| {
            steps.iter().any(|record| {
                record["name"] == step
                    && record["status"] == "completed"
                    && record["conclusion"] == "failure"
            })
        }) {
            return Err(refused(
                "failed step lacks actual completed failure execution",
            ));
        }
        let response = host.run_private_vcs_operation(operations::CI_JOB_LOG, json!({"workspace_path":workspace.to_string_lossy(),"run_id":run_id.to_string(),"job_id":job_id.to_string()}))?;
        if response["truncated"] != false
            || response["source_complete"] != true
            || !response["fallback_error"].is_null()
        {
            return Err(refused("incomplete, truncated or unreadable diagnostic"));
        }
        let log = response["log"]
            .as_str()
            .filter(|log| !log.trim().is_empty() && log.len() <= 64 * 1024)
            .ok_or_else(|| refused("missing or oversized diagnostic"))?;
        let lowered = log.to_ascii_lowercase();
        if transient_evidence(log).is_some()
            || super::required_command::missing_tool(Some(1), log, "").is_some()
            || [
                "bad credentials",
                "http 401",
                "http 403",
                "authentication failed",
                "permission denied",
                "could not read username",
                "resource not accessible by integration",
            ]
            .iter()
            .any(|marker| lowered.contains(marker))
        {
            return Err(refused("infrastructure failure"));
        }
        let signature = error_signature(log, step);
        if signature.step_fallback {
            return Err(refused("inconclusive source classification"));
        }
        let mut job_paths = diagnostic_paths(workspace, head, log)?;
        if job_paths.is_empty() {
            for selector in footprint
                .iter()
                .filter(|selector| selector.starts_with("file:") || selector.starts_with("symbol:"))
            {
                let Ok(anchor) = orbit_common::fs::selector::anchor_path(selector) else {
                    continue;
                };
                let relative = if anchor.is_absolute() {
                    match anchor.strip_prefix(workspace) {
                        Ok(path) => path.to_path_buf(),
                        Err(_) => continue,
                    }
                } else {
                    anchor
                };
                let path = relative.to_string_lossy().into_owned();
                if !matches!(
                    relative
                        .extension()
                        .and_then(|extension| extension.to_str()),
                    Some(
                        "rs" | "py"
                            | "sh"
                            | "js"
                            | "ts"
                            | "tsx"
                            | "jsx"
                            | "swift"
                            | "c"
                            | "cc"
                            | "cpp"
                            | "h"
                            | "hpp"
                            | "go"
                            | "java"
                    )
                ) || !ci_repair_path_is_safe(&path)
                {
                    continue;
                }
                let entry = git_output(
                    workspace,
                    &["--literal-pathspecs", "ls-tree", head, "--", &path],
                )?;
                if entry.starts_with("100644 ") || entry.starts_with("100755 ") {
                    job_paths.insert(path);
                }
            }
        }
        if job_paths.is_empty() {
            return Err(refused("diagnostic names no permitted source path"));
        }
        paths.extend(job_paths);
        checks.push(LandingFailedCheck {
            run_id,
            job_id,
            workflow: job["workflow"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| refused("missing workflow identity"))?
                .to_string(),
            name: check.1.clone(),
            failed_step: step.to_string(),
            signature: signature.text,
            diagnostic: log.to_string(),
            owner_requirement: owner_requirement(context, job["workflow"].as_str(), &check.1)
                .cloned(),
        });
    }
    let source = LandingSourceRepair {
        failed_head: head.to_string(),
        pull_request: status
            .number
            .ok_or_else(|| refused("missing PR identity"))?,
        cause: LandingSourceCause::RepeatedInheritedSourceFailure,
        checks,
        permitted_paths: paths.into_iter().collect(),
    };
    if !source.complete() {
        return Err(refused("incomplete or oversized source evidence"));
    }
    Ok(source)
}

/// Only explicit source locations on diagnostic lines count. Command arguments,
/// generic wrapper filenames and paths outside the failed Git tree grant nothing.
fn diagnostic_paths(
    workspace: &Path,
    head: &str,
    log: &str,
) -> Result<BTreeSet<String>, OrbitError> {
    let mut paths = BTreeSet::new();
    for line in log.lines().filter(|line| {
        line.trim_start().starts_with("-->")
            || ERROR_MARKERS
                .iter()
                .any(|marker| line.to_ascii_lowercase().contains(marker))
    }) {
        for word in line.split_whitespace() {
            let path = word
                .trim_matches(|c: char| matches!(c, '\'' | '"' | '(' | ')' | ',' | ';' | '`'))
                .split(':')
                .next()
                .unwrap_or_default()
                .trim_start_matches("./");
            if !path.contains('/') || !ci_repair_path_is_safe(path) {
                continue;
            }
            let entry = git_output(
                workspace,
                &["--literal-pathspecs", "ls-tree", head, "--", path],
            )?;
            if entry.starts_with("100644 ") || entry.starts_with("100755 ") {
                paths.insert(path.to_string());
            }
        }
    }
    Ok(paths)
}

/// Re-read source evidence and the remote lease immediately before the store
/// revokes the immutable handoff. No payload or artifact is authority.
pub fn validate_landing_source_identity<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    source: &LandingSourceRepair,
) -> Result<(), OrbitError> {
    let workspace = &context.workspace_path;
    let candidate = &context.candidate;
    let HandoffDelivery::PullRequest { number } = candidate.delivery else {
        return Err(refused("source repair requires a pull request"));
    };
    let status = read_pr_status(host, workspace, &number.to_string())?;
    if number != source.pull_request
        || status.number != Some(number)
        || status.head_sha.as_deref() != Some(source.failed_head.as_str())
        || status.head_branch.as_deref() != Some(candidate.source_branch.as_str())
        || status.base_branch.as_deref() != Some(candidate.base_branch.as_str())
    {
        return Err(refused("PR, branch, base or failed head changed"));
    }
    let remote = git_output(workspace, &["remote", "get-url", "origin"])?;
    if super::claim::slug(&remote).as_deref() != Some(candidate.repository.as_str()) {
        return Err(refused("repository identity changed"));
    }
    if remote_branch_sha(workspace, &candidate.source_branch)?.as_deref()
        != Some(source.failed_head.as_str())
    {
        return Err(refused("remote lease changed"));
    }
    if source.failed_head != candidate.candidate.commit {
        verified_base_update(
            workspace,
            &candidate.candidate.commit,
            &source.failed_head,
            &candidate.source_branch,
            &format!("origin/{}", candidate.landing_branch),
        )
        .map_err(refused)?;
    }
    let failed = required_failures(host, context, &status)?;
    if collect(
        host,
        workspace,
        &status,
        &failed,
        &context.repair_footprint,
        context,
    )? != *source
    {
        return Err(refused("failed-job diagnosis changed"));
    }
    Ok(())
}

/// Judge the repair delta against the original candidate cleanly merged with
/// the new base. Carried task work and base advances are not new repair edits.
/// `candidate` is None before commit, when the actual worktree is inspected.
pub fn validate_claim_source_repair(
    workspace: &Path,
    repair: &ClaimRepair,
    base: &str,
    candidate: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(source) = &repair.source else {
        return Ok(());
    };
    if !source.complete()
        || source
            .permitted_paths
            .iter()
            .any(|path| !ci_repair_path_is_safe(path))
    {
        return Err(refused("invalid authenticated permitted path scope"));
    }
    let expected = git_output(
        workspace,
        &[
            "merge-tree",
            "--write-tree",
            &repair.candidate.candidate.commit,
            base,
        ],
    )?;
    let tree = expected
        .lines()
        .next()
        .ok_or_else(|| refused("candidate restoration has no tree"))?;
    let mut args = vec!["diff", "--name-only", "--no-renames", "-z", tree];
    if let Some(candidate) = candidate {
        args.push(candidate);
    }
    args.push("--");
    let mut paths: BTreeSet<_> = git_output_paths(workspace, &args)?.into_iter().collect();
    if candidate.is_none() {
        paths.extend(
            git_output_paths(
                workspace,
                &["ls-files", "--others", "--exclude-standard", "-z", "--"],
            )?
            .into_iter()
            .filter(|path| !path.starts_with(".orbit/tmp/")),
        );
    }
    if candidate.is_none() {
        let mut carried = BTreeSet::new();
        for path in &paths {
            // Git paths are untrusted, including carried task work. Validate
            // lexical containment before resolving or inspecting any of them.
            let metadata = working_path_metadata(workspace, path)?;
            if working_path_matches_tree(workspace, tree, path, metadata.as_ref())? {
                carried.insert(path.clone());
            } else if metadata.is_some_and(|metadata| !metadata.is_file()) {
                return Err(refused("source repair changed a non-regular file"));
            }
        }
        paths.retain(|path| !carried.contains(path));
    }
    if paths
        .iter()
        .any(|path| !source.permitted_paths.contains(path) || !ci_repair_path_is_safe(path))
    {
        return Err(refused(format!(
            "unrelated or protected edits outside authenticated scope: {paths:?}"
        )));
    }
    if let Some(candidate) = candidate {
        super::commit::validate_claim_new_paths(workspace, &[], tree, candidate)?;
    }
    Ok(())
}

/// Only inspect canonical in-checkout paths. Comparing the canonical spelling
/// also refuses intermediate symlinks, including links back into the checkout.
/// A missing leaf is a removal, but its nearest existing ancestor must satisfy
/// the same containment rule before it can be treated as absent.
fn working_path_metadata(
    workspace: &Path,
    path: &str,
) -> Result<Option<std::fs::Metadata>, OrbitError> {
    if !claim_new_path_is_safe(path) {
        return Err(refused("unsafe worktree path"));
    }
    let root = workspace.canonicalize().map_err(refused)?;
    let joined = root.join(path);
    let mut existing = joined.as_path();
    loop {
        match existing.canonicalize() {
            Ok(resolved) => {
                if !resolved.starts_with(&root) || resolved != existing {
                    return Err(refused(
                        "worktree path escapes the checkout or traverses a symlink",
                    ));
                }
                if existing != joined {
                    // canonicalize reports NotFound for dangling links too.
                    // Inspect only the next component below the already
                    // validated ancestor, never an unresolved multi-part path.
                    let next = joined
                        .strip_prefix(existing)
                        .map_err(refused)?
                        .components()
                        .next()
                        .ok_or_else(|| refused("missing path component"))?;
                    for entry in std::fs::read_dir(&resolved).map_err(refused)? {
                        if entry.map_err(refused)?.file_name() == next.as_os_str() {
                            return Err(refused("unresolvable or dangling worktree path"));
                        }
                    }
                    return Ok(None);
                }
                return std::fs::symlink_metadata(&resolved)
                    .map(Some)
                    .map_err(refused);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing = existing
                    .parent()
                    .ok_or_else(|| refused("missing checkout root"))?;
            }
            Err(error) => return Err(refused(error)),
        }
    }
}

fn working_path_matches_tree(
    workspace: &Path,
    tree: &str,
    path: &str,
    metadata: Option<&std::fs::Metadata>,
) -> Result<bool, OrbitError> {
    let entry = git_output(
        workspace,
        &["--literal-pathspecs", "ls-tree", tree, "--", path],
    )?;
    let Some(metadata) = metadata else {
        return Ok(entry.is_empty());
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    let blob = entry.split_whitespace().nth(2).unwrap_or_default();
    let observed = git_output(workspace, &["hash-object", "--no-filters", "--", path])?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = metadata.permissions().mode() & 0o111 != 0;
        if executable != entry.starts_with("100755 ") {
            return Ok(false);
        }
    }
    Ok(observed == blob)
}

/// A repaired native/job failure needs execution of its original failed step
/// on the new exact head. A green rollup alone cannot certify skipped work.
pub(super) fn source_certification<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    status: &PullRequestStatus,
) -> Result<super::pr::OwedHostedChecks, OrbitError> {
    use super::pr::OwedHostedChecks;
    let Some(source) = &context.source_repair else {
        return Ok(OwedHostedChecks::Passed);
    };
    if !source.complete() {
        return Err(refused("incomplete retained source certification contract"));
    }
    let head = status
        .head_sha
        .as_deref()
        .ok_or_else(|| refused("missing certified head"))?;
    let jobs = commit_jobs(host, &context.workspace_path, head)?;
    let mut waiting = Vec::new();
    for check in &source.checks {
        let label = format!("{} / {}", check.workflow, check.name);
        let matching: Vec<_> = jobs
            .iter()
            .filter(|job| {
                job["workflow"] == check.workflow
                    && job["name"] == check.name
                    && job["head_sha"] == head
            })
            .collect();
        if matching.len() != 1 {
            waiting.push(label);
            continue;
        }
        let job = matching[0];
        if job["conclusion"] == "failure" {
            return Ok(OwedHostedChecks::Failed(format!(
                "hosted source certification '{label}' failed"
            )));
        }
        let certified = job["status"] == "completed"
            && job["conclusion"] == "success"
            && job["run_id"].as_u64().is_some_and(|id| id > 0)
            && job["job_id"].as_u64().is_some_and(|id| id > 0)
            && job["steps"].as_array().is_some_and(|steps| {
                steps.iter().any(|step| {
                    step["name"] == check.failed_step
                        && step["status"] == "completed"
                        && step["conclusion"] == "success"
                })
            });
        if !certified {
            waiting.push(label);
        }
    }
    Ok(if waiting.is_empty() {
        OwedHostedChecks::Passed
    } else {
        OwedHostedChecks::Waiting(waiting)
    })
}
