//! History replay at the admitted traversal limit, and canonical deadlines.
//!
//! ORB-14356: signing every commit with its own git process cannot finish a
//! range the traversal check admits, and swallowing a deadline on the
//! canonical pass turns that failure into an ambiguous mapping.

use std::cell::Cell;
use std::path::Path;
use std::process::Command;

use orbit_automation::AutomationError;
use orbit_automation::delivery::digest;
use orbit_automation::delivery::recovery::{
    self, HISTORY_REPLAY_COMMIT_LIMIT, HistoryReplayInput, Recovery,
};
use orbit_store::{Store, compose};
use orbit_types::workflow::automation::recovery::{
    HistoryReplayRecord, RecoveryPreview, RecoveryRequest, refusal,
};
use orbit_types::workflow::automation::{
    AutomationState, CoverageClass, DeliveryTrigger, SourcePage,
};

use super::super::source::{
    Source, arm_canonical_signature_deadline, arm_expire_source_after_next_head,
    clear_canonical_signature_deadline, clear_expired_source,
};

struct ClearDeadlineFault;

impl Drop for ClearDeadlineFault {
    fn drop(&mut self) {
        clear_canonical_signature_deadline();
        clear_expired_source();
    }
}

struct Diverged {
    root: tempfile::TempDir,
    old: Vec<String>,
    new: Vec<String>,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "replay")
        .env("GIT_AUTHOR_EMAIL", "replay@example.com")
        .env("GIT_COMMITTER_NAME", "replay")
        .env("GIT_COMMITTER_EMAIL", "replay@example.com")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git speaks utf-8")
        .trim()
        .to_string()
}

/// Two first-parent chains from one base. Step `i` on each chain uses the
/// same tree, so the parent-relative `.orbit` patch matches and the commits
/// themselves differ.
fn diverged(orphans: usize, canonical: usize) -> Diverged {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = root.path();
    git(repo, &["init"]);
    std::fs::create_dir(repo.join(".orbit")).expect("orbit dir");
    std::fs::write(repo.join(".orbit/n"), "0\n").expect("base file");
    git(repo, &["add", ".orbit"]);
    let base_tree = git(repo, &["write-tree"]);
    let base = git(repo, &["commit-tree", &base_tree, "-m", "base"]);

    let steps = orphans.max(canonical);
    let mut trees = Vec::with_capacity(steps);
    for step in 1..=steps {
        std::fs::write(repo.join(".orbit/n"), format!("{step}\n")).expect("step file");
        git(repo, &["add", ".orbit"]);
        trees.push(git(repo, &["write-tree"]));
    }

    let commit_chain = |count: usize, message: &str| {
        let mut parent = base.clone();
        let mut commits = Vec::with_capacity(count);
        for (step, tree) in trees.iter().take(count).enumerate() {
            let commit = git(
                repo,
                &[
                    "commit-tree",
                    tree,
                    "-p",
                    &parent,
                    "-m",
                    &format!("{message} {step}"),
                ],
            );
            parent = commit.clone();
            commits.push(commit);
        }
        commits
    };

    let old = commit_chain(orphans, "old");
    let new = commit_chain(canonical, "new");
    git(
        repo,
        &["update-ref", "refs/heads/main", new.last().unwrap_or(&base)],
    );
    git(repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);

    Diverged { root, old, new }
}

fn state_for<'a>(repo: &'a Path, old_tip: &str) -> (Source<'a>, AutomationState) {
    let source = Source::new(repo);
    let repository = source.repository().expect("repository");
    // The orphan tip's merge base with main is the shared parent. Covered and
    // baseline stay there, which is what a replay of this divergence requires.
    let base_commit = source
        .git(&["merge-base", old_tip, "refs/heads/main"])
        .expect("merge base");
    let base = source.revision(&base_commit).expect("base revision");
    let observed = source.revision(old_tip).expect("observed revision");
    let state = AutomationState {
        members: None,
        consumer: "consumer".into(),
        epoch: "epoch".into(),
        trigger: None,
        repository,
        branch: "main".into(),
        generation: 1,
        baseline: base.clone(),
        observed,
        covered: base,
        pending_commits: Vec::new(),
        pending: Vec::new(),
        waived: Vec::new(),
        excluded: Vec::new(),
        unresolved: Default::default(),
        associations: Default::default(),
        active: None,
        stall: None,
    };
    (source, state)
}

