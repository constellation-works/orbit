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

/// Tree of the base with `.orbit/n` set to `n` plus each of `files`.
fn tree_with(repo: &Path, base_tree: &str, n: usize, files: &[&str]) -> String {
    git(repo, &["read-tree", base_tree]);
    std::fs::write(repo.join(".orbit/n"), format!("{n}\n")).expect("orbit file");
    git(repo, &["add", ".orbit"]);
    for file in files {
        std::fs::write(repo.join(file), "side\n").expect("side file");
        git(repo, &["add", file]);
    }
    git(repo, &["write-tree"])
}

/// Two first-parent chains from one base, each one `.orbit` commit followed by
/// one `--no-ff` back-merge per name in `orphan_sides` / `canonical_sides`. A
/// merge brings in its named file from a side branch, so its first-parent patch
/// is that file and its `.orbit` tree equals every other commit's in the chain.
fn diverged_with_back_merges(orphan_sides: &[&str], canonical_sides: &[&str]) -> Diverged {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = root.path();
    git(repo, &["init"]);
    std::fs::create_dir(repo.join(".orbit")).expect("orbit dir");
    std::fs::write(repo.join(".orbit/n"), "0\n").expect("base file");
    git(repo, &["add", ".orbit"]);
    let base_tree = git(repo, &["write-tree"]);
    let base = git(repo, &["commit-tree", &base_tree, "-m", "base"]);

    let chain = |sides: &[&str], message: &str| {
        let tree = tree_with(repo, &base_tree, 1, &[]);
        let mut parent = git(
            repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &base,
                "-m",
                &format!("{message} orbit"),
            ],
        );
        let mut commits = vec![parent.clone()];
        let mut merged = Vec::new();
        for side in sides {
            let side_tree = tree_with(repo, &base_tree, 0, &[side]);
            let side_commit = git(
                repo,
                &[
                    "commit-tree",
                    &side_tree,
                    "-p",
                    &base,
                    "-m",
                    &format!("side {side}"),
                ],
            );
            merged.push(*side);
            let merge_tree = tree_with(repo, &base_tree, 1, &merged);
            parent = git(
                repo,
                &[
                    "commit-tree",
                    &merge_tree,
                    "-p",
                    &parent,
                    "-p",
                    &side_commit,
                    "-m",
                    &format!("{message} back-merge {side}"),
                ],
            );
            commits.push(parent.clone());
        }
        commits
    };

    let old = chain(orphan_sides, "old");
    let new = chain(canonical_sides, "new");
    git(
        repo,
        &[
            "update-ref",
            "refs/heads/main",
            new.last().expect("canonical tip"),
        ],
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
        lookup_retries: Default::default(),
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
        state.lookup_retries = history
            .old
            .iter()
            .map(|commit| {
                (
                    commit.clone(),
                    orbit_types::workflow::automation::AssociationLookupRetry {
                        last_checked_at: chrono::DateTime::UNIX_EPOCH,
                        attempts: 3,
                    },
                )
            })
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
            lookup_retries: Default::default(),
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
        assert!(
            checkpoint.lookup_retries.is_empty(),
            "replay retires orphan lookup retry keys"
        );
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
fn back_merges_with_equal_orbit_trees_map_by_their_first_parent_patch() {
    let history = diverged_with_back_merges(&["a.txt", "b.txt"], &["a.txt", "b.txt"]);
    let repo = history.root.path();
    let (source, state) = state_for(repo, history.old.last().expect("old tip"));

    let (_page, record) = replay(&source, &state)
        .expect("back-merges with distinct first-parent patches are not ambiguous");

    assert_eq!(record.mappings.len(), history.old.len());
    for (index, mapping) in record.mappings.iter().enumerate() {
        assert_eq!(mapping.orphan.commit, history.old[index], "orphan {index}");
        assert_eq!(
            mapping.canonical.commit, history.new[index],
            "each orphan maps to its own canonical commit, merges included"
        );
        assert_eq!(
            mapping.proof_digest,
            direct_signature(repo, &history.old[index]),
            "batched signature of commit {index} matches the first-parent `<commit>^1 <commit>` signature"
        );
    }
    assert_ne!(
        record.mappings[1].proof_digest, record.mappings[2].proof_digest,
        "two merges with one `.orbit` tree still carry their own patch"
    );
}

#[test]
fn back_merge_with_a_different_first_parent_patch_is_not_mapped() {
    let history = diverged_with_back_merges(&["a.txt"], &["b.txt"]);
    let (source, state) = state_for(history.root.path(), history.old.last().expect("old tip"));

    match replay(&source, &state) {
        Err(AutomationError::Refused(reason)) => assert_eq!(
            reason,
            refusal::HISTORY_MAPPING_AMBIGUOUS,
            "an orphan merge whose patch no canonical merge shares has no mapping"
        ),
        other => panic!("expected history_mapping_ambiguous, got {other:?}"),
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

/// Deterministic clock and provider faults exercise the retry schedule through
/// the real evaluator and checkpoint store, including observations that do
/// not advance the source cursor.
#[test]
fn recorded_associations_survive_observation_and_missing_identities_back_off() {
    use orbit_automation::delivery::{self, ActionOutcome, DeliveryHost, Evaluation};
    use orbit_types::workflow::automation::{BatchAttempt, DeliveryAssociation, SourceRevision};

    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &recorded_associations_survive_observation_and_missing_identities_back_off,
    )) {
        return;
    }
    struct LookupHost<'a> {
        source: Source<'a>,
        calls: &'a Cell<usize>,
        missing: &'a str,
    }
    impl DeliveryHost for LookupHost<'_> {
        fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
            self.source.head(branch)
        }
        fn observe(
            &self,
            branch: &str,
            state: &AutomationState,
        ) -> Result<SourcePage, AutomationError> {
            self.source.observe_with_lookup(branch, state, &|_, sha| {
                assert_eq!(
                    sha, self.missing,
                    "a recorded association is never queried again"
                );
                self.calls.set(self.calls.get() + 1);
                if self.calls.get() < 4 {
                    Err(AutomationError::Deferred("provider offline".into()))
                } else {
                    Ok("[]".into())
                }
            })
        }
        fn admit(&self, _: &BatchAttempt) -> Result<String, AutomationError> {
            unreachable!("this fixture never meets its delivery threshold")
        }
        fn outcome(&self, _: &BatchAttempt) -> Result<ActionOutcome, AutomationError> {
            unreachable!("no admitted action")
        }
    }

    let history = diverged_with_inserted_canonical_commit();
    let (source, mut state) = state_for(history.root.path(), history.old.last().unwrap());
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let trigger = DeliveryTrigger {
        owner_machine: None,
        branch: "main".into(),
        threshold: 50,
        max_wait_minutes: 60,
        coverage: CoverageClass::LandedCodeReviewV1,
        max_items: 50,
        retries: 0,
    };
    state.trigger = Some(trigger.clone());
    let baseline = AutomationState {
        observed: state.baseline.clone(),
        generation: 0,
        ..state.clone()
    };
    assert!(store.automation_initialize(&baseline).unwrap());
    state.observed = source.revision(history.new.last().unwrap()).unwrap();
    state.pending_commits = history.new.clone();
    state.unresolved = history
        .new
        .iter()
        .map(|sha| (sha.clone(), "landing_span_pending".into()))
        .collect();
    state.associations.insert(
        history.new[0].clone(),
        Some(DeliveryAssociation {
            key: "recorded-pr".into(),
            anchor: "anchor-not-yet-observed".into(),
            reference: "https://github.com/example/repo/pull/1".into(),
            landed_at: chrono::DateTime::UNIX_EPOCH,
        }),
    );
    state.associations.insert(history.new[1].clone(), None);
    assert!(store.automation_commit(&baseline, &state, None).unwrap());
    let known = state.associations.clone();
    let calls = Cell::new(0);
    // The first three observations make two attempts, and later observations
    // exercise the five- and thirty-minute boundaries without sleeping.
    for (seconds, expected) in [
        (0, 1),
        (30, 1),
        (60, 2),
        (359, 2),
        (360, 3),
        (2159, 3),
        (2160, 4),
        (4000, 4),
    ] {
        let now = chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(seconds);
        let host = LookupHost {
            source: Source::at(history.root.path(), now),
            calls: &calls,
            missing: &history.new[2],
        };
        let diagnostic = delivery::evaluate(
            store.as_ref(),
            &host,
            Evaluation {
                consumer: &state.consumer,
                epoch: &state.epoch,
                trigger: &trigger,
                enabled: true,
                dry_run: false,
                now,
            },
        )
        .unwrap();
        assert_eq!(
            calls.get(),
            expected,
            "provider attempts at {seconds}s respect persisted backoff"
        );
        let persisted = store.automation_state(&state.consumer).unwrap().unwrap();
        assert_eq!(diagnostic.state.as_ref(), Some(&persisted));
        for (sha, association) in &known {
            assert_eq!(persisted.associations.get(sha), Some(association));
        }
        if expected < 4 {
            assert_eq!(
                persisted.lookup_retries[&history.new[2]].attempts,
                expected as u32
            );
        } else {
            assert!(
                persisted.lookup_retries.is_empty(),
                "a successful identity lookup retires its retry state"
            );
        }
    }
}

