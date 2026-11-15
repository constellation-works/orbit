//! Fresh provider and Git identity shared by the two publication provenance paths.
use super::*;

pub(super) struct PublicationIdentity<'a> {
    pub(super) branch: &'a str,
    pub(super) head: &'a str,
    pub(super) base_sha: &'a str,
    pub(super) base_branch: &'a str,
    pub(super) number: &'a str,
    pub(super) allow_missing_branch: bool,
    /// The unpublished tip the source's certified chases carried `head` to:
    /// the only other local branch tip binding accepts.
    pub(super) certified_tip: Option<&'a super::certified_rebase::CertifiedRebaseTip>,
}

impl OrbitRuntime {
    pub(super) fn bind_source_publication(
        &self,
        source: JobRun,
        task: Task,
        state: PipelineState,
        definition: JobV2,
        identity: PublicationIdentity<'_>,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        let PublicationIdentity {
            branch,
            head,
            base_sha,
            base_branch,
            number,
            allow_missing_branch,
            certified_tip,
        } = identity;
        let repo = &self.paths().repo_root;
        for sha in [head, base_sha] {
            if !observation::is_exact_sha(sha) || !observation::resolves_to_commit(repo, sha)? {
                return Err(refuse("candidate/base object unavailable or inexact"));
            }
        }
        let local_ref = format!("refs/heads/{branch}");
        let raw_tip = git2::Repository::open(repo)
            .and_then(|r| r.find_reference(&local_ref).map(|r| r.target()))
            .ok()
            .flatten()
            .map(|oid| oid.to_string());
        let held_same_tree_tip = raw_tip
            .filter(|tip| tip != head)
            .filter(|tip| same_tree(repo, tip, head))
            .is_some_and(|tip| {
                self.held_commit_explains_tip(&task.id, &source.run_id, &tip)
                    .unwrap_or(false)
            });
        // [ORB-24993] A footprint-held resume recommits the published patch on
        // its adopted branch: the retained tip may be a different commit with
        // the identical tree (same content, different timestamp) created by
        // that held run. That is not a move when a held run that adopted this
        // source proves the tip with its own `commit` checkpoint; the remote
        // and PR stay bound to P below. Anything else keeps the refusal, so an
        // empty or divergent commit still refuses.
        let candidate_head = if held_same_tree_tip {
            None
        } else {
            match git2::Repository::open(repo)
                .and_then(|r| r.find_reference(&local_ref).map(|r| r.target()))
            {
                Ok(Some(oid)) if oid.to_string() == head => None,
                // The published head P stays the remote and PR identity below;
                // the local tip T is adopted only as the end of the source's
                // certified chase chain from P, and only while it still contains
                // the base the last chase landed it on.
                Ok(Some(oid)) if certified_tip.is_some_and(|tip| oid.to_string() == tip.head) => {
                    certified_tip
                }
                Err(error) if allow_missing_branch && error.code() == git2::ErrorCode::NotFound => {
                    None
                }
                _ => return Err(refuse("retained local branch moved")),
            }
        };
        if let Some(tip) = candidate_head
            && (!observation::resolves_to_commit(repo, &tip.head)?
                || !observation::is_git_ancestor(repo, &tip.target_base, &tip.head)?)
        {
            return Err(refuse("retained local branch moved"));
        }
        if observation::read_git_observation(
            repo,
            &[
                "ls-remote",
                "--exit-code",
                "origin",
                &format!("refs/heads/{branch}"),
            ],
            false,
        )?
        .split_whitespace()
        .next()
            != Some(head)
        {
            return Err(refuse("published branch lease drifted or disappeared"));
        }
        let remote_url = git2::Repository::open(repo)
            .and_then(|repo| repo.config()?.get_string("remote.origin.url"))
            .map_err(|e| refuse(format!("origin configuration unreadable: {e}")))?;
        let identity = orbit_types::workspace::git_remote_identity(&remote_url)
            .map_err(|e| refuse(e.to_string()))?;
        let repository = identity
            .strip_prefix("github.com/")
            .ok_or_else(|| refuse("source repair requires an exact GitHub origin identity"))?
            .to_string();
        let url = format!("https://github.com/{repository}/pull/{number}");
        task_pr_association(&task, number, &url)?;
        let status = self.read_pull_request_status(number)?;
        if status.url.as_deref() != Some(&url)
            || status.number.map(|n| n.to_string()).as_deref() != Some(number)
            || status.state.as_deref() != Some("OPEN")
            || status.head_branch.as_deref() != Some(branch)
            || status.head_sha.as_deref() != Some(head)
            || status.base_branch.as_deref() != Some(base_branch)
            || !status.merge_absence_confirmed()
            || status.merge_commit.is_some()
            || status.merged_at.is_some()
            || status.auto_merge_enabled != Some(false)
        {
            return Err(refuse(
                "provider PR identity changed, closed/merged, or automatic merge intent is uncertain; inspect it explicitly",
            ));
        }
        let binding = SourceRepairBinding {
            schema_version: 1,
            workspace_id: self.workspace_id()?,
            task_id: task.id.clone(),
            source_run_id: source.run_id.clone(),
            machine_id: self
                .automation_machine_identity()
                .ok_or_else(|| refuse("local identity missing"))?
                .into(),
            source_digest: run_evidence_digest(&source, &state, &definition)?,
            task_digest: task_digest(&task)?,
            repository,
            remote_url,
            branch: branch.into(),
            pull_request: number.into(),
            pull_request_url: url,
            head_sha: head.into(),
            base_sha: base_sha.into(),
            base_branch: base_branch.into(),
            superseded: None,
            checkpoint_owner: None,
            candidate_head_sha: candidate_head.map(|tip| tip.head.clone()),
        };
        Ok((source, task, binding))
    }
}

