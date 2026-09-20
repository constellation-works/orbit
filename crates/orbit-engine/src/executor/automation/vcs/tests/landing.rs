//! The owner landing activity [ORB-12499].
//!
//! Every case drives the real `handoff_land` against a self-contained Git
//! repository and the shared fake provider transport: the `pr.status` answers
//! are scripted, so the provider states, the merge request and the lost reply
//! are exercised without GitHub. The fake host also stands in for the owner
//! coordination store's guards — one unresolved merge intent at a time, no
//! completion while one is outstanding, and no authority decision without an
//! observation of the accepted candidate.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{HandoffArtifactRef, HandoffCandidate, HandoffDelivery};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use super::super::landing::handoff_land;
use super::super::pr::tests::test_support::{
    PR_MERGE_OPERATION, PrOpenTestHost, git, review_batch_task,
};
use crate::context::{HandoffLandingContext, HandoffLandingStep};

const HANDOFF: &str = "handoff-test";
const SOURCE_BRANCH: &str = "orbit/test-batch";
const LANDING_BRANCH: &str = "agent-main";

struct Owner {
    _temp: TempDir,
    repo: PathBuf,
}

/// An owner checkout with `origin`, a landing branch, and a candidate branch
/// one commit ahead of it. `remote` decides whether the candidate is published,
/// which is the difference between a pull-request delivery and an owner-local
/// one.
fn owner_checkout(remote: bool) -> Owner {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let origin = temp.path().join("origin.git");
    git(temp.path(), &["init", "--bare", &origin.to_string_lossy()]);
    fs::create_dir_all(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", LANDING_BRANCH]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(repo.join("README.md"), "base\n").expect("write readme");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "base"]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["push", "-u", "origin", LANDING_BRANCH]);
    git(&repo, &["checkout", "-b", SOURCE_BRANCH]);
    fs::write(repo.join("candidate.txt"), "candidate\n").expect("write candidate");
    git(&repo, &["add", "candidate.txt"]);
    git(&repo, &["commit", "-m", "candidate work"]);
    if remote {
        git(&repo, &["push", "-u", "origin", SOURCE_BRANCH]);
    }
    // The landing branch is what a landing merges into, so leave it checked out
    // the way an owner checkout waiting for work is.
    git(&repo, &["checkout", LANDING_BRANCH]);
    Owner { _temp: temp, repo }
}

fn revision(repo: &Path, reference: &str) -> SourceRevision {
    SourceRevision {
        commit: git(repo, &["rev-parse", &format!("{reference}^{{commit}}")]),
        tree: git(repo, &["rev-parse", &format!("{reference}^{{tree}}")]),
    }
}

fn accepted_candidate(repo: &Path, delivery: HandoffDelivery) -> HandoffCandidate {
    HandoffCandidate {
        repository: "orbit/test".into(),
        source_branch: SOURCE_BRANCH.into(),
        base_branch: LANDING_BRANCH.into(),
        landing_branch: LANDING_BRANCH.into(),
        candidate: revision(repo, SOURCE_BRANCH),
        base: revision(repo, LANDING_BRANCH),
        delivery,
    }
}

fn context(repo: &Path, candidate: HandoffCandidate) -> HandoffLandingContext {
    HandoffLandingContext {
        handoff_id: HANDOFF.into(),
        task_id: "T1".into(),
        claim_id: "claim-1".into(),
        candidate,
        unresolved_merge_intent: None,
        workspace_path: repo.to_path_buf(),
    }
}

fn landing_host(context: HandoffLandingContext) -> PrOpenTestHost {
    let repo = context.workspace_path.clone();
    PrOpenTestHost::new(vec![review_batch_task("T1", None, None)], repo)
        .with_landing_context(context)
}

fn input() -> Value {
    json!({ "handoff_id": HANDOFF, "task_id": "T1", "max_wait_seconds": 10 })
}

fn open_state(merge_state: &str, head: &str) -> Value {
    json!({
        "number": 42,
        "state": "OPEN",
        "mergeStateStatus": merge_state,
        "headRefName": SOURCE_BRANCH,
        "headRefOid": head,
        "baseRefName": LANDING_BRANCH,
    })
}

fn merged_state(head: &str) -> Value {
    json!({
        "number": 42,
        "state": "MERGED",
        "mergedAt": "2026-09-20T00:00:00Z",
        "mergeStateStatus": "CLEAN",
        "headRefName": SOURCE_BRANCH,
        "headRefOid": head,
        "baseRefName": LANDING_BRANCH,
        "mergeCommit": { "oid": "1".repeat(40) },
    })
}

fn stop_evidence(host: &PrOpenTestHost) -> String {
    host.landing_updates()
        .into_iter()
        .find(|update| update.step == HandoffLandingStep::Stop)
        .map(|update| update.evidence)
        .expect("a stopped landing records why")
}

fn merge_calls(host: &PrOpenTestHost) -> Vec<Value> {
    host.vcs_calls()
        .into_iter()
        .filter(|call| call.operation == PR_MERGE_OPERATION)
        .map(|call| call.input)
        .collect()
}

#[test]
fn a_pull_request_lands_only_after_the_intent_and_a_verified_merge() {
    let owner = owner_checkout(true);
    let candidate = accepted_candidate(&owner.repo, HandoffDelivery::PullRequest { number: 42 });
    let head = candidate.candidate.commit.clone();
    let host = landing_host(context(&owner.repo, candidate));
    host.queue_pr_status([
        open_state("PENDING", &head),
        open_state("CLEAN", &head),
        merged_state(&head),
    ]);
    host.queue_merge_capabilities(true, true, true, false);

    let output = handoff_land(&host, &input()).expect("authorized landing");

    assert_eq!(output["phase"], "landed");
    assert_eq!(
        host.landing_steps(),
        vec!["publish_intent", "resolve_intent:true", "complete"],
        "the intent is durable before the merge and resolved by what the provider reported"
    );
    let merges = merge_calls(&host);
    assert_eq!(merges.len(), 1, "one external merge for one handoff");
    assert_eq!(merges[0]["auto"], false, "auto-merge cannot pin the head");
    assert_eq!(
        merges[0]["reviewed_head_sha"], head,
        "the provider mutation carries the pinned candidate"
    );
    assert_eq!(
        output["evidence"]["delivery"]["merge_commit"],
        "1".repeat(40)
    );
    assert_eq!(host.unresolved_merge_intent(), None);
}

#[test]
fn protection_conflict_and_a_moved_head_stop_before_any_merge() {
    /// One scripted provider answer about the pinned candidate, and the
    /// refusal its state must produce.
    type ProviderCase = (fn(&str) -> Value, &'static str);

    let cases: Vec<ProviderCase> = vec![
        (|head| open_state("BLOCKED", head), "branch protection"),
        (|head| open_state("DIRTY", head), "conflicts with its base"),
        (
            |_| open_state("CLEAN", &"9".repeat(40)),
            "delivery_evidence_stale",
        ),
        (
            |head| {
                json!({
                    "number": 42, "state": "CLOSED", "mergeStateStatus": "",
                    "headRefName": SOURCE_BRANCH, "headRefOid": head,
                    "baseRefName": LANDING_BRANCH,
                })
            },
            "closed without merging",
        ),
        (
            |head| {
                json!({
                    "number": 42, "state": "OPEN", "mergeStateStatus": "CLEAN",
                    "headRefName": SOURCE_BRANCH, "headRefOid": head,
                    "baseRefName": "other-base",
                })
            },
            "delivery_evidence_stale",
        ),
    ];
    for (status, expected) in cases {
        let owner = owner_checkout(true);
        let candidate =
            accepted_candidate(&owner.repo, HandoffDelivery::PullRequest { number: 42 });
        let head = candidate.candidate.commit.clone();
        let host = landing_host(context(&owner.repo, candidate));
        host.queue_pr_status([status(&head)]);
        host.queue_merge_capabilities(true, true, true, false);

        let error = handoff_land(&host, &input()).expect_err("landing must stop");

        let evidence = stop_evidence(&host);
        assert!(
            evidence.contains(expected),
            "expected {expected} in {evidence}"
        );
        assert!(error.to_string().contains(expected), "{error}");
        assert!(
            merge_calls(&host).is_empty(),
            "a stopped landing asks for no merge"
        );
        assert_eq!(host.landing_steps(), vec!["stop"]);
    }
}

#[test]
fn an_exhausted_check_budget_stops_with_the_candidate_still_in_review() {
    let owner = owner_checkout(true);
    let candidate = accepted_candidate(&owner.repo, HandoffDelivery::PullRequest { number: 42 });
    let head = candidate.candidate.commit.clone();
    let host = landing_host(context(&owner.repo, candidate));
    host.queue_pr_status([open_state("PENDING", &head)]);

    let mut input = input();
    input["max_wait_seconds"] = json!(0);
    let error = handoff_land(&host, &input).expect_err("the budget is not a merge");

    assert!(error.to_string().contains("did not reach a merged state"));
    assert!(merge_calls(&host).is_empty());
    assert_eq!(host.landing_steps(), vec!["stop"]);
}

#[test]
fn a_lost_merge_reply_is_reconciled_against_the_provider_before_anything_retries() {
    for (reconciled, expected) in [
        (true, "resolve_intent:true"),
        (false, "resolve_intent:false"),
    ] {
        let owner = owner_checkout(true);
        let candidate =
            accepted_candidate(&owner.repo, HandoffDelivery::PullRequest { number: 42 });
        let head = candidate.candidate.commit.clone();
        let host = landing_host(context(&owner.repo, candidate));
        host.queue_pr_status([open_state("CLEAN", &head)]);
        host.queue_merge_capabilities(true, true, true, false);
        host.fail_vcs(
            PR_MERGE_OPERATION,
            "connection reset after the request was sent",
        );

        // The reply never arrives: the run fails with the intent outstanding.
        let error = handoff_land(&host, &input()).expect_err("an unacknowledged merge fails");
        assert!(error.to_string().contains("connection reset"), "{error}");
        assert_eq!(host.landing_steps(), vec!["publish_intent"]);
        assert!(
            host.unresolved_merge_intent().is_some(),
            "uncertainty outlives the run that created it"
        );

        // The next attempt reads what actually happened before doing anything.
        host.clear_vcs_error(PR_MERGE_OPERATION);
        if reconciled {
            host.queue_pr_status([merged_state(&head)]);
            let output = handoff_land(&host, &input()).expect("reconciled merge completes");
            assert_eq!(output["evidence"]["reconciled"], "pull_request_merged");
            assert_eq!(host.landing_steps()[1..], [expected, "complete"]);
            assert_eq!(
                merge_calls(&host).len(),
                1,
                "a merge that already happened is not requested again"
            );
        } else {
            host.queue_pr_status([open_state("BLOCKED", &head), open_state("BLOCKED", &head)]);
            handoff_land(&host, &input()).expect_err("an open pull request stays in review");
            assert_eq!(host.landing_steps()[1..], [expected, "stop"]);
        }
        assert_eq!(host.unresolved_merge_intent(), None);
    }
}

#[test]
fn an_owner_local_candidate_fast_forwards_its_landing_ref_or_stops() {
    let owner = owner_checkout(false);
    let candidate = accepted_candidate(&owner.repo, HandoffDelivery::LocalCandidate);
    let head = candidate.candidate.commit.clone();
    let host = landing_host(context(&owner.repo, candidate));

    let output = handoff_land(&host, &input()).expect("local landing");

    assert_eq!(output["phase"], "landed");
    assert_eq!(
        host.landing_steps(),
        vec!["publish_intent", "resolve_intent:true", "complete"]
    );
    assert_eq!(
        git(&owner.repo, &["rev-parse", LANDING_BRANCH]),
        head,
        "the landing branch actually moved to the candidate"
    );

    // A landing branch that moved on is not fast-forwardable, and the owner
    // does not rebase unvalidated code to make it one.
    let diverged = owner_checkout(false);
    let diverged_candidate = accepted_candidate(&diverged.repo, HandoffDelivery::LocalCandidate);
    fs::write(diverged.repo.join("other.txt"), "other\n").expect("write other");
    git(&diverged.repo, &["add", "other.txt"]);
    git(&diverged.repo, &["commit", "-m", "landing branch moved on"]);
    let moved = git(&diverged.repo, &["rev-parse", LANDING_BRANCH]);
    let diverged_host = landing_host(context(&diverged.repo, diverged_candidate));

    let error = handoff_land(&diverged_host, &input()).expect_err("no silent rebase");

    assert!(error.to_string().contains("cannot fast-forward"), "{error}");
    assert_eq!(
        diverged_host.landing_steps(),
        vec!["publish_intent", "resolve_intent:false", "stop"],
        "the uncertain send is resolved by reading the ref, then the attempt stops"
    );
    assert_eq!(git(&diverged.repo, &["rev-parse", LANDING_BRANCH]), moved);
}

#[test]
fn no_diff_delivery_completes_from_its_covering_commit_without_an_external_call() {
    let owner = owner_checkout(false);
    let covering = git(&owner.repo, &["rev-parse", LANDING_BRANCH]);
    let mut accepted = accepted_candidate(
        &owner.repo,
        HandoffDelivery::AlreadyLanded {
            covering_commit: covering.clone(),
            evidence: HandoffArtifactRef {
                path: "already-landed.json".into(),
                sha256: "0".repeat(64),
            },
        },
    );
    // No-diff delivery has nothing to merge: the candidate is the base.
    accepted.candidate = accepted.base.clone();
    let host = landing_host(context(&owner.repo, accepted.clone()));

    let output = handoff_land(&host, &input()).expect("no-diff landing");

    assert_eq!(output["evidence"]["external_merge"], false);
    assert_eq!(
        host.landing_steps(),
        vec!["complete"],
        "nothing external is sent, so no intent is recorded"
    );
    assert!(host.vcs_calls().is_empty(), "no provider call is made");

    // A covering commit that is not on the landing ref is not already landed.
    let other = owner_checkout(false);
    let mut unlanded = accepted.clone();
    unlanded.candidate = revision(&other.repo, LANDING_BRANCH);
    unlanded.base = unlanded.candidate.clone();
    unlanded.delivery = HandoffDelivery::AlreadyLanded {
        covering_commit: revision(&other.repo, SOURCE_BRANCH).commit,
        evidence: HandoffArtifactRef {
            path: "already-landed.json".into(),
            sha256: "0".repeat(64),
        },
    };
    let unlanded_host = landing_host(context(&other.repo, unlanded));

    let error = handoff_land(&unlanded_host, &input()).expect_err("unverified no-diff delivery");

    assert!(error.to_string().contains("not already landed"), "{error}");
    assert_eq!(unlanded_host.landing_steps(), vec!["stop"]);
}

#[test]
fn a_candidate_the_owner_cannot_read_stops_instead_of_landing() {
    let owner = owner_checkout(true);
    let mut rewritten =
        accepted_candidate(&owner.repo, HandoffDelivery::PullRequest { number: 42 });
    let head = rewritten.candidate.commit.clone();
    // The accepted tree is not the tree that commit carries: the candidate is
    // not the object that was validated.
    rewritten.candidate.tree = "2".repeat(40);
    let host = landing_host(context(&owner.repo, rewritten));
    host.queue_pr_status([open_state("CLEAN", &head)]);
    host.queue_merge_capabilities(true, true, true, false);

    let error = handoff_land(&host, &input()).expect_err("unverifiable candidate");

    assert!(
        error
            .to_string()
            .contains("but the accepted handoff recorded"),
        "{error}"
    );
    assert!(merge_calls(&host).is_empty());
    assert_eq!(host.landing_steps(), vec!["stop"]);
}
