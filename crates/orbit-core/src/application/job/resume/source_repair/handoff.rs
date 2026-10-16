//! A terminal hook publishes preserved work without certifying that candidate.
use super::identity::PublicationIdentity;
use super::observation::authenticated_checkpoint;
use super::*;

impl OrbitRuntime {
    pub(super) fn observe_handoff_publication(
        &self,
        source: JobRun,
        task: Task,
        state: PipelineState,
        definition: JobV2,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        let checkpoint = state
            .failure_activity_checkpoint
            .as_ref()
            .ok_or_else(|| refuse("failure-handoff checkpoint missing"))?;
        let output = &checkpoint.output;
        let worktree = authenticated_checkpoint(&definition, &state, "worktree", "worktree_setup")?;
        let failed_index = definition
            .steps
            .iter()
            .position(|s| s.id == checkpoint.failed_step_id)
            .ok_or_else(|| {
                refuse("failure-handoff failed step is absent from source definition")
            })?;
        if definition.failure_activity.as_deref() != Some("pr_failure_handoff")
            || !definition.resolved_failure_activity.as_ref().is_some_and(|a|
                matches!(&a.spec, ActivityV2Spec::Deterministic(s) if s.action == "pr_failure_handoff"))
            || checkpoint.activity_name != "pr_failure_handoff"
            || state.step_states.get(&(failed_index as u32)) != Some(&JobRunState::Failed)
            || field(output, "phase")? != "failure_handoff"
            || !matches!(field(output, "decision")?, "blocked_failure_pr" | "blocked_conflict_pr")
            || field(output, "task_id")? != task.id
            || field(output, "handoff_run_id")? != source.run_id
            || field(output, "failed_step_id")? != checkpoint.failed_step_id
            || field(output, "checkpoint_owner")? != source.run_id
            || field(worktree, "job_run_id")? != source.run_id
            || field(output, "original_base_sha")? != field(worktree, "base_sha")?
            || !task.spec_digest_matches(field(output, "task_spec_digest")?)
        {
            return Err(refuse("failure-handoff ownership, failed step or task specification disagrees; retain the original host receipt"));
        }
        let branch = field(output, "branch")?;
        let head = field(output, "head_sha")?;
        let base_sha = field(output, "target_base_sha")?;
        let number = field(output, "pr_number")?;
        let base_branch = source
            .input
            .as_ref()
            .and_then(|i| i.get("base_branch"))
            .and_then(Value::as_str)
            .unwrap_or("main");
        let prior = if source
            .input
            .as_ref()
            .is_some_and(|i| i.get(SOURCE_REPAIR_KEY).is_some())
        {
            Some(self.source_repair_adopted_binding(
                &source,
                &task,
                worktree,
                authenticated_checkpoint(
                    &definition,
                    &state,
                    "resume_candidate",
                    "candidate_resume",
                )?,
            )?)
        } else {
            self.read_owner_publication_binding(&source.run_id, false)?
        };
        if branch
            != prior
                .as_ref()
                .map_or(field(worktree, "head_ref")?, |b| b.branch.as_str())
            || prior
                .as_ref()
                .is_some_and(|b| b.pull_request != number || b.base_branch != base_branch)
            || output["push"]["phase"] != "push"
            || output["push"]["branch"] != branch
            || output["push"]["local_sha"] != head
            || !matches!(
                output["push"]["decision"].as_str(),
                Some(
                    "performed_create"
                        | "performed_fast_forward"
                        | "performed_force_with_lease"
                        | "reused_current"
                )
            )
        {
            return Err(refuse(
                "failure-handoff publication branch, head, PR or push proof disagrees",
            ));
        }
        // A hook may commit dirty repairs after validation failed. Its immediate
        // parent must be the immutable setup base or a successful host candidate;
        // an arbitrary descendant supplied as a receipt is insufficient.
        let repo = &self.paths().repo_root;
        let preservation = output
            .get("preservation_commit_created")
            .and_then(Value::as_bool)
            .ok_or_else(|| refuse("failure-handoff preservation decision missing"))?;
        for sha in [head, base_sha, field(worktree, "base_sha")?] {
            if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(refuse(
                    "failure-handoff candidate/base identity is not exact",
                ));
            }
        }
        let parent = if preservation {
            if output
                .get("committed_files")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
            {
                return Err(refuse(
                    "failure-handoff preservation commit lacks changed paths",
                ));
            }
            let commit = git2::Repository::open(repo)
                .and_then(|r| {
                    r.find_commit(git2::Oid::from_str(head)?)
                        .map(|c| (c.parent_count(), c.parent_id(0)))
                })
                .map_err(|e| refuse(format!("failure-handoff commit unavailable: {e}")))?;
            if commit.0 != 1 {
                return Err(refuse(
                    "failure-handoff preservation must be an exact single-parent commit",
                ));
            }
            commit.1.map_err(|e| refuse(e.to_string()))?.to_string()
        } else {
            head.to_string()
        };
        if state.pipeline.get("commit").is_some_and(|v| !v.is_null()) {
            authenticated_checkpoint(&definition, &state, "commit", "git_commit")?;
        }
        let commit = self
            .observe_task_delivery(&task.id, Some(&source.run_id))?
            .commit;
        let mut allowed = vec![field(worktree, "base_sha")?.to_string()];
        if matches!(
            commit.status,
            CommitObservationStatus::Committed | CommitObservationStatus::AlreadyCommitted
        ) && commit.base_sha.as_deref() == Some(field(worktree, "base_sha")?)
            && let Some(committed) = &commit.head_sha
        {
            allowed.push(committed.clone());
            if state
                .pipeline
                .get("sync_base")
                .is_some_and(|v| !v.is_null())
            {
                let prepared =
                    authenticated_checkpoint(&definition, &state, "prepare_branch", "pr_prepare")?;
                let sync =
                    authenticated_checkpoint(&definition, &state, "sync_base", "git_rebase")?;
                if field(prepared, "head")? != branch
                    || field(sync, "head")? != branch
                    || field(prepared, "base")? != base_branch
                    || field(sync, "base")? != base_branch
                    || field(prepared, "base_sha")? != base_sha
                    || field(sync, "base_sha")? != base_sha
                    || field(prepared, "head_sha")? != field(sync, "head_sha_before")?
                    || !run_git(
                        repo,
                        &[
                            "merge-base",
                            "--is-ancestor",
                            committed,
                            field(prepared, "head_sha")?,
                        ],
                    )?
                    .success
                    || !run_git(
                        repo,
                        &[
                            "merge-base",
                            "--is-ancestor",
                            base_sha,
                            field(sync, "head_sha")?,
                        ],
                    )?
                    .success
                    || sync.get("rewritten").and_then(Value::as_bool)
                        != Some(field(sync, "head_sha_before")? != field(sync, "head_sha")?)
                {
                    return Err(refuse(
                        "failure-handoff preceding host commit/synchronization disagrees",
                    ));
                }
                let synchronized = field(sync, "head_sha")?;
                allowed.push(synchronized.into());
                // A before-PR reviewer may advance the synchronized candidate.
                // Reuse normal publication's settlement and exact-head host
                // validation contract; the hook's push alone certifies no work.
                let checkpoint = |id: &str, action: &str| {
                    authenticated_checkpoint(&definition, &state, id, action)
                };
                let reviewed = super::publication::reviewed_head(
                    &definition,
                    &state,
                    &checkpoint,
                    false,
                    synchronized,
                    (branch, base_sha),
                    repo,
                )?;
                allowed.push(reviewed.into());
            }
        }
        if !allowed.contains(&parent)
            || !run_git(
                repo,
                &[
                    "merge-base",
                    "--is-ancestor",
                    field(worktree, "base_sha")?,
                    head,
                ],
            )?
            .success
        {
            return Err(refuse(
                "failure-handoff candidate lacks an authenticated preservation parent",
            ));
        }
        for id in ["push", "landing_push"] {
            if state.pipeline.get(id).is_some_and(|v| !v.is_null()) {
                let published = authenticated_checkpoint(&definition, &state, id, "git_push")?;
                if field(published, "phase")? != "push"
                    || field(published, "branch")? != branch
                    || !run_git(
                        repo,
                        &[
                            "merge-base",
                            "--is-ancestor",
                            field(published, "local_sha")?,
                            head,
                        ],
                    )?
                    .success
                {
                    return Err(refuse(
                        "failure-handoff contradicts preceding host publication",
                    ));
                }
            }
        }
        if let Some(pr) = state.pipeline.get("pr_open").filter(|v| !v.is_null()) {
            let authenticated =
                authenticated_checkpoint(&definition, &state, "pr_open", "pr_open")?;
            if pr != authenticated
                || pr["pr_number"] != number
                || pr["head"] != branch
                || pr["base"] != base_branch
            {
                return Err(refuse(
                    "failure-handoff contradicts preceding PR publication",
                ));
            }
        }
        let result = self.bind_source_publication(
            source.clone(),
            task.clone(),
            state.clone(),
            definition.clone(),
            PublicationIdentity {
                branch,
                head,
                base_sha,
                base_branch,
                number,
                allow_missing_branch: true,
            },
        )?;
        if field(output, "pr_url")? != result.2.pull_request_url
            || prior.as_ref().is_some_and(|b| {
                b.remote_url != result.2.remote_url || b.repository != result.2.repository
            })
        {
            return Err(refuse("failure-handoff PR repository identity disagrees"));
        }
        Ok(result)
    }
}