#[test]
fn missing_git_executable_defers_ancestor_check_instead_of_reporting_divergence() {
    let root = tempfile::tempdir().expect("tempdir");
    let empty_bin = root.path().join("empty-bin");
    std::fs::create_dir(&empty_bin).expect("empty PATH directory");
    let path = empty_bin.to_str().expect("PATH is utf-8");
    let _path = orbit_common::test_env::scoped([("PATH", Some(path))]);
    let source = Source::new(root.path());

    let error = source
        .is_ancestor("older", "newer")
        .expect_err("a missing Git executable is an operational failure");
    assert!(
        matches!(&error, AutomationError::Deferred(reason) if reason.starts_with("source_spawn_failed:")),
        "unexpected error: {error}"
    );
    assert!(
        !super::super::source::is_batch_mismatch(&error),
        "a spawn failure must not become an unverifiable-coverage mismatch"
    );
}

fn diverged_with_inserted_canonical_commit() -> Diverged {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = root.path();
    git(repo, &["init"]);
    std::fs::create_dir(repo.join(".orbit")).expect("orbit dir");
    std::fs::write(repo.join(".orbit/n"), "0\n").expect("base file");
    git(repo, &["add", ".orbit"]);
    let base_tree = git(repo, &["write-tree"]);
    let base = git(repo, &["commit-tree", &base_tree, "-m", "base"]);

    let empty = git(
        repo,
        &[
            "commit-tree",
            &base_tree,
            "-p",
            &base,
            "-m",
            "empty insertion",
        ],
    );

    std::fs::write(repo.join("outside.txt"), "inserted\n").expect("inserted file");
    git(repo, &["add", "outside.txt"]);
    let outside_tree = git(repo, &["write-tree"]);
    let old_outside = git(
        repo,
        &[
            "commit-tree",
            &outside_tree,
            "-p",
            &base,
            "-m",
            "old outside",
        ],
    );
    let canonical_outside = git(
        repo,
        &[
            "commit-tree",
            &outside_tree,
            "-p",
            &empty,
            "-m",
            "canonical outside",
        ],
    );

    std::fs::write(repo.join(".orbit/n"), "1\n").expect("replay file");
    git(repo, &["add", ".orbit"]);
    let replay_tree = git(repo, &["write-tree"]);
    let old_final = git(
        repo,
        &[
            "commit-tree",
            &replay_tree,
            "-p",
            &old_outside,
            "-m",
            "old final",
        ],
    );
    let canonical_final = git(
        repo,
        &[
            "commit-tree",
            &replay_tree,
            "-p",
            &canonical_outside,
            "-m",
            "canonical final",
        ],
    );
    git(repo, &["update-ref", "refs/heads/main", &canonical_final]);
    git(repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);

    let remote = root.path().join("remote.git");
    std::fs::create_dir(&remote).expect("remote dir");
    let output = Command::new("git")
        .args(["init", "--bare"])
        .current_dir(&remote)
        .output()
        .expect("init bare remote");
    assert!(output.status.success(), "git init --bare failed");
    let remote_text = remote.to_str().expect("remote path is utf-8");
    let provider_url = "https://github.com/example/repo.git";
    let rewrite = format!("url.{remote_text}.insteadOf");
    git(repo, &["config", &rewrite, provider_url]);
    git(repo, &["remote", "add", "origin", provider_url]);
    git(repo, &["push", "origin", "refs/heads/main"]);

    Diverged {
        root,
        old: vec![old_outside, old_final],
        new: vec![empty, canonical_outside, canonical_final],
    }
}

/// The pre-batch signature: one `rev-parse` of `.orbit` and one `diff-tree`.
fn direct_signature(repo: &Path, commit: &str) -> String {
    let orbit_tree = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}:.orbit"),
        ],
    );
    let patch = git(
        repo,
        &[
            "diff-tree",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-commit-id",
            "-r",
            &format!("{commit}^1"),
            commit,
        ],
    );
    digest(format!("{orbit_tree}\0{patch}").as_bytes())
}

