//! [ORB-15314] A claimed leaf stopped while its reviewer is working keeps
//! the reviewer's uncommitted repair and the attempt's findings, and the
//! task's next claim continues from them instead of starting over.
//!
//! Cancelling the leaf commits the reviewer's edits as a marked partial
//! repair on top of the candidate head, under a candidate ref on `origin`,
//! without touching the candidate branch or the worktree. The release keeps
//! the candidate with that ref; the owner adds the findings of the
//! attempt's report. The next claim — on another host, or after a forced
//! drain cancel on the same one — applies the repair onto its base and hands
//! the implementer the findings, continuing an earlier pull request on its
//! branch rather than opening a second.

use orbit_core::application::job::JobRunCancelOutcome;
use orbit_types::workflow::{REVIEW_ADMISSION_KEY, ReviewAdmission, ReviewVerdict};

use super::candidate_carry::{
    bare, claim_candidate, engine_action, has_commit, host_checkout, owner_published,
    settled_candidate,
};
use super::claimed_review::ReviewedLeaf;
use super::*;

const SECOND: &str = "hm_second";
/// The reviewer's edit of the candidate, uncommitted when the leaf stops.
const REPAIRED: &str = "fn work() {}\n// repaired by the reviewer\n";
/// A file the reviewer added, untracked when the leaf stops.
const ADDED: &str = "src/f0_test.rs";

/// The run state a claimed PR leaf records up to a running review: the
/// candidate committed on `branch` at `head` and synchronized onto `base`.
fn reviewing(pair: &Pair, leaf: &str, repo: &Path, branch: &str, head: &str, base: &str) -> Value {
    let steps = json!({
        "worktree": {"workspace_path": repo, "head_ref": branch, "base_sha": base},
        "commit": {"commit_sha": head},
        "prepare_branch": {"head": branch, "head_sha": head, "base": "main", "base_sha": base},
        "sync_base": {"head": branch, "head_sha": head, "base": "main", "base_sha": base},
    });
    let mut state = pair.follower.read_run_state(leaf).unwrap().unwrap();
    for (step, output) in steps.as_object().unwrap() {
        state.pipeline[step] = output.clone();
    }
    pair.follower.write_run_state(leaf, &state).unwrap();
    steps
}

/// Record step `step`'s output on `leaf`'s run state.
fn record_step(pair: &Pair, leaf: &str, step: &str, output: &Value) {
    let mut state = pair.follower.read_run_state(leaf).unwrap().unwrap();
    state.pipeline[step] = output.clone();
    pair.follower.write_run_state(leaf, &state).unwrap();
}

/// The reviewer's work so far, left uncommitted in `repo`.
fn reviewer_edits(repo: &Path) {
    std::fs::write(repo.join("src/f0.rs"), REPAIRED).unwrap();
    std::fs::write(repo.join(ADDED), "#[test]\nfn works() {}\n").unwrap();
}

