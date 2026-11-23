use orbit_types::workflow::{
    PrAutoFixAttempt, PrAutoFixBlocker, PrAutoFixClaim, PrAutoFixDecision, decide_pr_auto_fix,
    decide_pr_auto_fix_settled,
};

fn attempt(number: u32, head: &str, run: &str) -> PrAutoFixAttempt {
    PrAutoFixAttempt {
        pr_number: "7".into(),
        head_sha: head.into(),
        blocker: PrAutoFixBlocker::FailedChecks,
        attempt: number,
        budget: 3,
        run_id: run.into(),
        crew: "fixer".into(),
        crew_source: "pool".into(),
    }
}

fn claim<'a>(head: &'a str, run: &'a str) -> PrAutoFixClaim<'a> {
    PrAutoFixClaim {
        pr_number: "7",
        head_sha: head,
        blocker: PrAutoFixBlocker::FailedChecks,
        run_id: run,
    }
}

#[test]
fn a_run_that_may_still_be_live_keeps_its_head_a_duplicate() {
    let attempts = [attempt(1, "h1", "run-a")];
    let decided = decide_pr_auto_fix(&attempts, &claim("h1", "run-b"), 3, "c", "s");
    assert!(
        matches!(decided, PrAutoFixDecision::Duplicate(_)),
        "{decided:?}"
    );
    let decided =
        decide_pr_auto_fix_settled(&attempts, &claim("h1", "run-b"), 3, "c", "s", &|_| false);
    assert!(
        matches!(decided, PrAutoFixDecision::Duplicate(_)),
        "{decided:?}"
    );
}

#[test]
fn an_ended_run_that_published_nothing_leaves_the_unspent_attempts_usable() {
    let attempts = [attempt(1, "h1", "run-a")];
    let decided =
        decide_pr_auto_fix_settled(&attempts, &claim("h1", "run-b"), 3, "c", "s", &|_| true);
    match decided {
        PrAutoFixDecision::Admit(next) => assert_eq!(next.attempt, 2),
        other => panic!("{other:?}"),
    }
    // The same run's poll of the head is still a duplicate.
    let decided =
        decide_pr_auto_fix_settled(&attempts, &claim("h1", "run-a"), 3, "c", "s", &|_| true);
    assert!(
        matches!(decided, PrAutoFixDecision::Duplicate(_)),
        "{decided:?}"
    );
}

#[test]
fn a_spent_budget_is_exhausted_even_on_an_unchanged_head() {
    let attempts = [
        attempt(1, "h1", "run-a"),
        attempt(2, "h1", "run-b"),
        attempt(3, "h1", "run-c"),
    ];
    let decided =
        decide_pr_auto_fix_settled(&attempts, &claim("h1", "run-d"), 3, "c", "s", &|_| true);
    assert!(
        matches!(decided, PrAutoFixDecision::Exhausted(_)),
        "{decided:?}"
    );
}
