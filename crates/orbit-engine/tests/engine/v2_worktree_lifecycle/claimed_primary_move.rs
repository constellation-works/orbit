use super::{
    Fixture, agent_loop, audit_writer, branch, commit_file, git, head, integrity_diagnostic,
    isolated_in, read, status,
};
use orbit_engine::{ResolvedSandbox, execute_job_with_resume};
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::task::ExecutionLocation;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::{ClaimFailureClass, ExecutorSandboxKind, HeldFailure};
use serde_json::json;
use std::fs;

#[test]
fn claimed_primary_move_holds_only_when_confined_and_disjoint() {
    isolated_in(
        module_path!(),
        "claimed_primary_move_holds_only_when_confined_and_disjoint",
        || {
            for (confined, task_selector, primary_path) in [
                (true, None, "unrelated.txt"),
                (true, None, "README.md"),
                (true, Some("dir:."), "unrelated.txt"),
                (
                    true,
                    Some("module:orbit_engine::activity_job::workspace::boundary_guard"),
                    "module.rs",
                ),
                (false, None, "unrelated.txt"),
            ] {
                check_primary_move(confined, task_selector, primary_path);
            }
        },
    );
}

fn check_primary_move(confined: bool, task_selector: Option<&str>, primary_path: &str) {
    let task_id = "T-PRIMARY-MOVE";
    let run_id = "jrun-claimed-primary-move";
    let (fixture, host, checkout) = Fixture::with_checkout(task_id, run_id);
    let assigned_head = head(&checkout.path);
    let assigned_branch = branch(&checkout.path);
    fs::write(checkout.path.join("README.md"), "retained candidate\n").unwrap();
    let assigned_status = status(&checkout.path);

    let primary_before = head(&fixture.repo);
    git(&fixture.repo, &["checkout", "-b", "operator-work"]);
    commit_file(&fixture.repo, primary_path, "operator work\n");
    let primary_after = head(&fixture.repo);
    git(&fixture.repo, &["checkout", super::BASE]);

    let mut host = fixture.with_provider(&host, "");
    if let Some(task_selector) = task_selector {
        host.publish_task_context = true;
        host.edit_task(task_id, |task| {
            task.context_files.push(task_selector.to_string());
        });
    }
    host.worker = Some(WorkerInvocation {
        owner_machine_id: "owner".to_string(),
        owner_workspace_id: "workspace".to_string(),
        owner_destination: "owner".to_string(),
        task_id: task_id.to_string(),
        claim_id: "claim".to_string(),
        execution: ExecutionLocation {
            machine_id: "worker".to_string(),
            machine_name: None,
        },
        bound_run_id: run_id.to_string(),
    });
    // The host changes the primary after the guard captures it, before the
    // substitute provider launches. The provider leaves both checkouts alone.
    host.primary_branch_at_agent_launch = Some("operator-work".to_string());
    host.sandbox = confined.then(|| ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: ResolvedFsProfile {
            name: "implementer".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![format!("{}/**", checkout.path.display())],
        },
        allow_fallback: false,
        managed_worktree: true,
        runtime_write_authority: Vec::new(),
        mask: None,
    });

    let job = orbit_engine::activity_job::load_job_asset(
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "task_claimed_pr_pipeline"},
            "spec": {"state": "enabled", "kind": "workflow", "steps": [{
                "id": "implement_bundle",
                "loop": {
                    "items": "{{ input.task_ids }}",
                    "max_iterations": 1,
                    "steps": [{
                        "id": "implement_one",
                        "spec": agent_loop("Use the assigned checkout."),
                    }],
                },
            }]},
        })
        .to_string(),
    )
    .unwrap();
    let input = json!({
        "task_id": task_id,
        "task_ids": [task_id],
        "workspace_path": checkout.path,
        "repo_root": checkout.path,
    });
    let (_, audit) = audit_writer(run_id, fixture.root.path());
    let outcome = orbit_engine::test_support::with_post_run_guard_without_user_namespace(|| {
        execute_job_with_resume(&job.spec, input, run_id, audit, &host, None)
    });

    assert_ne!(primary_before, primary_after);
    assert_eq!(head(&fixture.repo), primary_after);
    assert_eq!(head(&checkout.path), assigned_head);
    assert_eq!(branch(&checkout.path), assigned_branch);
    assert_eq!(status(&checkout.path), assigned_status);
    assert_eq!(
        read(checkout.path.join("README.md")),
        "retained candidate\n"
    );

    if confined && task_selector.is_none() && primary_path == "unrelated.txt" {
        let outcome = outcome.expect("a confined, disjoint primary move holds the run");
        assert!(!outcome.success);
        let code = outcome.failure_code.expect("typed hold code");
        assert_eq!(
            HeldFailure::of_claimed_leaf(Some(code.as_str()), None),
            Some(HeldFailure::ClaimedPrimaryCheckoutMoved)
        );
        assert_eq!(
            ClaimFailureClass::of_step_failure_code(Some(&code), None),
            Some(ClaimFailureClass::Transient)
        );
    } else {
        let error = outcome.expect_err("a conflicting or unconfined primary move fails the run");
        let diagnostic = integrity_diagnostic(&error, "primary_checkout_drift");
        if confined && task_selector == Some("dir:.") {
            assert_eq!(diagnostic["conflicting_paths"], json!([primary_path]));
        }
    }
}