/// Cancelling a claimed leaf inside its before-PR review keeps the
/// reviewer's uncommitted repair at a candidate ref on `origin` and the
/// attempt's findings with the released candidate; the task's next claim,
/// on a host with its own object store, applies that repair onto its base
/// and hands the implementer the open findings.
fn a_cancelled_review_keeps_its_repair_and_findings_for_the_next_claim() {
    let (mut reviewed, worker) = ReviewedLeaf::admit_with_worker();
    let pair = &reviewed.pair;
    let (leaf, task) = (reviewed.leaf.clone(), reviewed.task.clone());
    let repo = pair.follower_repo.clone();
    let origin_url = git(&repo, &["config", "--get", "remote.origin.url"]);
    let origin = bare(pair);
    git(
        origin.parent().unwrap(),
        &["init", "--bare", "-q", origin.to_str().unwrap()],
    );
    git(
        &repo,
        &[
            "config",
            &format!("url.{}.insteadOf", origin.display()),
            origin_url.trim(),
        ],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let base = reviewed.base.commit.clone();
    let branch = git(&repo, &["branch", "--show-current"]).trim().to_string();
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    reviewing(pair, &leaf, &repo, &branch, &head, &base);

    let admitted = reviewed.admit_review();
    assert_eq!(admitted["applies"], true, "{admitted}");
    let attempt = admitted["attempt_id"].as_str().unwrap().to_string();
    record_step(&reviewed.pair, &leaf, "review_gate_admit", &admitted);
    // The reviewer persisted a report with an open finding, then was
    // stopped with its fix unfinished and uncommitted.
    reviewed.reviewer_reports(&attempt, ReviewVerdict::Incomplete, false);
    let pair = &reviewed.pair;
    reviewer_edits(&repo);

    let cancel = pair
        .follower
        .cancel_job_run_with_options_and_policy(
            &leaf,
            "operator",
            "cli",
            Some("host maintenance"),
            false,
            false,
        )
        .expect("cancel the leaf");
    assert_eq!(cancel.outcome, JobRunCancelOutcome::Cancelled, "{cancel:?}");
    assert!(worker.stopped(), "the leaf's worker is stopped");

    // The repair is a reviewer-authored commit on the candidate head, kept
    // on `origin`; the candidate branch and the worktree are untouched.
    let repair_ref = format!("refs/orbit/candidates/{task}/{leaf}-review");
    let repair = git(&origin, &["rev-parse", &repair_ref]).trim().to_string();
    assert_eq!(
        git(&origin, &["rev-parse", &format!("{repair}^")]).trim(),
        head,
        "the repair sits on the reviewed candidate"
    );
    let identity = git(&origin, &["log", "-1", "--format=%an|%cn", &repair]);
    let (author, committer) = identity.trim().split_once('|').unwrap();
    assert!(author.ends_with("-reviewer"), "{identity}");
    assert_eq!(committer, "orbit", "{identity}");
    assert_eq!(
        git(&origin, &["show", &format!("{repair}:src/f0.rs")]),
        REPAIRED
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(
        git(&repo, &["branch", "--show-current"]).trim(),
        branch,
        "the candidate branch did not move"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("src/f0.rs")).unwrap(),
        REPAIRED,
        "the worktree keeps the reviewer's edits"
    );

    // The release keeps the candidate, the repair and the findings.
    assert_eq!(pair.owner_status(&task), "backlog");
    let kept = settled_candidate(pair, &leaf, "Release");
    assert_eq!(kept["head_sha"], head.as_str(), "{kept}");
    assert_eq!(kept["durable_ref"], repair_ref.as_str(), "{kept}");
    let interrupted = &kept["interrupted_review"];
    assert_eq!(interrupted["step_id"], "review", "{kept}");
    assert_eq!(interrupted["attempt_id"], attempt.as_str(), "{kept}");
    assert_eq!(interrupted["repair_ref"], repair_ref.as_str(), "{kept}");
    assert_eq!(interrupted["repair_commit"], repair.as_str(), "{kept}");
    assert!(
        comments_of(&pair.owner_task(&task)).contains(&repair_ref),
        "the release names where the repair is kept: {}",
        pair.owner_task(&task)
    );

    let second = pair.another_host(SECOND);
    let checkout = host_checkout(pair, &second);
    assert!(!has_commit(&checkout, &head));
    let drain = second.run_drain();
    let next = second.running_leaf(&drain, 1);
    assert_eq!(second.claimed_task(&next), task, "the task is pulled again");
    let offered = claim_candidate(&second, &next).expect("the claim carries the candidate");
    let review = offered
        .interrupted_review
        .as_ref()
        .expect("the candidate carries the stopped review");
    assert_eq!(review.verdict, Some(ReviewVerdict::Incomplete));
    assert_eq!(
        review
            .findings
            .iter()
            .map(|finding| finding.id.as_str())
            .collect::<Vec<_>>(),
        ["F1"],
        "the owner added the attempt's findings"
    );
    let input = second
        .follower_jobs
        .get_job_run(&next)
        .unwrap()
        .and_then(|run| run.input)
        .expect("leaf input");
    // The resumed leaf runs under the review admission captured when it was
    // admitted, not the stopped leaf's.
    let admission = ReviewAdmission::from_run_input(&input)
        .unwrap()
        .expect("the resumed leaf carries its own review admission");
    assert!(admission.gates_pr(), "{}", input[REVIEW_ADMISSION_KEY]);

    let resumed = engine_action(
        &second.follower,
        "candidate_resume",
        &json!({
            "job_run_id": next,
            "task_ids": [task],
            "workspace_path": checkout,
            "base_sha": base,
            "candidate": input["resume_candidate"],
            "claimed": true,
        }),
    );
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["implement"], true, "{resumed}");
    let repair_input = &resumed["repair"];
    assert_eq!(repair_input["trigger"], "review", "{resumed}");
    assert_eq!(
        repair_input["interrupted_review"]["repair_applied"], true,
        "{resumed}"
    );
    assert_eq!(
        repair_input["interrupted_review"]["findings"][0]["id"], "F1",
        "{resumed}"
    );
    assert_eq!(
        repair_input["interrupted_review"]["findings"][0]["disposition"]["kind"], "open",
        "{resumed}"
    );
    assert_eq!(resumed["source_sha"], head.as_str(), "{resumed}");
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]).trim(),
        base,
        "the leaf starts on its own base"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("src/f0.rs")).unwrap(),
        REPAIRED,
        "the reviewer's repair is applied for the implementer to continue"
    );
    assert!(
        checkout.join(ADDED).is_file(),
        "the reviewer's new file too"
    );
}

