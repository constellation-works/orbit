//! [ORB-14822] A step after `commit` whose recovery repairs the worktree is
//! retried on a commit the host makes; the recovery agent never commits.
//!
//! The substitute provider only writes files and its decision; it runs no
//! Git command. A repair the host must not commit is refused by name before
//! the index changes, and the step fails without its retry.

use super::*;

/// The candidate the implementation committed, which `validate` judges.
fn commit_candidate(worktree: &Path) {
    std::fs::write(worktree.join("candidate.txt"), "broken\n").unwrap();
    git(worktree, &["add", "candidate.txt"]);
    git(worktree, &["commit", "-q", "-m", "implementation"]);
}

/// The output every `git_commit` outcome records.
fn committed() -> (&'static str, Value) {
    (
        "commit",
        json!({
            "phase": "commit",
            "decision": "performed",
            "committed": true,
            "skipped_no_diff_expected": false,
            "task_id": TASK,
        }),
    )
}

const REPAIR: &str =
    r#"printf 'fixed\n' > "$root/candidate.txt"; bound retry "fixed the candidate" > "$path""#;

fn failure_text(observed: &Observed) -> String {
    match &observed.result {
        Ok((_, message)) => message.clone().unwrap_or_default(),
        Err(message) => message.clone(),
    }
}

#[test]
fn a_validate_repair_is_committed_by_the_host_and_retried_on_that_head() {
    if !isolated(
        "step_recovery::repair_commit::a_validate_repair_is_committed_by_the_host_and_retried_on_that_head",
    ) {
        return;
    }
    let fixture = fixture();
    // A claimed leaf's `validate` has step recovery too, and the same commit.
    for (name, claimed) in [("owner_repair", false), ("claimed_repair", true)] {
        let observed = run(
            &fixture,
            &Case {
                name,
                producer: REPAIR,
                claimed,
                prepare: commit_candidate,
                before: vec![committed()],
                ..Case::default()
            },
        );
        assert_eq!(observed.result, Ok((true, None)), "{name}");
        assert_eq!(observed.post_recovery, ["success"], "{name}");
        let [(implementation, failed_status), (retried, retried_status)] =
            observed.attempts_saw.as_slice()
        else {
            panic!("{name}: two validate attempts: {:?}", observed.attempts_saw);
        };
        assert_eq!(failed_status, "", "{name}: the failure saw a clean tree");
        assert_ne!(
            retried, implementation,
            "{name}: the retry ran on a new head"
        );
        assert_eq!(
            retried_status, "",
            "{name}: the retry saw the repair committed, not left dirty"
        );
        let worktree = &observed.worktree;
        assert_eq!(
            git_stdout(worktree, &["rev-parse", &format!("{retried}^")]),
            *implementation,
            "{name}: the repair is one commit on top of the candidate"
        );
        assert_eq!(
            git_stdout(worktree, &["show", "--format=", "--name-only", retried]),
            "candidate.txt",
            "{name}: exactly the repaired path"
        );
        assert_eq!(
            git_stdout(worktree, &["show", &format!("{retried}:candidate.txt")]),
            "fixed"
        );
        // Orbit's process identity commits; the author and trailers record
        // the recovery that made the change.
        assert_eq!(
            git_stdout(
                worktree,
                &["show", "-s", "--format=%an <%ae>|%cn <%ce>", retried]
            ),
            "orbit-recovery <orbit-recovery@orbit.local>|orbit <orbit@orbit.local>",
            "{name}"
        );
        let message = git_stdout(worktree, &["show", "-s", "--format=%B", retried]);
        for trailer in [
            format!("Orbit-Recovery-Run: run-{name}"),
            "Orbit-Recovery-Step: validate".to_string(),
            "Orbit-Recovery-Activity: step_failure_recovery".to_string(),
        ] {
            assert!(message.contains(&trailer), "{name}: {trailer}: {message}");
        }
        assert!(message.contains(&format!("[{TASK}]")), "{name}: {message}");
    }

    // A review admitted on an earlier candidate (before a final-recovery
    // resume moved it) does not pin the current head.
    let stale_review = run(
        &fixture,
        &Case {
            name: "stale_review",
            producer: REPAIR,
            prepare: commit_candidate,
            before: vec![
                committed(),
                (
                    "review_gate_admit",
                    json!({"applies": true, "attempt_id": "attempt-0", "head_sha": "0".repeat(40)}),
                ),
            ],
            ..Case::default()
        },
    );
    assert_eq!(stale_review.result, Ok((true, None)));
    assert_ne!(
        stale_review.attempts_saw[0].0,
        stale_review.attempts_saw[1].0
    );

    // A recovery that changes nothing in the worktree leaves the candidate
    // as it was: no commit, and the retry runs on the same head.
    let clean = run(
        &fixture,
        &Case {
            name: "clean_repair",
            producer: r#"bound retry "the runner recovered" > "$path""#,
            prepare: commit_candidate,
            before: vec![committed()],
            ..Case::default()
        },
    );
    assert_eq!(clean.result, Ok((true, None)));
    let heads = clean
        .attempts_saw
        .iter()
        .map(|(head, _)| head)
        .collect::<Vec<_>>();
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0], heads[1], "nothing to commit");

    // Before the candidate is committed, the `commit` step owns the repair.
    let uncommitted = run(
        &fixture,
        &Case {
            name: "before_commit",
            producer: REPAIR,
            prepare: commit_candidate,
            ..Case::default()
        },
    );
    assert_eq!(uncommitted.result, Ok((true, None)));
    assert_eq!(
        uncommitted.attempts_saw[0].0, uncommitted.attempts_saw[1].0,
        "no host commit before the commit step ran"
    );
    assert_eq!(uncommitted.attempts_saw[1].1, "M candidate.txt");
}

