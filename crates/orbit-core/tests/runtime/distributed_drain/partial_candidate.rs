//! An unfinished claimed implementation survives on origin without changing
//! the leaf checkout, and another host resumes that partial work.

use super::candidate_carry::{
    bare, claim_candidate, engine_action, has_commit, host_checkout, owner_published,
    settled_candidate,
};
use super::*;

const BRANCH: &str = "orbit/partial-implementation";
const CONTENT: &str = "fn work() { unfinished(); }\n";

/// Fail the implementer after it has written files, run the terminal failure
/// hook over its pre-commit checkpoint, then settle its claim with the owner.
fn fail_implementation(pair: &Pair, repo: &Path, base: &str) -> (String, Value) {
    let task = &pair.tasks[0];
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let steps = json!({
        "worktree": {"workspace_path": repo, "head_ref": BRANCH, "base_sha": base},
    });
    let error = "[transient_failure] implementer stopped after writing partial files";
    let carry = engine_action(
        &pair.follower,
        "claim_candidate_carry",
        &json!({
            "failed_step_id": "implement_bundle",
            "activity_name": "implement_one",
            "error_code": "execution_failed",
            "error_message": error,
            "job_input": {"task_ids": [task]},
            "pipeline": steps,
            "run_id": leaf,
        }),
    );
    let mut state = pair.follower.read_run_state(&leaf).unwrap().unwrap();
    state.pipeline["worktree"] = steps["worktree"].clone();
    state.record_failure_activity(
        "claim_candidate_carry".into(),
        "implement_bundle".into(),
        carry.clone(),
    );
    pair.follower.write_run_state(&leaf, &state).unwrap();
    pair.leaf_fails_with(&leaf, error);
    pair.pass_over(&drain, json!({"window_expired": true}));
    (leaf, carry)
}

fn index_bytes(repo: &Path) -> Vec<u8> {
    let index = git(repo, &["rev-parse", "--git-path", "index"]);
    let path = PathBuf::from(index.trim());
    std::fs::read(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
    .unwrap()
}

#[test]
fn an_uncommitted_claimed_candidate_is_carried_and_resumed_on_another_host() {
    if !isolated(
        module_path!(),
        "an_uncommitted_claimed_candidate_is_carried_and_resumed_on_another_host",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let base = owner_published(&pair);
    let repo = host_checkout(&pair, &pair);
    git(&repo, &["checkout", "-q", "-b", BRANCH]);
    std::fs::write(repo.join("src/f0.rs"), "staged intermediate\n").unwrap();
    git(&repo, &["add", "src/f0.rs"]);
    std::fs::write(repo.join("src/f0.rs"), CONTENT).unwrap();
    std::fs::write(repo.join("src/new.rs"), "new partial file\n").unwrap();
    std::fs::rename(repo.join(".gitignore"), repo.join("ignore-rules")).unwrap();
    // Excluded even when scratch is not ignored by the fixture's renamed rules.
    std::fs::create_dir_all(repo.join(".orbit/tmp")).unwrap();
    std::fs::write(repo.join(".orbit/tmp/implementer.log"), "scratch\n").unwrap();
    // Ignore runtime metadata while leaving scratch visible to path discovery.
    git(&repo, &["config", "core.excludesFile", "ignore-rules"]);
    // Keep runtime metadata ignored while making scratch visible to discovery.
    std::fs::write(repo.join("ignore-rules"), "/.orbit/*\n!/.orbit/tmp/\n").unwrap();
    let before_status = git(&repo, &["status", "--porcelain", "--untracked-files=all"]);
    let before_index = index_bytes(&repo);
    let (leaf, carry) = fail_implementation(&pair, &repo, &base);
    let reference = format!("refs/orbit/candidates/{task}/{leaf}");
    assert_eq!(carry["carry"], "durable", "{carry}");
    assert_eq!(carry["partial"], true, "{carry}");
    assert_eq!(carry["durable_ref"], reference, "{carry}");
    let head = carry["head_sha"].as_str().unwrap();
    assert_eq!(git(&bare(&pair), &["rev-parse", &reference]).trim(), head);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), base);
    assert_eq!(
        git(&repo, &["symbolic-ref", "--short", "HEAD"]).trim(),
        BRANCH
    );
    assert_eq!(index_bytes(&repo), before_index);
    assert_eq!(
        git(&repo, &["status", "--porcelain", "--untracked-files=all"]),
        before_status
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("src/f0.rs")).unwrap(),
        CONTENT
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", &format!("{base}..{head}")]).trim(),
        "1"
    );
    assert_eq!(git(&repo, &["show", &format!("{head}:src/f0.rs")]), CONTENT);
    assert!(
        !git(&repo, &["ls-tree", "-r", "--name-only", head])
            .lines()
            .any(|path| path.starts_with(".orbit/"))
    );
    let message = git(&repo, &["log", "-1", "--format=%B", head]);
    assert!(
        message
            .lines()
            .any(|line| line == format!("Orbit-Partial-Candidate-Run: {leaf}"))
    );
    let kept = settled_candidate(&pair, &leaf, "Release");
    assert_eq!(kept["durable_ref"], reference, "{kept}");
    assert_eq!(kept["head_sha"], head, "{kept}");
    assert_eq!(kept["failed_step_id"], "implement_bundle", "{kept}");
    assert!(
        pair.wire
            .owner
            .get_task_history(&task)
            .unwrap()
            .iter()
            .any(|entry| {
                entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains(&reference))
            })
    );

    let second = pair.another_host("hm_partial_second");
    let checkout = host_checkout(&pair, &second);
    assert!(!has_commit(&checkout, head));
    let drain = second.run_drain();
    let next = second.running_leaf(&drain, 1);
    let offered = claim_candidate(&second, &next).expect("next claim keeps partial work");
    assert_eq!(offered.durable_ref.as_deref(), Some(reference.as_str()));
    let input = second
        .follower_jobs
        .get_job_run(&next)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let resumed = engine_action(
        &second.follower,
        "candidate_resume",
        &json!({
            "job_run_id": next, "task_ids": [task], "workspace_path": checkout,
            "base_sha": base, "candidate": input["resume_candidate"], "claimed": true,
        }),
    );
    assert_eq!(resumed["repair"]["trigger"], "implementation", "{resumed}");
    assert_eq!(resumed["source_sha"], head, "{resumed}");
    assert_eq!(
        std::fs::read_to_string(checkout.join("src/f0.rs")).unwrap(),
        CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("src/new.rs")).unwrap(),
        "new partial file\n"
    );
    assert!(!checkout.join(".gitignore").exists());
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]).trim(), base);
}