fn replay(
    source: &Source<'_>,
    state: &AutomationState,
) -> Result<(SourcePage, HistoryReplayRecord), AutomationError> {
    source.replay_history_with_lookup(
        "main",
        state,
        &|_, _, _| unreachable!("no provider lookup"),
        0,
    )
}

#[test]
fn admitted_replay_limit_completes_and_one_past_it_is_refused() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &admitted_replay_limit_completes_and_one_past_it_is_refused,
    )) {
        return;
    }
    clear_canonical_signature_deadline();
    for count in [201, HISTORY_REPLAY_COMMIT_LIMIT] {
        let history = diverged(count, count);
        let (source, mut state) =
            state_for(history.root.path(), history.old.last().expect("old tip"));
        let trigger = DeliveryTrigger {
            owner_machine: Some("fixture-machine".into()),
            branch: "main".into(),
            threshold: 1,
            max_wait_minutes: 60,
            coverage: CoverageClass::LandedCodeReviewV1,
            max_items: 50,
            retries: 1,
        };
        state.trigger = Some(trigger.clone());
        state.pending_commits = history.old.clone();
        state.unresolved = history
            .old
            .iter()
            .map(|commit| (commit.clone(), "delivery_owner_evidence_pending".into()))
            .collect();
        let (page, record) = replay(&source, &state).expect(
            "ORB-14356: a 1000-commit replay finishes instead of exhausting the source deadline",
        );
        assert_eq!(
            record.mappings.len(),
            history.old.len(),
            "every admitted orphan maps"
        );
        for (index, mapping) in record.mappings.iter().enumerate() {
            assert_eq!(mapping.orphan.commit, history.old[index], "orphan {index}");
            assert_eq!(
                mapping.canonical.commit, history.new[index],
                "canonical {index}"
            );
        }
        let first = &record.mappings[0];
        let last = record.mappings.last().expect("last mapping");
        assert_eq!(
            first.proof_digest,
            direct_signature(history.root.path(), &history.old[0]),
            "batched signature matches the per-commit git signature"
        );
        assert_eq!(
            last.proof_digest,
            direct_signature(history.root.path(), history.old.last().expect("old tip")),
            "batched signature matches the per-commit git signature"
        );

        let store = compose::automation_store(Store::open_in_memory().expect("store"))
            .expect("automation store");
        let baseline = AutomationState {
            observed: state.baseline.clone(),
            generation: 0,
            pending_commits: vec![],
            unresolved: Default::default(),
            ..state.clone()
        };
        assert!(store.automation_initialize(&baseline).expect("initialize"));
        let ordinary = source
            .observe_with_lookup("main", &baseline, &|_, _| unreachable!("no provider"))
            .expect("ordinary observation");
        assert_eq!(ordinary.commits, history.new[..200]);
        assert_eq!(ordinary.through.commit, history.new[199]);
        assert!(!ordinary.complete, "ordinary observation remains paginated");
        assert!(
            store
                .automation_commit(&baseline, &state, None)
                .expect("observe orphans")
        );
        let request = RecoveryRequest {
            replay_history: true,
            ..RecoveryRequest::default()
        };
        let operation = Recovery {
            consumer: &state.consumer,
            epoch: &state.epoch,
            trigger: &trigger,
            repository: &state.repository,
            host_refusal: None,
            request: &request,
            by: "operator",
            now: chrono::DateTime::UNIX_EPOCH,
            replay: Some(HistoryReplayInput { page, record }),
            resolved_action_id: None,
            expected_generation: Some(state.generation),
            action_terminal: false,
            action_failed_without_evidence: false,
        };
        let preview = recovery::preview(store.as_ref(), &operation)
            .expect("a replay beyond the ordinary page limit can be previewed");
        assert!(preview.refusals.is_empty());
        assert_eq!(preview.debt.pending_commits, count);
        assert_eq!(preview.debt.unresolved, count);
        assert_eq!(
            store.automation_state(&state.consumer).expect("state"),
            Some(state.clone()),
            "preview must leave the orphaned checkpoint untouched"
        );
        let apply_request = RecoveryRequest {
            reason: "reconcile the content-preserving rebase".into(),
            ..request
        };
        let applied = recovery::apply(
            store.as_ref(),
            &Recovery {
                request: &apply_request,
                ..operation
            },
        )
        .expect("the returned replay page can be applied");
        assert_eq!(applied.applied, [RecoveryPreview::REPLAYED_HISTORY]);
        assert_eq!(applied.history_replay, preview.history_replay);
        let checkpoint = store
            .automation_state(&state.consumer)
            .expect("state")
            .expect("checkpoint");
        assert_eq!(
            checkpoint.observed.commit,
            *history.new.last().expect("canonical tip")
        );
        assert_eq!(checkpoint.pending_commits, history.new);
        assert_eq!(
            checkpoint.unresolved,
            checkpoint
                .pending_commits
                .iter()
                .map(|commit| { (commit.clone(), "delivery_owner_evidence_pending".into()) })
                .collect()
        );
        assert_eq!(checkpoint.baseline, state.baseline);
        assert_eq!(checkpoint.covered, state.covered);
        assert_eq!(checkpoint.generation, state.generation + 1);
        assert_eq!(applied.history[0].replayed_history, applied.history_replay);
    }

    for (orphans, canonical) in [
        (HISTORY_REPLAY_COMMIT_LIMIT + 1, 1),
        (1, HISTORY_REPLAY_COMMIT_LIMIT + 1),
    ] {
        let over = diverged(orphans, canonical);
        let (source, state) = state_for(over.root.path(), over.old.last().expect("old tip"));
        match replay(&source, &state) {
            Err(AutomationError::Refused(reason)) => assert_eq!(
                reason,
                refusal::HISTORY_TRAVERSAL_LIMIT,
                "ORB-14356: one commit past the admitted limit is the traversal refusal"
            ),
            other => panic!("expected history_traversal_limit, got {other:?}"),
        }
    }
}

