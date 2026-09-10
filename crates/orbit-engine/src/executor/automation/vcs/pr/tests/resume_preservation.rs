use std::fs;
use std::sync::Arc;

use orbit_agent::loop_engine::audit::NullSink;
use orbit_types::workflow::PipelineState;
use serde_json::json;

use super::resume_failure::{
    CHECKPOINT_RUN_ID, FIRST_RESUME_RUN_ID, ResumeFailureHost, SECOND_RESUME_RUN_ID, TASK_ID,
    THIRD_RESUME_RUN_ID, completed_worktree_checkpoint, record_preservation_evidence,
    resumed_pr_delivery_job, task_owned_by,
};
use super::test_support::{PrOpenTestHost, git, no_diff_pr_workspace};
use crate::{DispatchError, V2AuditWriter, execute_job_with_resume};

fn execute_resume(host: &ResumeFailureHost, run_id: &str, state: &PipelineState) -> DispatchError {
    execute_job_with_resume(
        &resumed_pr_delivery_job(),
        json!({"task_ids": [TASK_ID]}),
        run_id,
        Arc::new(V2AuditWriter::new(run_id, "test-agent", Arc::new(NullSink))),
        host,
        Some(state),
    )
    .expect_err("invalid preservation evidence fails before implementation")
}

#[test]
fn resume_preflight_rejects_moved_head_without_preservation_evidence() {
    let workspace = no_diff_pr_workspace();
    let state = completed_worktree_checkpoint(SECOND_RESUME_RUN_ID, &workspace.repo);
    fs::write(workspace.repo.join("candidate.txt"), "unknown candidate\n")
        .expect("write candidate");
    git(&workspace.repo, &["add", "candidate.txt"]);
    git(&workspace.repo, &["commit", "-m", "unknown commit"]);
    let unknown_head = git(&workspace.repo, &["rev-parse", "HEAD"]);
    fs::write(workspace.repo.join("user-notes.txt"), "keep me\n").expect("write user notes");
    let status_before = git(&workspace.repo, &["status", "--porcelain"]);

    let host = ResumeFailureHost::new(
        PrOpenTestHost::new(
            vec![task_owned_by(CHECKPOINT_RUN_ID)],
            workspace.repo.clone(),
        )
        .with_job_run(CHECKPOINT_RUN_ID, None)
        .with_job_run(SECOND_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID)),
    );
    host.write_state(state.clone());

    let error = execute_resume(&host, SECOND_RESUME_RUN_ID, &state);

    assert!(
        error.to_string().contains("resume_preservation_unverified"),
        "{error}"
    );
    assert!(!workspace.repo.join("src/repaired.rs").exists());
    assert_eq!(git(&workspace.repo, &["rev-parse", "HEAD"]), unknown_head);
    assert_eq!(
        git(&workspace.repo, &["status", "--porcelain"]),
        status_before,
        "preflight preserves both the candidate and user changes",
    );
}

#[test]
fn resume_preflight_rejects_unrelated_lineage_and_changed_ownership() {
    for changed_owner in [false, true] {
        let workspace = no_diff_pr_workspace();
        let mut source = completed_worktree_checkpoint(SECOND_RESUME_RUN_ID, &workspace.repo);
        fs::write(
            workspace.repo.join("candidate.txt"),
            "preserved candidate\n",
        )
        .expect("write candidate");
        git(&workspace.repo, &["add", "candidate.txt"]);
        git(&workspace.repo, &["commit", "-m", "Orbit preservation"]);
        let preserved_head = git(&workspace.repo, &["rev-parse", "HEAD"]);
        record_preservation_evidence(&mut source, SECOND_RESUME_RUN_ID, &preserved_head);
        let mut resumed = source.clone();
        resumed.run_id = THIRD_RESUME_RUN_ID.to_string();
        fs::write(workspace.repo.join("user-notes.txt"), "keep me\n").expect("write user notes");
        let status_before = git(&workspace.repo, &["status", "--porcelain"]);

        let owner = if changed_owner {
            "jrun-new-owner"
        } else {
            CHECKPOINT_RUN_ID
        };
        let third_parent = if changed_owner {
            SECOND_RESUME_RUN_ID
        } else {
            FIRST_RESUME_RUN_ID
        };
        let host = ResumeFailureHost::new(
            PrOpenTestHost::new(vec![task_owned_by(owner)], workspace.repo.clone())
                .with_job_run(CHECKPOINT_RUN_ID, None)
                .with_job_run(FIRST_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID))
                .with_job_run(SECOND_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID))
                .with_job_run(THIRD_RESUME_RUN_ID, Some(third_parent)),
        );
        host.write_state(source);
        host.write_state(resumed.clone());

        let error = execute_resume(&host, THIRD_RESUME_RUN_ID, &resumed);

        let message = error.to_string();
        if changed_owner {
            assert!(message.contains("jrun-new-owner"), "{message}");
        } else {
            assert!(message.contains("not a retry descendant"), "{message}");
        }
        assert!(!workspace.repo.join("src/repaired.rs").exists());
        assert_eq!(
            git(&workspace.repo, &["status", "--porcelain"]),
            status_before,
            "candidate and user changes survive the refusal",
        );
    }
}