/// A forced drain cancel records its leaf's release before stopping it,
/// naming the ref the stop then keeps the running before-landing reviewer's
/// repair under. The task's next claim continues the published branch and
/// its pull request, with that repair applied: no second pull request and
/// no re-implementation from the base.
fn a_forced_drain_cancel_keeps_a_landing_reviewers_repair_on_the_published_candidate() {
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let base = owner_published(&pair);
    let first = host_checkout(&pair, &pair);
    let branch = format!("orbit/{task}-aaaaaaaa");
    git(&first, &["checkout", "-q", "-b", &branch]);
    std::fs::write(first.join("src/f0.rs"), "fn work() { todo!() }\n").unwrap();
    git(&first, &["commit", "-q", "-am", "candidate"]);
    git(&first, &["push", "-q", "origin", &branch]);
    let head = git(&first, &["rev-parse", "HEAD"]).trim().to_string();

    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), task);
    reviewing(&pair, &leaf, &first, &branch, &head, &base);
    for (step, output) in [
        ("review_gate_admit", json!({"applies": false})),
        ("review_gate_settle", json!({"applies": false})),
        ("validate", json!({"decision": "passed"})),
        ("push", json!({"branch": branch, "local_sha": head})),
        ("pr_open", json!({"pr_number": "41"})),
        ("landing_review_gate_admit", {
            let mut admitted = crate::review_admission_shape::admitted_review(
                "landing-attempt-1",
                "lineage",
                &head,
            );
            admitted["reviewer"] = json!({
                "crew": "sol", "crew_source": "workspace", "provider": "claude",
                "model": "claude-opus-4", "reasoning_effort": null,
            });
            admitted
        }),
    ] {
        record_step(&pair, &leaf, step, &output);
    }
    reviewer_edits(&first);
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &drain_worker.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(drain_worker.stopped());

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), true)
        .expect("forced drain cancel");
    assert_eq!(cancel.forced_runs, vec![leaf.clone()], "{cancel:?}");
    assert!(worker.stopped());

    let repair_ref = format!("refs/orbit/candidates/{task}/{leaf}-review");
    let kept = settled_candidate(&pair, &leaf, "Release");
    assert_eq!(kept["published"], true, "{kept}");
    assert_eq!(kept["pull_request"], "41", "{kept}");
    assert_eq!(kept["head_sha"], head.as_str(), "{kept}");
    let interrupted = &kept["interrupted_review"];
    assert_eq!(interrupted["step_id"], "landing_review", "{kept}");
    assert_eq!(interrupted["attempt_id"], "landing-attempt-1", "{kept}");
    assert_eq!(
        interrupted["repair_ref"],
        repair_ref.as_str(),
        "the release names the ref its stop writes: {kept}"
    );
    let repair = git(&bare(&pair), &["rev-parse", &repair_ref])
        .trim()
        .to_string();
    assert_eq!(
        git(&bare(&pair), &["rev-parse", &format!("{repair}^")]).trim(),
        head
    );
    assert_eq!(
        git(
            &bare(&pair),
            &["rev-parse", &format!("refs/heads/{branch}")]
        )
        .trim(),
        head,
        "the published branch did not move"
    );
    assert_eq!(pair.owner_status(&task), "backlog");

    let drain = pair.run_drain();
    let next = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&next), task, "the task is pulled again");
    let input = pair
        .follower_jobs
        .get_job_run(&next)
        .unwrap()
        .and_then(|run| run.input)
        .expect("leaf input");
    let checkout = first.with_file_name("next-claim");
    let ours = format!("orbit/{task}-bbbbbbbb");
    git(
        &first,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &ours,
            checkout.to_str().unwrap(),
            &base,
        ],
    );
    let resumed = engine_action(
        &pair.follower,
        "candidate_resume",
        &json!({
            "job_run_id": next,
            "task_ids": [task],
            "workspace_path": checkout,
            "base_sha": base,
            "candidate": input["resume_candidate"],
            "claimed": true,
        }),
    );
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["repair"]["trigger"], "review", "{resumed}");
    assert_eq!(
        resumed["repair"]["interrupted_review"]["repair_commit"],
        repair.as_str(),
        "{resumed}"
    );
    assert_eq!(resumed["prior_pull_request"], "41", "{resumed}");
    assert_eq!(resumed["reused_branch"], branch.as_str(), "{resumed}");
    assert_eq!(resumed["reused_head_sha"], head.as_str(), "{resumed}");
    assert_eq!(
        std::fs::read_to_string(checkout.join("src/f0.rs")).unwrap(),
        REPAIRED,
        "the reviewer's repair is applied on the continued branch"
    );
    assert!(checkout.join(ADDED).is_file());
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]).trim(), base);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_cancelled_review_keeps_its_repair_and_findings_for_the_next_claim_scenarios,
    a_cancelled_review_keeps_its_repair_and_findings_for_the_next_claim,
    a_forced_drain_cancel_keeps_a_landing_reviewers_repair_on_the_published_candidate
);