#[test]
fn a_repair_the_host_must_not_commit_is_refused_by_name_and_nothing_is_committed() {
    if !isolated(
        "step_recovery::repair_commit::a_repair_the_host_must_not_commit_is_refused_by_name_and_nothing_is_committed",
    ) {
        return;
    }
    let fixture = fixture();
    let admitted_review = (
        "review_gate_admit",
        json!({"applies": true, "attempt_id": "attempt-1", "head_sha": "HEAD"}),
    );
    let settled_review = ("review_gate_settle", json!({"reviewed_head_sha": "HEAD"}));
    let mut no_diff = committed();
    no_diff.1["skipped_no_diff_expected"] = json!(true);
    let cases = [
        (
            "protected_path",
            r#"printf 'TOKEN=x\n' > "$root/.env""#,
            vec![committed()],
            "?? .env",
        ),
        (
            "outside_footprint",
            r#"ln -s /etc/hosts "$root/hosts""#,
            vec![committed()],
            "?? hosts",
        ),
        ("no_diff_route", "", vec![no_diff], ""),
        (
            "reviewed_candidate",
            "",
            vec![committed(), admitted_review],
            "",
        ),
    ];
    let cases = cases.into_iter().chain([(
        // A claimed leaf validates the reviewed head.
        "reviewed_candidate",
        "",
        vec![committed(), settled_review],
        "",
    )]);
    for (index, (reason, extra, before, leftover)) in cases.enumerate() {
        let name = format!("{reason}_{index}");
        // Each refused repair also fixes the candidate, which must stay
        // uncommitted with the rest of it.
        let producer = format!("{extra}\n{REPAIR}");
        let observed = run(
            &fixture,
            &Case {
                name: &name,
                producer: &producer,
                prepare: commit_candidate,
                before,
                ..Case::default()
            },
        );
        assert!(observed.decision().retry_admitted, "{reason}");
        assert_eq!(observed.deliveries, 1, "{reason}: no post-recovery attempt");
        assert!(observed.post_recovery.is_empty(), "{reason}");
        let failure = failure_text(&observed);
        assert!(
            failure.contains(&format!("recovery_commit_refused:{reason}:"))
                && failure.contains(ORIGINAL_FAILURE),
            "{reason}: the step fails with the typed refusal and keeps the original: {failure}"
        );
        let worktree = &observed.worktree;
        assert_eq!(
            git_stdout(worktree, &["rev-parse", "HEAD"]),
            observed.attempts_saw[0].0,
            "{reason}: nothing was committed"
        );
        assert_eq!(
            git_stdout(worktree, &["diff", "--cached", "--name-only"]),
            "",
            "{reason}: the index is untouched"
        );
        let status = git_stdout(worktree, &["status", "--porcelain"]);
        assert!(
            status.contains("M candidate.txt") && status.contains(leftover),
            "{reason}: the repair is left as recovery wrote it: {status}"
        );
    }
}
