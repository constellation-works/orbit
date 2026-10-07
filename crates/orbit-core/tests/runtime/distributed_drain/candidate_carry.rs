//! [ORB-14338] A claimed leaf's committed candidate survives a release to
//! another host.
//!
//! Each host has its own repository and object store, sharing only a bare
//! `origin`. The first host commits a candidate and fails before its push;
//! its failure hook carries the candidate to a durable ref on `origin`, the
//! release names that ref, and a claim on the second host fetches and resumes
//! the same commit. When the carry fails, the owner keeps the candidate for
//! the host that has it and records why a claim elsewhere implements fresh.
//!
//! [ORB-14603] The owner's own run of a task whose claim failed continues
//! the candidate that claim kept, and never reads a local run that shares the
//! follower leaf's id.

use orbit_types::task::{CANDIDATE_RESUME_EVENT, TaskStatus};

use super::*;

const BRANCH: &str = "orbit/carried-candidate";
const ORIGIN_URL: &str = "https://github.com/owner/repository.git";
const SECOND: &str = "hm_second";
const CARRIED: &str = "fn work() { carried(); }\n";
/// The candidate an unrelated owner run under the follower leaf's id
/// preserved; resuming it would be reading the wrong machine's run.
const UNRELATED: &str = "0123456789abcdef0123456789abcdef01234567";

fn engine_action(host: &OrbitRuntime, action: &str, input: &Value) -> Value {
    orbit_engine::execute_deterministic_action(
        host,
        action,
        &json!({}),
        input,
        false,
        &Default::default(),
        None,
    )
    .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// The owner's repository published to its bare `origin`; returns the base.
fn owner_published(pair: &Pair) -> String {
    let repo = &pair.owner_repo;
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["remote", "add", "origin", ORIGIN_URL]);
    publish_origin(repo);
    git(repo, &["rev-parse", "HEAD"]).trim().to_string()
}

/// `host`'s own clone of `origin`'s `main`, in its own object store.
fn host_checkout(pair: &Pair, host: &Pair) -> PathBuf {
    let repo = host.follower_repo.clone();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", ORIGIN_URL]);
    git(
        &repo,
        &[
            "config",
            &format!("url.{}.insteadOf", bare(pair).display()),
            ORIGIN_URL,
        ],
    );
    git(&repo, &["fetch", "-q", "origin", "main"]);
    git(&repo, &["reset", "-q", "--hard", "origin/main"]);
    repo
}

fn bare(pair: &Pair) -> PathBuf {
    pair.owner_repo.with_file_name("origin.git")
}

fn has_commit(repo: &Path, sha: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .status()
        .unwrap()
        .success()
}

/// Host A's claimed leaf commits a candidate on its branch in `repo` and stops before
/// its push: its pipeline state holds the worktree, commit and prepared
/// branch, and its failure hook runs over that state the way the engine runs
/// a job's `failure_activity`, its output checkpointed on the run. Returns
/// the drain, the leaf, the candidate commit and the hook's output.
fn committed_then_failed(pair: &Pair, repo: &Path, base: &str) -> (String, String, String, Value) {
    committed_then_stopped(pair, repo, base, false)
}

