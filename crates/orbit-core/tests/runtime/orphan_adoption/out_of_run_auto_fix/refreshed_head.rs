use orbit_engine::RunLifecycle;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use orbit_core::runtime::task_pr_forge::{LeasePullRequest, PullRequestRefreshOutcome};

use super::*;

struct RefreshForge(FakeForge);

impl TaskPrForge for RefreshForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        let state = self.0.0.lock().unwrap();
        Ok(state
            .blockers
            .values()
            .map(|blockers| blockers.pull.clone())
            .collect())
    }

    fn lease_pull_request(
        &self,
        _: &Path,
        binding: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        Ok(Some(LeasePullRequest {
            number: binding.trim_start_matches('#').parse().unwrap(),
            base_branch: "main".into(),
            merged: false,
        }))
    }

    fn pull_request_blockers(
        &self,
        repo: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        self.0.pull_request_blockers(repo, number)
    }

    fn update_pull_request_branch(
        &self,
        repo: &Path,
        number: u64,
        head_branch: &str,
        base_branch: &str,
        expected_head: &str,
    ) -> Result<PullRequestRefreshOutcome, OrbitError> {
        let mut state = self.0.0.lock().unwrap();
        let observed = state.blockers.get_mut(&number).unwrap();
        assert_eq!(observed.head_sha, expected_head);
        assert_eq!(observed.pull.head_branch, head_branch);
        let base = git(repo, &["rev-parse", base_branch]);
        let head_sha = clean_merge(repo, expected_head, &base, "Orbit refresh");
        observed.head_sha.clone_from(&head_sha);
        Ok(PullRequestRefreshOutcome::Updated { head_sha })
    }

    fn authenticated_login(&self, repo: &Path) -> Result<String, OrbitError> {
        self.0.authenticated_login(repo)
    }

    fn close_pull_request(
        &self,
        repo: &Path,
        number: u64,
        comment: &str,
    ) -> Result<(), OrbitError> {
        self.0.close_pull_request(repo, number, comment)
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let result = crate::git_test_support::run(repo, args).unwrap();
    assert!(result.success, "git {args:?}: {}", result.stderr);
    result.stdout.trim().to_string()
}

fn clean_merge(repo: &Path, head: &str, base: &str, message: &str) -> String {
    let output = git(repo, &["merge-tree", "--write-tree", head, base]);
    let tree = output.lines().next().unwrap();
    git(
        repo,
        &["commit-tree", tree, "-p", head, "-p", base, "-m", message],
    )
}

fn report_completion(bin: &Path, leaf: &Leaf, head: &str, landed: &str) {
    let write = |name: &str, value: &Value| {
        std::fs::write(bin.join(name), value.to_string()).unwrap();
    };
    let mut status = json!({
        "number": leaf.number, "state": "OPEN", "isDraft": false, "mergeStateStatus": "CLEAN",
        "reviewDecision": "APPROVED",
        "url": format!("https://github.com/owner/repository/pull/{}", leaf.number),
        "baseRefName": "main", "headRefName": leaf.head, "headRefOid": head,
        "statusCheckRollup": [{"name": "Test", "status": "COMPLETED", "conclusion": "SUCCESS"}]
    });
    write("open.json", &status);
    status["state"] = json!("MERGED");
    status["mergeCommit"] = json!({"oid": landed});
    status["mergedAt"] = json!("2026-11-23T11:06:00Z");
    write("merged.json", &status);
    write("merge-result.json", &json!({"merged":true,"sha":landed}));
    write(
        "checks.json",
        &json!({"data":{"repository":{"pullRequest":{"headRefOid":head,"commits":{"nodes":[
            {"commit":{"oid":head,"statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},
                "nodes":[{"__typename":"CheckRun","name":"Test","status":"COMPLETED",
                    "conclusion":"SUCCESS","isRequired":true}]}}}}
        ]}}}}}),
    );
}