#[test]
fn canonical_signature_deadline_propagates() {
    let _guard = ClearDeadlineFault;
    let history = diverged(1, 1);
    let (source, state) = state_for(history.root.path(), history.old.last().expect("old tip"));
    arm_canonical_signature_deadline();
    match replay(&source, &state) {
        Err(AutomationError::Deferred(reason)) => assert_eq!(
            reason, "source_deadline",
            "ORB-14356: a deadline while signing canonical commits propagates"
        ),
        other => panic!(
            "ORB-14356: expected source_deadline, not a skipped proof that fails later, got {other:?}"
        ),
    }
}

#[test]
fn replay_provider_lookups_use_a_fresh_source_after_the_initial_head() {
    let _guard = ClearDeadlineFault;
    clear_expired_source();
    let history = diverged_with_inserted_canonical_commit();
    assert_eq!(
        direct_signature(history.root.path(), &history.old[0]),
        direct_signature(history.root.path(), &history.new[1]),
        "the old and canonical outside-file commits have the same patch"
    );
    assert_eq!(
        direct_signature(history.root.path(), &history.old[1]),
        direct_signature(history.root.path(), &history.new[2]),
        "the old and canonical automation commits have the same patch"
    );
    let (source, state) = state_for(history.root.path(), history.old.last().expect("old tip"));
    let lookups = Cell::new(0);
    arm_expire_source_after_next_head();

    let (page, record) = source
        .replay_history_with_lookup(
            "main",
            &state,
            &|lookup_source, _repository, _sha| {
                lookups.set(lookups.get() + 1);
                let origin = lookup_source.git(&["config", "--get", "remote.origin.url"])?;
                assert_eq!(origin, "https://github.com/example/repo.git");
                Ok("[]".into())
            },
            0,
        )
        .expect("replay's provider observation has a fresh command budget");

    assert_eq!(
        record.mappings.len(),
        2,
        "both orphans map past the insertion"
    );
    assert_eq!(
        page.commits, history.new,
        "the insertion is observed before the mapping"
    );
    assert_eq!(
        lookups.get(),
        3,
        "all observed commits get provider lookups"
    );
    assert!(
        page.unresolved
            .values()
            .all(|reason| reason != "evidence_unavailable"),
        "a stale pre-fetch source must not silently turn provider lookups into missing evidence"
    );
}
