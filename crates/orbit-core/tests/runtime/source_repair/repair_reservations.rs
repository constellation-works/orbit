//! Abandoned CI repairs must stop reserving unrelated remediation paths.
use super::*;
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, TaskPrForge};
use orbit_types::task::ContextWideningStep;
use std::path::Path;
use std::sync::Arc;

struct OpenPublication;
impl TaskPrForge for OpenPublication {
    fn open_pull_requests(
        &self,
        _: &Path,
    ) -> Result<Vec<ForgePullRequest>, orbit_core::OrbitError> {
        Ok(Vec::new()) // Recorded PRs must also be observed individually.
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, orbit_core::OrbitError> {
        unreachable!()
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), orbit_core::OrbitError> {
        unreachable!()
    }
}

fn admission(f: &Fixture) -> Value {
    f.runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            orbit_tools::ToolContext::default(),
        )
        .unwrap()
}

fn published_repair_releases_only_after_verified_drop() {
    published_repair_drop(false);
}

fn pulled_repair_releases_only_after_verified_drop() {
    published_repair_drop(true);
}

fn pull_repair(f: &Fixture) -> orbit_core::application::distributed::TaskPullResponse {
    use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
    let session = ToolSessionContext {
        caller_machine_id: Some("hm_follower".into()),
        process_machine_id: Some("hm_fixture".into()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: std::collections::BTreeSet::from([McpCapability::Agent]),
        ..Default::default()
    };
    let probe = f
        .runtime
        .drain_probe(&session, &Default::default())
        .unwrap();
    let request = serde_json::from_value(json!({
        "request_id": "repair-reservations-pull",
        "caller_version": probe.binary_version, "caller_schema": probe.protocol_schema,
        "caller_fingerprint": probe.protocol_fingerprint,
        "review_gate": true, "ship": probe.ship,
        "run_context": {"run_id": "repair-reservations-drain", "job_name": "workspace_pull_pipeline"}
    })).unwrap();
    f.runtime.serve_task_pull(&session, &request).unwrap()
}

fn published_repair_drop(use_pull: bool) {
    let mut f = Fixture::drained();
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(OpenPublication));
    std::fs::write(f.repo.join("logging.txt"), "CI repair\n").unwrap();
    git(&f.repo, &["add", "logging.txt"]);
    git(&f.repo, &["commit", "-qm", "CI repair"]);
    git(&f.repo, &["push", "-q", "origin", &f.branch]);
    let repaired_head = git(&f.repo, &["rev-parse", "HEAD"]);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(repaired_head);
    std::fs::write(&f.answer, serde_json::to_vec(&answer).unwrap()).unwrap();
    f.runtime
        .widen_task_context_files(
            &f.task,
            &f.source.run_id,
            ContextWideningStep::Recovery,
            "step_failure_recovery",
            &["logging.txt".into()],
        )
        .unwrap();
    let repair = f
        .runtime
        .add_task(TaskAddParams {
            title: "Independent logging remediation".into(),
            description: "Repair shared logging".into(),
            acceptance_criteria: vec!["Logging fixed".into()],
            plan: "Repair logging".into(),
            context_files: vec!["file:logging.txt".into()],
            complexity: orbit_core::TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    let selected = |output: &Value| {
        output["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(repair.id))
    };
    let conflict = admission(&f);
    assert!(!selected(&conflict), "published repair still holds logging");
    assert!(
        conflict["excluded"].as_array().unwrap().contains(&json!({
            "id": repair.id, "reason": "context_lock_conflict",
            "conflicts": [{"requested_file": "file:logging.txt", "locking_task_id": f.task}]
        })),
        "the stale reservation belongs to the original CSS/source task: {conflict}"
    );

    // A local restoration alone cannot release a repair still on the PR.
    git(&f.repo, &["reset", "--hard", &f.head]);
    assert!(
        !selected(&admission(&f)),
        "remote still publishes the repair"
    );
    git(
        &f.repo,
        &["push", "-q", "--force-with-lease", "origin", &f.branch],
    );
    answer["headRefOid"] = json!(f.head);
    std::fs::write(&f.answer, serde_json::to_vec(&answer).unwrap()).unwrap();

    // Even an observed clean publication cannot release an active repair.
    let active = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            2,
            Utc::now(),
            Some(json!({"task_ids":[f.task]})),
            Some(f.source.run_id.clone()),
        )
        .unwrap();
    assert!(
        !selected(&admission(&f)),
        "a pending successor still owns the repair"
    );
    f.runtime
        .finalize_job_run(&active.run_id, JobRunState::Cancelled, Utc::now(), Some(0))
        .unwrap();
    // An unreadable provider answer is outcome-unknown, even with no leaf.
    std::fs::write(&f.answer, "{}").unwrap();
    assert!(
        !selected(&admission(&f)),
        "unknown publication must retain the repair"
    );
    std::fs::write(&f.answer, serde_json::to_vec(&answer).unwrap()).unwrap();

    // The owner drain may still be running with an explicit task selection;
    // it is not a repair leaf and must be able to admit the remediation.
    let drain = f
        .runtime
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids": [f.task]})),
            None,
        )
        .unwrap();
    if use_pull {
        let response = pull_repair(&f);
        assert_eq!(
            response.receipt.claim.as_ref().map(|claim| &claim.task_id),
            Some(&repair.id),
            "a follower pull must reconcile the same stale footprint: {response:?}"
        );
    } else {
        assert!(
            selected(&admission(&f)),
            "a stopped repair absent from the observed PR must release its reservation"
        );
        assert!(selected(&admission(&f)), "cleanup is idempotent");
        let grant = f
            .runtime
            .run_deterministic(
                "reserve_locks",
                &json!({}),
                &json!({"task_ids": [repair.id]}),
                orbit_tools::ToolContext::default(),
            )
            .unwrap();
        assert_eq!(
            grant["reserved"], true,
            "the authoritative task lock boundary admits the independent repair: {grant}"
        );
    }
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().context_files,
        ["file:source.txt"]
    );
    f.runtime
        .finalize_job_run(&drain.run_id, JobRunState::Cancelled, Utc::now(), Some(0))
        .unwrap();
}