#[test]
fn resumed_recovery_requires_the_exact_immutable_source_checkpoint() {
    use crate::executor::automation::vcs::freshness::recovered_head_checkpoint;

    let workspace = no_diff_pr_workspace();
    let mut source = completed_worktree_checkpoint(FIRST_RESUME_RUN_ID, &workspace.repo);
    let base = git(&workspace.repo, &["rev-parse", "HEAD"]);
    fs::write(
        workspace.repo.join("candidate.txt"),
        "recovered candidate\n",
    )
    .unwrap();
    git(&workspace.repo, &["add", "candidate.txt"]);
    git(&workspace.repo, &["commit", "-m", "recovered candidate"]);
    let head = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let checkpoint = json!({
        "run_id": FIRST_RESUME_RUN_ID,
        "step_id": "sync_base",
        "task_ids": [TASK_ID],
        "workspace_path": workspace.repo,
        "head": "orbit/test-batch",
        "head_sha_before": "original-candidate",
        "original_base_sha": base,
        "base_sha": base,
        "head_sha": head,
        "rewritten": true,
    });
    source
        .rebase_recovery_checkpoints
        .insert("sync_base".to_string(), checkpoint.clone());
    let host = ResumeFailureHost::new(
        PrOpenTestHost::new(
            vec![task_owned_by(CHECKPOINT_RUN_ID)],
            workspace.repo.clone(),
        )
        .with_job_run(FIRST_RESUME_RUN_ID, None)
        .with_job_run(SECOND_RESUME_RUN_ID, Some(FIRST_RESUME_RUN_ID)),
    );
    host.write_state(source.clone());
    let mut resumed: PipelineState =
        serde_json::from_slice(&serde_json::to_vec(&source).unwrap()).unwrap();
    resumed.run_id = SECOND_RESUME_RUN_ID.to_string();
    host.write_state(resumed.clone());

    // The row exists in the run store the leaf can write, but nothing has
    // certified it yet. That is the pre-boundary shape (ORB-12015): it is not
    // authority, so it is treated as no usable checkpoint — not trusted, but
    // also not a hard refusal — which is what lets a resume redo the rebase
    // instead of getting stuck.
    assert_eq!(
        recovered_head_checkpoint(&host, SECOND_RESUME_RUN_ID, &workspace.repo, &head).unwrap(),
        None
    );

    host.inner
        .certify_recovery(FIRST_RESUME_RUN_ID, "sync_base", &checkpoint);
    assert_eq!(
        recovered_head_checkpoint(&host, SECOND_RESUME_RUN_ID, &workspace.repo, &head).unwrap(),
        Some(checkpoint)
    );
    assert_eq!(
        recovered_head_checkpoint(&host, SECOND_RESUME_RUN_ID, &workspace.repo, &base).unwrap(),
        None
    );

    resumed
        .rebase_recovery_checkpoints
        .get_mut("sync_base")
        .unwrap()["head_sha_before"] = json!("substituted-origin");
    host.write_state(resumed.clone());
    // The substituted payload has no certificate of its own either, so it is
    // just as unusable as the pre-boundary case above.
    assert_eq!(
        recovered_head_checkpoint(&host, SECOND_RESUME_RUN_ID, &workspace.repo, &head).unwrap(),
        None
    );

    // Certifying the substituted payload isolates the second gate, which is
    // what keeps a resume honest even about evidence the host did write: the
    // copy still has to equal the immutable source row.
    let substituted = resumed.rebase_recovery_checkpoints["sync_base"].clone();
    host.inner
        .certify_recovery(FIRST_RESUME_RUN_ID, "sync_base", &substituted);
    let error =
        recovered_head_checkpoint(&host, SECOND_RESUME_RUN_ID, &workspace.repo, &head).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("differs from its source checkpoint"),
        "{error}"
    );
}
