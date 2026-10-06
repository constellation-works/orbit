//! History replay at the admitted traversal limit, and canonical deadlines.
//!
//! ORB-14356: signing every commit with its own git process cannot finish a
//! range the traversal check admits, and swallowing a deadline on the
//! canonical pass turns that failure into an ambiguous mapping.

use std::path::Path;
use std::process::Command;

use orbit_automation::AutomationError;
use orbit_automation::delivery::digest;
use orbit_types::workflow::automation::AutomationState;
use orbit_types::workflow::automation::recovery::{HistoryReplayRecord, refusal};

use super::super::source::{
    Source, arm_canonical_signature_deadline, clear_canonical_signature_deadline,
};

struct ClearDeadlineFault;

impl Drop for ClearDeadlineFault {
    fn drop(&mut self) {
        clear_canonical_signature_deadline();
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
) -> Result<HistoryReplayRecord, AutomationError> {
    source
        .replay_history_with_lookup("main", state, &|_, _| unreachable!("no provider lookup"), 0)
        .map(|(_page, record)| record)
}

#[test]
fn admitted_replay_limit_completes_and_one_past_it_is_refused() {
    clear_canonical_signature_deadline();
    let history = diverged(1000, 1000);
    let (source, state) = state_for(history.root.path(), history.old.last().expect("old tip"));
    let record = replay(&source, &state).expect(
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
    let last = &record.mappings[999];
    assert_eq!(
        first.proof_digest,
        direct_signature(history.root.path(), &history.old[0]),
        "batched signature matches the per-commit git signature"
    );
    assert_eq!(
        last.proof_digest,
        direct_signature(history.root.path(), &history.old[999]),
        "batched signature matches the per-commit git signature"
    );

    let over = diverged(1001, 1);
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