// Kernel process cleanup and lock contention fault injection.

use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
#[test]
fn timed_out_source_reaps_its_leader_and_kills_the_grandchild() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &timed_out_source_reaps_its_leader_and_kills_the_grandchild,
    )) {
        return;
    }
    // SAFETY: this isolated child adopts orphaned grandchildren so the test
    // can reap them and prove they are gone, even with a non-reaping PID 1.
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
    let root = tempfile::tempdir().unwrap();
    let source = Source::new(root.path());
    let started = Instant::now();
    let result = source.git(&[
        "-c",
        "alias.timeout-fixture=!sleep 60 & echo \"$PPID $$ $!\" > pids; wait",
        "timeout-fixture",
    ]);
    assert!(
        matches!(result, Err(orbit_automation::AutomationError::Deferred(reason)) if reason == "source_budget")
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let pids = std::fs::read_to_string(root.path().join("pids")).unwrap();
    let pids: Vec<i32> = pids
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(
        pids.len(),
        3,
        "fake command recorded git, shell and grandchild"
    );
    let cleanup_end = Instant::now() + Duration::from_secs(2);
    loop {
        // SAFETY: waitpid probes only the adopted test grandchild, without
        // blocking. kill(pid, 0) checks existence and sends no signal.
        for pid in &pids[1..] {
            unsafe {
                libc::waitpid(*pid, std::ptr::null_mut(), libc::WNOHANG);
            }
        }
        if pids.iter().all(|pid| unsafe { libc::kill(*pid, 0) } == -1) {
            break;
        }
        assert!(
            Instant::now() < cleanup_end,
            "source timeout left a leader or helper alive: {pids:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn held_fetch_lock_consumes_only_the_remaining_source_deadline() {
    use orbit_common::fs::file_lock::{FileLockOptions, acquire_exclusive_file_lock};

    let history = diverged(1, 1);
    let repo = history.root.path();
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/held.git",
        ],
    );
    let lock = acquire_exclusive_file_lock(
        &repo.join(".git/.orbit-git-fetch.lock"),
        "held fetch fixture",
        FileLockOptions::default(),
    )
    .unwrap();
    let source = Source::new(repo);
    let started = Instant::now();
    let result = source.head("main");
    assert!(result.is_err(), "a held lock must defer the fetch");
    assert!(
        started.elapsed() < Duration::from_secs(32),
        "lock wait renewed the full timeout"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(29),
        "the fixture must exercise real lock contention rather than an earlier error"
    );
    drop(lock);
}