fn refused_unpublished_repair_releases_after_worktree_restoration() {
    for discard in [false, true] {
        let mut f = Fixture::drained();
        f.runtime = f
            .runtime
            .clone()
            .with_task_pr_forge(Arc::new(OpenPublication));
        // The agent-exit widening precedes scope-record validation. A refused
        // commit has no repair checkpoint or repair publication to clean up.
        let candidate = orbit_engine::run_worktree_paths(&f.repo, &f.source)
            .unwrap()
            .remove(0);
        std::fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        git(
            &f.repo,
            &[
                "worktree",
                "add",
                "--detach",
                candidate.to_str().unwrap(),
                &f.head,
            ],
        );
        std::fs::write(candidate.join("logging.txt"), "uncommittable CI repair\n").unwrap();
        std::fs::write(f.repo.join("logging.txt"), "shared logging\n").unwrap();
        f.runtime
            .widen_task_context_files(
                &f.task,
                &f.source.run_id,
                ContextWideningStep::Recovery,
                "step_failure_recovery",
                &["logging.txt".into()],
            )
            .unwrap();
        let repair = f
            .runtime
            .add_task(TaskAddParams {
                title: "Independent logging remediation".into(),
                description: "Repair shared logging".into(),
                acceptance_criteria: vec!["Logging fixed".into()],
                plan: "Repair logging".into(),
                context_files: vec!["file:logging.txt".into()],
                complexity: orbit_core::TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
        let selected = |output: &Value| {
            output["task_ids"]
                .as_array()
                .unwrap()
                .contains(&json!(repair.id))
        };
        assert!(
            !selected(&admission(&f)),
            "uncommitted recovery edits still protect their paths"
        );
        if discard {
            f.runtime
                .update_task_as_human(
                    &f.task,
                    orbit_core::application::task::TaskUpdateParams {
                        discard_candidate: true,
                        ..Default::default()
                    },
                    "operator".into(),
                )
                .unwrap();
        } else {
            std::fs::remove_file(candidate.join("logging.txt")).unwrap();
        }
        assert!(
            selected(&admission(&f)),
            "a refused repair absent from both candidate and publication must release"
        );
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().context_files,
            ["file:source.txt"]
        );
    }
}

crate::scenario_table!(
    abandoned_repair_reservation_scenarios,
    published_repair_releases_only_after_verified_drop,
    pulled_repair_releases_only_after_verified_drop
);

crate::scenario_table!(
    abandoned_unpublished_repair_reservation_scenarios,
    refused_unpublished_repair_releases_after_worktree_restoration
);
