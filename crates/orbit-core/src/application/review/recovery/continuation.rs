//! A claim-only, read-only checkout for independent evidence adjudication.
use super::refused;
use crate::OrbitRuntime;
use orbit_common::{
    OrbitError,
    fs::git::{GIT_REMOTE_TIMEOUT, run_git_within},
};
use orbit_types::workflow::HeldRecoveryDisposition;
use serde_json::{Value, json};

pub(crate) fn prepare(
    runtime: &OrbitRuntime,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, OrbitError> {
    let leaf = runtime.current_claimed_leaf()?;
    let id = leaf.binding.bound_run_id.as_str();
    if run_id.is_some_and(|requested| requested != id) {
        return Err(refused("adjudication run identity changed"));
    }
    let admission = super::claimed_admission(runtime, id)?
        .ok_or_else(|| refused("authenticated adjudication receipt missing"))?;
    let b = &admission.binding;
    if b.disposition != HeldRecoveryDisposition::AdjudicateEvidence {
        return Err(refused("source repair cannot enter read-only adjudication"));
    }
    let pr = runtime.read_pull_request_status(&b.pull_request)?;
    if pr.number.map(|n| n.to_string()).as_deref() != Some(b.pull_request.as_str())
        || pr.url.as_deref() != Some(b.pull_request_url.as_str())
        || pr.state.as_deref() != Some("OPEN")
        || pr.head_sha.as_deref() != Some(b.candidate_sha.as_str())
        || pr.head_branch.as_deref() != Some(b.branch.as_str())
        || pr.base_branch.as_deref() != Some(b.base_branch.as_str())
        || pr.auto_merge_enabled != Some(false)
        || pr.merge_commit.is_some()
        || pr.merged_at.is_some()
    {
        return Err(refused("adjudication PR identity or merge intent changed"));
    }
    let repo = &runtime.paths().repo_root;
    let remote = git2::Repository::open(repo)
        .and_then(|r| r.config()?.get_string("remote.origin.url"))
        .map_err(|e| refused(e.to_string()))?;
    if remote != b.remote_url {
        return Err(refused("adjudication repository changed"));
    }
    for (branch, sha) in [(&b.branch, &b.candidate_sha), (&b.base_branch, &b.base_sha)] {
        let out = run_git_within(
            repo,
            &[
                "ls-remote",
                "--exit-code",
                "origin",
                &format!("refs/heads/{branch}"),
            ],
            GIT_REMOTE_TIMEOUT,
        )?;
        if !out.success || out.stdout.split_whitespace().next() != Some(sha.as_str()) {
            return Err(refused("adjudication head or target lease changed"));
        }
    }
    if input["observe"].as_bool() == Some(true) {
        return Ok(
            json!({"phase":"observed","pr_number":b.pull_request,"pr_url":b.pull_request_url,"head":b.branch,"head_sha":b.candidate_sha,"base":b.base_branch}),
        );
    }
    let fetched = run_git_within(
        repo,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("refs/heads/{}", b.branch),
        ],
        GIT_REMOTE_TIMEOUT,
    )?;
    if !fetched.success {
        return Err(refused(format!(
            "fetch exact retained source: {}",
            fetched.stderr
        )));
    }
    let expected = runtime
        .paths()
        .state_dir
        .join("recovery-checkouts")
        .join(id);
    let checkout = if expected.exists() {
        if std::fs::symlink_metadata(&expected)
            .map_err(|e| refused(e.to_string()))?
            .file_type()
            .is_symlink()
        {
            return Err(refused("adjudication checkout is a symlink"));
        }
        expected
            .canonicalize()
            .map_err(|e| refused(e.to_string()))?
    } else {
        runtime.create_recovery_checkout(id, &b.candidate_sha)?
    };
    if !orbit_engine::review_gate::uncommitted_paths(&checkout)?.is_empty() {
        return Err(refused(
            "adjudication checkout has unverified edits; preserve it for inspection",
        ));
    }

    let candidate = orbit_engine::review_gate::revision(&checkout, "HEAD")?;
    if candidate.commit != b.candidate_sha
        || candidate.tree != b.candidate_tree
        || !orbit_engine::review_gate::contains_commit(
            &checkout,
            &b.source_base_sha,
            &b.candidate_sha,
        )?
    {
        return Err(refused("retained source identity/base changed"));
    }
    Ok(
        json!({"phase":"held_recovery_prepare","job_run_id":id,"workspace_path":checkout,"head_ref":b.branch,"head_sha":b.candidate_sha,
        "base_sha":b.source_base_sha,"source_base_sha":b.source_base_sha,"target_base_sha":b.base_sha,"before_landing":b.review.gates_landing(),"detached":true}),
    )
}