/// [`committed_then_failed`], or with `at_review` a leaf that synchronized
/// its candidate and stopped in its before-PR review: a candidate failure,
/// which the owner settles by blocking the task.
fn committed_then_stopped(
    pair: &Pair,
    repo: &Path,
    base: &str,
    at_review: bool,
) -> (String, String, String, Value) {
    let task = pair.tasks[0].clone();
    git(repo, &["checkout", "-q", "-b", BRANCH]);
    std::fs::write(repo.join("src/f0.rs"), CARRIED).unwrap();
    git(repo, &["commit", "-q", "-am", "candidate"]);
    let head = git(repo, &["rev-parse", "HEAD"]).trim().to_string();

    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), task);
    let mut steps = json!({
        "worktree": {"workspace_path": repo, "head_ref": BRANCH, "base_sha": base},
        "commit": {"commit_sha": head},
        "prepare_branch": {"head": BRANCH, "head_sha": head, "base": "main", "base_sha": base},
    });
    let (failed_step, error) = if at_review {
        steps["sync_base"] =
            json!({"head": BRANCH, "head_sha": head, "base": "main", "base_sha": base});
        steps["review_gate_admit"] = json!({"admitted": true});
        (
            "review_gate_settle",
            "the before-PR review timed out with a partial report",
        )
    } else {
        (
            "sync_base",
            "[validation_environment] required validation 'make ci-fast' could not run: cargo \
             is missing",
        )
    };
    let mut state = pair.follower.read_run_state(&leaf).unwrap().unwrap();
    for (step, output) in steps.as_object().unwrap() {
        state.pipeline[step] = output.clone();
    }
    let carry = engine_action(
        &pair.follower,
        "claim_candidate_carry",
        &json!({
            "failed_step_id": failed_step,
            "activity_name": "git_rebase",
            "error_code": "execution_failed",
            "error_message": error,
            "job_input": {"task_ids": [task]},
            "pipeline": steps,
            "run_id": leaf,
        }),
    );
    state.record_failure_activity(
        "claim_candidate_carry".into(),
        failed_step.into(),
        carry.clone(),
    );
    pair.follower.write_run_state(&leaf, &state).unwrap();
    pair.leaf_fails_with(&leaf, error);
    pair.pass_over(&drain, json!({"window_expired": true}));
    (drain, leaf, head, carry)
}

/// The release the owner recorded for `leaf`'s claim.
fn released_candidate(pair: &Pair, leaf: &str) -> Value {
    let claim_id = pair
        .admission(leaf)
        .receipt
        .and_then(|receipt| receipt.claim)
        .expect("claim")
        .claim_id;
    pair.wire
        .calls("orbit.drain.claim.settle")
        .into_iter()
        .rev()
        .find(|settle| settle["claim_id"] == claim_id.as_str())
        .map(|settle| settle["settlement"]["Release"]["failure"]["candidate"].clone())
        .expect("the leaf's release was delivered")
}

fn claim_candidate(pair: &Pair, leaf: &str) -> Option<orbit_store::contracts::ClaimCandidateRef> {
    pair.admission(leaf)
        .receipt
        .and_then(|receipt| receipt.task)
        .and_then(|task| task.resume_candidate)
}

/// A candidate committed on one host and released before its push is carried
/// to a durable ref on `origin`, and the task's next claim on a host with its
/// own object store resumes that same commit instead of implementing anew.
#[test]
fn a_carried_candidate_resumes_on_another_host() {
    if !isolated(
        module_path!(),
        "a_carried_candidate_resumes_on_another_host",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let base = owner_published(&pair);
    let first = host_checkout(&pair, &pair);
    let (_, leaf, head, carry) = committed_then_failed(&pair, &first, &base);

    let reference = format!("refs/orbit/candidates/{task}/{leaf}");
    assert_eq!(carry["carry"], "durable", "{carry}");
    assert_eq!(carry["durable_ref"], reference.as_str(), "{carry}");
    assert_eq!(
        git(&bare(&pair), &["rev-parse", &reference]).trim(),
        head,
        "origin holds the candidate at its durable ref"
    );
    assert_eq!(pair.owner_status(&task), "backlog");
    let released = released_candidate(&pair, &leaf);
    assert_eq!(released["durable_ref"], reference.as_str(), "{released}");
    assert_eq!(released["head_sha"], head.as_str(), "{released}");
    assert!(
        comments_of(&pair.owner_task(&task)).contains(&reference),
        "the release evidence names the durable ref: {}",
        pair.owner_task(&task)
    );

    let second = pair.another_host(SECOND);
    let repo = host_checkout(&pair, &second);
    assert!(
        !has_commit(&repo, &head),
        "the second host's object store starts without the candidate"
    );
    let drain = second.run_drain();
    let next = second.running_leaf(&drain, 1);
    assert_eq!(second.claimed_task(&next), task, "the task is pulled again");
    let offered = claim_candidate(&second, &next).expect("the claim carries the candidate");
    assert_eq!(offered.durable_ref.as_deref(), Some(reference.as_str()));
    let input = second
        .follower_jobs
        .get_job_run(&next)
        .unwrap()
        .and_then(|run| run.input)
        .expect("leaf input");
    assert_eq!(
        input["resume_candidate"]["durable_ref"],
        reference.as_str(),
        "{input}"
    );

    let resumed = engine_action(
        &second.follower,
        "candidate_resume",
        &json!({
            "job_run_id": next,
            "task_ids": [task],
            "workspace_path": repo,
            "base_sha": base,
            "candidate": input["resume_candidate"],
            "claimed": true,
        }),
    );
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["repair"]["trigger"], "continuation", "{resumed}");
    assert_eq!(resumed["source_sha"], head.as_str(), "{resumed}");
    assert_eq!(resumed["source_run_id"], leaf.as_str(), "{resumed}");
    assert!(has_commit(&repo, &head), "the candidate was fetched");
    assert_eq!(
        std::fs::read_to_string(repo.join("src/f0.rs")).unwrap(),
        CARRIED,
        "the candidate's change is applied for the implementer to continue"
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), base);
}