fn out_of_run_resume_completes_a_green_audited_refresh() {
    let fx = fixture();
    let leaf = fx.delivered_leaf(
        198,
        "done",
        Shelf::Review,
        LeafEnd::Held(DeliveryRefusal::CompletionBudgetExpired.error_code()),
    );
    orbit_core::test_support::EngineHost::new(&fx.runtime)
        .apply_task_automation_update(
            &leaf.task,
            TaskAutomationUpdate {
                execution_summary: Some(
                    "Outcome: success\nChanges: Published the delivery file.".into(),
                ),
                ..Default::default()
            },
        )
        .unwrap();
    let repo = fx._root.path().join("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["commit", "--allow-empty", "-qm", "fork point"]);
    let fork = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-qb", &leaf.head]);
    std::fs::write(repo.join("delivery.txt"), "candidate").unwrap();
    git(&repo, &["add", "delivery.txt"]);
    git(&repo, &["commit", "-qm", "publish candidate"]);
    let published = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("base.txt"), "new base").unwrap();
    git(&repo, &["add", "base.txt"]);
    git(&repo, &["commit", "-qm", "advance base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let origin = fx._root.path().join("origin.git");
    git(
        &repo,
        &["clone", "-q", "--bare", ".", origin.to_str().unwrap()],
    );
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );

    let mut state = fx.jobs.read_run_state(&leaf.run).unwrap().unwrap();
    let push = republished(&leaf, &published, &fork);
    state.record_step(1, JobRunState::Success, Some(push.clone()), None);
    state.record_pipeline_output("push", push);
    fx.jobs.write_run_state(&leaf.run, &state).unwrap();
    fx.report_required_checks(&leaf, &published, "orbit-bot", &[], &["Test"]);
    let refresh_runtime = fx
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(RefreshForge(fx.forge.clone())));
    // The public refresh operation writes the same audit rows as the resident tool.
    let refreshed = refresh_runtime
        .refresh_task_pull_request(&leaf.task, Some("resident"))
        .unwrap();
    assert_eq!(refreshed.previous_head, published);
    assert!(refreshed.checks_started);
    fx.report_required_checks(&leaf, &refreshed.head_sha, "orbit-bot", &["Test"], &[]);

    let pass = fx.pass(&fx.drain(), "done", true);
    let entry = started(&pass, &leaf.task).unwrap_or_else(|| panic!("{pass}"));
    let resumed = &fx.retries(&leaf.run)[0];
    assert_eq!(entry["resumed_run_id"], resumed.run_id, "{pass}");
    let seeded = fx.jobs.read_run_state(&resumed.run_id).unwrap().unwrap();
    assert_eq!(seeded.next_step_index, 4);
    assert_eq!(seeded.pipeline["push"]["local_sha"], published);
    let mut input = resumed.input.clone().unwrap();
    input["job_run_id"] = seeded.pipeline["worktree"]["job_run_id"].clone();
    input["run_id"] = json!(resumed.run_id);
    input["workspace_path"] = json!(repo);
    input["completed_task_ids"] = seeded.pipeline["promote_tasks"]["performed_task_ids"].clone();
    input["pr_number"] = seeded.pipeline["pr_open"]["pr_number"].clone();
    input["head"] = seeded.pipeline["pr_open"]["head"].clone();
    input["base"] = seeded.pipeline["pr_open"]["base"].clone();
    input["published_head_sha"] = seeded.pipeline["push"]["local_sha"].clone();
    input["base_sync"] = json!("local");
    input["max_wait_seconds"] = json!(0);

    let bin = PathBuf::from(std::env::var_os("HOME").unwrap()).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, r#"#!/bin/sh
cd "$HOME/bin" || exit 98
case "$*" in
 'pr view'*) if [ -f merge-request ]; then cat merged.json; else cat open.json; fi ;;
 'repo view'*) echo '{"nameWithOwner":"owner/repository"}' ;;
 'api graphql'*isRequired*) cat checks.json ;;
 'api graphql'*) echo '{"data":{"repository":{"autoMergeAllowed":false,"mergeCommitAllowed":true,"rebaseMergeAllowed":false,"squashMergeAllowed":false,"pullRequest":{"baseRefName":"main","baseRef":{"branchProtectionRule":null}}}}}' ;;
 'api repos/'*'/merge'*) printf '%s\n' "$@" > merge-request; cat merge-result.json ;;
 *) exit 99 ;;
esac
"#).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let complete = || {
        orbit_engine::execute_deterministic_action(
            &orbit_core::test_support::EngineHost::new(&fx.runtime),
            "pr_complete",
            &json!({}),
            &input,
            false,
            &Default::default(),
            None,
        )
    };
    // Even an otherwise identical clean base merge needs its own audit edge.
    let unaudited = clean_merge(&repo, &published, &base, "Unaudited refresh");
    report_completion(&bin, &leaf, &unaudited, &base);
    let refused = complete().unwrap_err();
    assert!(
        matches!(
            refused,
            OrbitError::DeliveryRefused {
                refusal: DeliveryRefusal::HeadChanged,
                ..
            }
        ),
        "{refused}"
    );
    assert!(!bin.join("merge-request").exists());

    let landed = clean_merge(&repo, &base, &refreshed.head_sha, "Land delivery");
    report_completion(&bin, &leaf, &refreshed.head_sha, &landed);
    let output = complete().unwrap();
    assert_eq!(
        output["merge"]["delivery_evidence"]["head_sha"],
        refreshed.head_sha
    );
    assert_eq!(output["completed_task_ids"], json!([leaf.task]));
    assert_eq!(
        fx.runtime.get_task(&leaf.task).unwrap().status,
        TaskStatus::Done
    );
    let request = std::fs::read_to_string(bin.join("merge-request")).unwrap();
    assert!(
        request
            .lines()
            .any(|arg| arg == format!("sha={}", refreshed.head_sha)),
        "{request}"
    );
}

crate::scenario_table!(
    refreshed_delivery_scenarios,
    out_of_run_resume_completes_a_green_audited_refresh
);
