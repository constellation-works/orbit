//! The owner's `deliveries_landed` batches name the tasks handoff landings
//! delivered [ORB-13894].

use super::*;

use orbit_core::application::automation::evaluate_auto_task;
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{ExternalRef, TaskComplexity};
use orbit_types::workflow::automation::{DirectLandingRequest, UNATTRIBUTED_NO_LANDING_TASK};

const REPOSITORY: &str = "owner/repository";
const CONSUMER: &str = "landed-review";

/// A follower's pull request carries no reference on the owner's task: the
/// owner's accepted handoff is what names the task it delivered. Once that PR
/// lands, the owner's next review batch lists it under the claimed task. An
/// owner-run PR keeps the task its promotion stamped, an owner-local handoff
/// that fast-forwards keeps the task its landing intent names, even across a
/// retried attempt, and a PR no record names is reported as unattributed,
/// with the reason.
#[test]
fn the_owners_review_batch_names_the_tasks_handoff_landings_delivered() {
    if !isolated(
        module_path!(),
        "the_owners_review_batch_names_the_tasks_handoff_landings_delivered",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let owner = &pair.wire.owner;
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let claimed = pair.claimed_task(&leaf);
    pair.leaf_hands_off(&leaf);
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert_eq!(pair.owner_status(&claimed), "review");

    // Promotion stamps an owner-run PR on the task it delivers.
    let owner_run = owner
        .add_task(TaskAddParams {
            title: "Owner-run delivery".into(),
            acceptance_criteria: vec!["Lands through a pull request.".into()],
            complexity: TaskComplexity::Low,
            external_refs: vec![ExternalRef::github_pr("7").unwrap()],
            ..TaskAddParams::default()
        })
        .unwrap()
        .id
        .to_string();
    let owner_local = owner
        .add_task(TaskAddParams {
            title: "Owner-local claimed delivery".into(),
            acceptance_criteria: vec!["Lands by fast-forward.".into()],
            complexity: TaskComplexity::Low,
            ..TaskAddParams::default()
        })
        .unwrap()
        .id
        .to_string();

    let repo = &pair.owner_repo;
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "baseline"]);
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{REPOSITORY}.git"),
        ],
    );
    owner
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": CONSUMER,
                "schedule": {"deliveries_landed": {
                    "branch": "main",
                    "owner_machine": OWNER,
                    "threshold": 4,
                    "max_wait_minutes": 60,
                    "coverage": "landed_code_review_v1",
                    "max_items": 20,
                    "retries": 0,
                }},
                "template": {"title": "Review landed deliveries"},
            }),
        )
        .expect("consumer");
    owner.auto_task_toggle(CONSUMER, true).unwrap();
    let definition = owner.auto_task_show(CONSUMER).unwrap().unwrap();
    publish_origin(repo);
    let baseline = evaluate_auto_task(owner, &definition, false, Utc::now()).expect("baseline");
    assert!(baseline.state.is_some(), "{baseline:#?}");

    // Each PR squash-merges as one first-parent commit; the stand-in `gh`
    // answers the provider's commit-to-PR lookup from these records.
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let pulls = home.join("pulls");
    std::fs::create_dir_all(&pulls).unwrap();
    for pr in [42_u64, 7, 9] {
        std::fs::write(repo.join("landed.txt"), format!("landed by #{pr}\n")).unwrap();
        git(repo, &["add", "landed.txt"]);
        git(
            repo,
            &["commit", "-q", "-m", &format!("Squash-merge #{pr}")],
        );
        let sha = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
        let response = json!([{
            "number": pr,
            "html_url": format!("https://github.com/{REPOSITORY}/pull/{pr}"),
            "merge_commit_sha": sha,
            "merged_at": "2026-10-04T00:00:00Z",
            "base": {"ref": "main", "repo": {"full_name": REPOSITORY}},
        }]);
        std::fs::write(pulls.join(format!("{sha}.json")), response.to_string()).unwrap();
    }

    // An owner-local candidate fast-forwards main. The landing job retains
    // its intent first; a retried attempt runs under a new run id and
    // re-records the same intent rather than a second one.
    let before = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    git(repo, &["checkout", "-q", "-b", "orbit/local"]);
    std::fs::write(repo.join("local.txt"), "landed locally\n").unwrap();
    git(repo, &["add", "local.txt"]);
    git(repo, &["commit", "-q", "-m", "Owner-local candidate"]);
    let after = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    git(repo, &["checkout", "-q", "main"]);
    for run_id in ["jrun-landing", "jrun-landing-retry"] {
        owner
            .record_direct_landing_intent(&DirectLandingRequest {
                run_id: run_id.into(),
                branch: "main".into(),
                before_commit: before.clone(),
                after_commit: after.clone(),
                task_ids: vec![owner_local.clone()],
                handoff_id: Some("handoff-local".into()),
            })
            .unwrap();
    }
    git(repo, &["merge", "-q", "--ff-only", "orbit/local"]);

    std::fs::create_dir_all(home.join("bin")).unwrap();
    let gh = home.join("bin/gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nsha=${2#*/commits/}\nexec cat \"$HOME/pulls/${sha%%/*}.json\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    publish_origin(repo);
    let diagnostic = evaluate_auto_task(owner, &definition, false, Utc::now()).unwrap();
    let state = diagnostic
        .state
        .clone()
        .unwrap_or_else(|| panic!("no consumer state: {diagnostic:#?}"));
    let batch = state
        .active
        .unwrap_or_else(|| panic!("no batch admitted: {:#?}", state.unresolved))
        .batch;
    let attributed = batch
        .deliveries
        .iter()
        .map(|delivery| {
            (
                delivery.key.as_str(),
                delivery.task_ids.clone(),
                delivery.unattributed.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attributed,
        vec![
            ("pr:owner/repository:main:42", vec![claimed], None),
            ("pr:owner/repository:main:7", vec![owner_run], None),
            (
                "pr:owner/repository:main:9",
                vec![],
                Some(UNATTRIBUTED_NO_LANDING_TASK)
            ),
            (
                "direct:owner/repository:main:handoff-local",
                vec![owner_local],
                None
            ),
        ]
    );
}