/// When the candidate cannot be pushed to a durable ref, a claim on another
/// host implements fresh and the owner's task history says why, typed; the
/// host that holds the candidate is still offered it.
#[test]
fn an_uncarried_candidate_is_fresh_elsewhere_with_a_typed_reason() {
    if !isolated(
        module_path!(),
        "an_uncarried_candidate_is_fresh_elsewhere_with_a_typed_reason",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let base = owner_published(&pair);
    // Host A can fetch from `origin` but its pushes go nowhere.
    let first = host_checkout(&pair, &pair);
    let unreachable = pair.follower_repo.with_file_name("unreachable.git");
    git(
        &first,
        &[
            "config",
            "remote.origin.pushurl",
            unreachable.to_str().unwrap(),
        ],
    );
    let (_, leaf, head, carry) = committed_then_failed(&pair, &first, &base);

    assert_eq!(carry["carry"], "failed", "{carry}");
    assert!(
        carry["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "{carry}"
    );
    let released = released_candidate(&pair, &leaf);
    assert_eq!(released["head_sha"], head.as_str(), "{released}");
    assert!(released.get("durable_ref").is_none(), "{released}");
    assert!(
        released["carry_failure"].as_str().is_some(),
        "the release says why the candidate stayed on its host: {released}"
    );

    let second = pair.another_host(SECOND);
    let drain = second.run_drain();
    let next = second.running_leaf(&drain, 1);
    assert_eq!(second.claimed_task(&next), task);
    assert!(
        claim_candidate(&second, &next).is_none(),
        "another host cannot fetch the candidate, so it implements fresh"
    );
    let history = pair.wire.owner.get_task_history(&task).unwrap();
    let fresh = history
        .iter()
        .rev()
        .find(|entry| entry.event == CANDIDATE_RESUME_EVENT)
        .expect("the owner records why the claim implements fresh");
    let note = fresh.note.as_deref().unwrap_or_default();
    assert_eq!(fresh.by, SECOND);
    assert!(note.starts_with("fresh: "), "{note}");
    assert!(note.contains("reason=not_durable"), "{note}");
    assert!(note.contains(&head), "{note}");

    // The second host's leaf ends without a candidate; the first host, whose
    // object store holds it, is offered it again.
    second.leaf_fails_with(&next, "[transient_failure] the network dropped");
    second.pass_over(&drain, json!({"window_expired": true}));
    let retry_drain = pair.run_drain();
    let retry = pair.running_leaf(&retry_drain, 1);
    assert_eq!(pair.claimed_task(&retry), task);
    let offered = claim_candidate(&pair, &retry).expect("the holding host resumes it");
    assert_eq!(offered.head_sha, head);
}

/// On-call returns a task its claim's failure blocked to the backlog, as the
/// documented procedure for a follower's review failure does.
fn returned_to_backlog(pair: &Pair, task: &str) {
    pair.wire
        .owner
        .update_task_as_human(
            task,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:on-call".into(),
        )
        .expect("back to the backlog");
}

fn owner_jobs(pair: &Pair) -> Arc<dyn JobRunStoreBackend> {
    orbit_store::compose::workspace_job_run_store(
        pair.wire.owner.sqlite_store().unwrap(),
        pair.wire.owner.workspace_id().unwrap(),
    )
}

/// An unrelated run in the owner's own store under `run_id` — the follower
/// leaf's id, which the owner minted for its own work too [ORB-13649] —
/// whose failure handoff preserved another candidate for `task`.
fn owner_run_named(pair: &Pair, run_id: &str, task: &str) {
    let owner = &pair.wire.owner;
    let local = owner_jobs(pair)
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .unwrap()
        .run_id;
    let workspace_id = owner.workspace_id().unwrap();
    owner
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET run_id = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    [run_id, workspace_id.as_str(), local.as_str()],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    let mut state = PipelineState::new(run_id.into(), "task_pr_pipeline".into(), json!({}));
    state.record_failure_activity(
        "pr_failure_handoff".into(),
        "review".into(),
        json!({
            "decision": "blocked_failure_pr",
            "task_id": task,
            "branch": "orbit/unrelated",
            "head_sha": UNRELATED,
            "task_spec_digest": owner.get_task(task).unwrap().spec_digest(),
        }),
    );
    owner.write_run_state(run_id, &state).unwrap();
}

/// The owner's own `task_pr_pipeline` run of `task`: its checkout, then its
/// `resume_candidate` step, with the inputs the pipeline passes them.
fn owner_local_run(pair: &Pair, task: &str) -> (Value, Value) {
    let owner = &pair.wire.owner;
    let run = owner_jobs(pair)
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .unwrap()
        .run_id;
    let setup = engine_action(
        owner,
        "worktree_setup",
        &json!({
            "job_run_id": run,
            "run_id": run,
            "task_ids": [task],
            "base": "main",
            "base_sync": "local",
            "dependency_delivery": "ignore",
        }),
    );
    let resumed = engine_action(
        owner,
        "candidate_resume",
        &json!({
            "job_run_id": run,
            "task_ids": [task],
            "workspace_path": setup["workspace_path"],
            "base_sha": setup["base_sha"],
            "prior_job_run_id": setup["prior_job_run_id"],
            "prior_foreign_run": setup["prior_foreign_run"],
        }),
    );
    (setup, resumed)
}

/// The note of the owner's latest `candidate_resume` event for `task`.
fn resume_note(pair: &Pair, task: &str) -> String {
    pair.wire
        .owner
        .get_task_history(task)
        .unwrap()
        .iter()
        .rev()
        .find(|entry| entry.event == CANDIDATE_RESUME_EVENT)
        .and_then(|entry| entry.note.clone())
        .expect("the owner-local run records its candidate resume")
}

/// [ORB-14603] A claim that committed a candidate and failed in review blocks
/// the task; returned to the backlog, the task's next run is the owner's own
/// `task_pr_pipeline`. That run continues the follower's candidate — same
/// source run, same commit — instead of implementing anew, and never reads
/// the unrelated owner run that shares the leaf's id.
#[test]
fn an_owner_local_run_continues_a_failed_claims_candidate() {
    if !isolated(
        module_path!(),
        "an_owner_local_run_continues_a_failed_claims_candidate",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let base = owner_published(&pair);
    let follower = host_checkout(&pair, &pair);
    let (_, leaf, head, carry) = committed_then_stopped(&pair, &follower, &base, true);
    assert_eq!(carry["carry"], "durable", "{carry}");
    assert_eq!(pair.owner_status(&task), "blocked", "a candidate failure");
    let shown = pair.owner_task(&task);
    assert_eq!(shown["job_run_id"], leaf.as_str(), "{shown}");
    assert_eq!(shown["job_run_machine"]["machine_id"], FOLLOWER, "{shown}");
    owner_run_named(&pair, &leaf, &task);
    returned_to_backlog(&pair, &task);

    let (setup, resumed) = owner_local_run(&pair, &task);
    assert!(setup["prior_job_run_id"].is_null(), "{setup}");
    assert_eq!(
        setup["prior_foreign_run"],
        json!({"run_id": leaf, "machine_id": FOLLOWER}),
        "{setup}"
    );
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["implement"], true, "{resumed}");
    assert_eq!(resumed["repair"]["trigger"], "review", "{resumed}");
    assert_eq!(resumed["source_run_id"], leaf.as_str(), "{resumed}");
    assert_eq!(resumed["source_machine_id"], FOLLOWER, "{resumed}");
    assert_eq!(resumed["source_sha"], head.as_str(), "{resumed}");
    assert!(!resumed.to_string().contains(UNRELATED), "{resumed}");
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(checkout.join("src/f0.rs")).unwrap(),
        CARRIED,
        "the follower's change is applied for the implementer to continue"
    );
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]).trim(), base);

    let note = resume_note(&pair, &task);
    assert!(note.starts_with("resumed_repaired: "), "{note}");
    assert!(note.contains(&format!("machine={FOLLOWER}")), "{note}");
    assert!(note.contains(&format!("source_run={leaf}")), "{note}");
    assert!(note.contains(&format!("source_sha={head}")), "{note}");
    assert!(!note.contains(UNRELATED), "{note}");
}