impl OrbitRuntime {
    /// Whether a footprint-held run that adopted `source_id` proves `tip` with
    /// its own `commit` checkpoint: the held resume recommitted the published
    /// patch, so the retained branch names a different commit with the same
    /// content. Anything else (an empty or divergent manual commit, another
    /// run's work) answers `false` and keeps the ordinary refusal.
    fn held_commit_explains_tip(
        &self,
        task_id: &str,
        source_id: &str,
        tip: &str,
    ) -> Result<bool, OrbitError> {
        let runs =
            self.stores()
                .jobs()
                .list_job_runs_filtered(&orbit_store::contracts::JobRunQuery {
                    job_id: Some(orbit_types::workflow::TASK_PR_PIPELINE_JOB.into()),
                    task_id: Some(task_id.into()),
                    limit: Some(super::successor::SUCCESSOR_CHAIN_WINDOW),
                    include_steps: false,
                    ..Default::default()
                })?;
        for listed in runs {
            // The filtered list omits steps; `footprint_held` judges them, so
            // reload the full run exactly as `predecessor()` does.
            let Some(run) = self.get_job_run_backend(&listed.run_id)? else {
                continue;
            };
            if !super::published_owner::footprint_held(&run) {
                continue;
            }
            if self.footprint_held_adopted_source(&run)?.as_deref() != Some(source_id) {
                continue;
            }
            let Ok(Some(state)) = self.read_run_state(&run.run_id) else {
                continue;
            };
            if state
                .pipeline
                .get("commit")
                .and_then(|c| c.get("commit_sha"))
                .and_then(|v| v.as_str())
                == Some(tip)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Whether two commits name the identical tree (same content). A recommitted
/// patch (same parent, same tree, same message, different timestamp) names a
/// different commit with the same tree; that is not a branch move. Any Git
/// failure answers `false` so the caller keeps its original refusal.
fn same_tree(repo: &std::path::Path, a: &str, b: &str) -> bool {
    let tree = |sha: &str| {
        observation::read_git_observation(repo, &["rev-parse", &format!("{sha}^{{tree}}")], false)
            .ok()
    };
    match (tree(a), tree(b)) {
        (Some(ta), Some(tb)) => !ta.is_empty() && ta == tb,
        _ => false,
    }
}