#[test]
fn a_partial_claim_refuses_protected_files_and_gitlinks_before_carry() {
    if !isolated(
        module_path!(),
        "a_partial_claim_refuses_protected_files_and_gitlinks_before_carry",
    ) {
        return;
    }
    for protected in [".ENV.local", ".Orbit/state.json", "nested", "staged-nested"] {
        let pair = Pair::new(1);
        let base = owner_published(&pair);
        let repo = host_checkout(&pair, &pair);
        git(&repo, &["checkout", "-q", "-b", BRANCH]);
        std::fs::write(repo.join("src/f0.rs"), CONTENT).unwrap();
        if protected.ends_with("nested") {
            let nested = repo.join(protected);
            std::fs::create_dir_all(&nested).unwrap();
            git(&nested, &["init", "-q"]);
            std::fs::write(nested.join("file"), "nested file\n").unwrap();
            git(&nested, &["add", "file"]);
            git(&nested, &["commit", "-q", "-m", "nested"]);
            if protected == "staged-nested" {
                git(&repo, &["add", "--", protected]);
            }
        } else {
            let path = repo.join(protected);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "private content\n").unwrap();
            // Already-staged additions must be refused too.
            git(&repo, &["add", "-f", "--", protected]);
        }
        let before = git(&repo, &["status", "--porcelain", "--untracked-files=all"]);
        let index = index_bytes(&repo);
        let (leaf, carry) = fail_implementation(&pair, &repo, &base);
        assert_eq!(carry["carry"], "failed", "{carry}");
        assert!(
            carry["reason"].as_str().unwrap().contains(protected),
            "{carry}"
        );
        assert!(
            carry.get("head_sha").is_none(),
            "no unsafe candidate: {carry}"
        );
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), base);
        assert_eq!(index_bytes(&repo), index);
        assert_eq!(
            git(&repo, &["status", "--porcelain", "--untracked-files=all"]),
            before
        );
        let reference = format!("refs/orbit/candidates/{}/{leaf}", pair.tasks[0]);
        assert!(
            git(&repo, &["ls-remote", "origin", &reference])
                .trim()
                .is_empty()
        );
        let settle = pair
            .wire
            .calls("orbit.drain.claim.settle")
            .into_iter()
            .last()
            .unwrap();
        assert!(
            settle["settlement"]["Release"]["failure"]["reason"]
                .as_str()
                .unwrap()
                .contains(protected),
            "{settle}"
        );
        assert!(settled_candidate(&pair, &leaf, "Release").is_null());
    }
}