/// [ORB-14603] A claim that failed before committing kept no candidate. The
/// owner's own run implements fresh, says the foreign machine's run kept
/// nothing, and does not resume what an unrelated owner run under the same
/// id preserved.
#[test]
fn an_owner_local_run_never_reads_a_local_run_sharing_the_leafs_id() {
    if !isolated(
        module_path!(),
        "an_owner_local_run_never_reads_a_local_run_sharing_the_leafs_id",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    owner_published(&pair);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    pair.leaf_fails_with(&leaf, "the implementer gave up");
    pair.pass_over(&drain, json!({"window_expired": true}));
    assert_eq!(pair.owner_status(&task), "blocked");
    owner_run_named(&pair, &leaf, &task);
    returned_to_backlog(&pair, &task);

    let (setup, resumed) = owner_local_run(&pair, &task);
    assert_eq!(
        setup["prior_foreign_run"]["machine_id"], FOLLOWER,
        "{setup}"
    );
    assert_eq!(resumed["outcome"], "fresh", "{resumed}");
    assert_eq!(resumed["reason_code"], "no_candidate", "{resumed}");
    let reason = resumed["reason"].as_str().unwrap_or_default();
    assert!(reason.contains(FOLLOWER), "{reason}");
    assert!(reason.contains(&leaf), "{reason}");
    assert!(resumed["source_sha"].is_null(), "{resumed}");
    assert!(!resumed.to_string().contains(UNRELATED), "{resumed}");
}

/// [ORB-14603] A kept candidate the owner cannot fetch — its follower could
/// not carry it to `origin` — is set aside by the owner's own run with a
/// typed reason in the task's history naming the claim and the machine that
/// holds it.
#[test]
fn an_owner_local_run_records_why_it_sets_a_kept_candidate_aside() {
    if !isolated(
        module_path!(),
        "an_owner_local_run_records_why_it_sets_a_kept_candidate_aside",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let base = owner_published(&pair);
    let follower = host_checkout(&pair, &pair);
    let unreachable = pair.follower_repo.with_file_name("unreachable.git");
    git(
        &follower,
        &[
            "config",
            "remote.origin.pushurl",
            unreachable.to_str().unwrap(),
        ],
    );
    let (_, leaf, head, carry) = committed_then_failed(&pair, &follower, &base);
    assert_eq!(carry["carry"], "failed", "{carry}");
    assert_eq!(pair.owner_status(&task), "backlog");

    let (_, resumed) = owner_local_run(&pair, &task);
    assert_eq!(resumed["outcome"], "fresh", "{resumed}");
    assert_eq!(resumed["implement"], true, "{resumed}");
    assert_eq!(resumed["reason_code"], "not_durable", "{resumed}");
    assert_eq!(resumed["source_machine_id"], FOLLOWER, "{resumed}");
    assert_eq!(resumed["source_sha"], head.as_str(), "{resumed}");
    let note = resume_note(&pair, &task);
    assert!(note.starts_with("fresh: "), "{note}");
    assert!(note.contains("claim=claim-"), "{note}");
    assert!(note.contains(&format!("machine={FOLLOWER}")), "{note}");
    assert!(note.contains(&format!("source_run={leaf}")), "{note}");
    assert!(note.contains("reason_code=not_durable"), "{note}");
}
