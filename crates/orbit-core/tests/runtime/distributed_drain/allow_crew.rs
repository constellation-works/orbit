//! `orbit run auto --pull --allow-crew` [ORB-14174]: a follower restricts the
//! crews it runs for one drain, so the owner never hands it a task on any
//! other crew, on any pass or a resumed run. The restriction is the run's
//! input; it changes no configuration, no task crew and not the owner's
//! before-PR reviewer, which must still run here.

use super::*;

fn claimed_tasks(pair: &Pair) -> BTreeSet<String> {
    pair.owner_claims()
        .iter()
        .map(|claim| claim["claim"]["task_id"].as_str().unwrap().to_string())
        .collect()
}

fn declared_crews(pull: &Value) -> BTreeSet<String> {
    pull["crews"]["runnable"]
        .as_array()
        .expect("declared crews")
        .iter()
        .map(|crew| crew.as_str().unwrap().to_string())
        .collect()
}

fn before_pr_owner(crew: &str) -> String {
    format!("[review]\nbefore_pr = true\n\n[operation]\nreview_crew = \"{crew}\"\n")
}

/// A drain restricted to `sol` and `luna` claims only their tasks, on every
/// refill and after a resume, while tasks on crews it can run but was not
/// allowed stay in the owner's backlog with their crews unchanged.
#[test]
fn a_restricted_pull_drain_claims_only_allowed_crews_on_every_refill_and_resume() {
    if !isolated(
        module_path!(),
        "a_restricted_pull_drain_claims_only_allowed_crews_on_every_refill_and_resume",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("opus"), Some("sol"), Some("antigravity"), Some("luna")]);
    let (opus, sol, antigravity, luna) = (
        pair.tasks[0].clone(),
        pair.tasks[1].clone(),
        pair.tasks[2].clone(),
        pair.tasks[3].clone(),
    );
    let input = json!({"destination": pair.destination, "allowed_crews": ["luna", "sol"]});
    let drain = pair.run_drain_with_input(input.clone());

    let first_leaf = pair.running_leaf(&drain, 1);
    let first = pair.pass(&drain);
    assert_eq!(first["crews"]["allowed"], json!(["luna", "sol"]), "{first}");
    assert_eq!(claimed_tasks(&pair).len(), 1);

    // The source drain ends and a resumed run takes over with the same input
    // and run state, as `orbit job resume` submits it.
    pair.leaf_fails_with(&first_leaf, "candidate validation failed");
    pair.follower_jobs
        .finalize_job_run(&drain, JobRunState::Failed, Utc::now(), None)
        .unwrap();
    let resumed = pair.run_drain_with_input(input);
    let mut state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    state.run_id = resumed.clone();
    pair.follower.write_run_state(&resumed, &state).unwrap();
    let next_leaf = pair.running_leaf(&resumed, 1);
    pair.leaf_fails_with(&next_leaf, "candidate validation failed");
    for _ in 0..3 {
        pair.pass(&resumed);
    }

    assert_eq!(claimed_tasks(&pair), BTreeSet::from([sol, luna]));
    let allowed = BTreeSet::from(["luna".to_string(), "sol".to_string()]);
    let pulls = pair.wire.calls("orbit.task.pull");
    assert!(pulls.len() >= 3, "{pulls:?}");
    for pull in &pulls {
        let declared = declared_crews(pull);
        assert!(
            !declared.is_empty() && declared.is_subset(&allowed),
            "{pull}"
        );
    }
    for (task, crew) in [(&opus, "opus"), (&antigravity, "antigravity")] {
        let shown = pair.owner_task(task);
        assert_eq!(shown["status"], "backlog", "{shown}");
        assert_eq!(
            shown["crew"], crew,
            "the owner's task crew is untouched: {shown}"
        );
    }
    let idle = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .rev()
        .filter(|record| record.phase == LocalPullPhase::Idle)
        .find_map(|record| record.receipt)
        .expect("the owner answered idle");
    for task in [&opus, &antigravity] {
        assert!(
            idle.crew_unavailable
                .iter()
                .any(|skipped| &skipped.task_id == task),
            "{idle:#?}"
        );
    }
    let window = pair
        .follower
        .pull_drain_crew_window(&resumed)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert_eq!(
        window.allowed,
        Some(vec!["luna".to_string(), "sol".to_string()])
    );
    assert!(
        window.excluded.is_empty(),
        "a crew outside the restriction is not reported as broken: {window:#?}"
    );
    assert!(
        window
            .describe()
            .iter()
            .any(|line| line == "allowed (--allow-crew): luna, sol"),
        "{window:#?}"
    );
}

/// The restriction selects implementation crews only: an owner whose
/// before-PR review runs on `opus` still hands a `sol`-only drain its `sol`
/// task, because `opus` runs here.
#[test]
fn a_restriction_leaves_the_owners_before_pr_reviewer_usable() {
    if !isolated(
        module_path!(),
        "a_restriction_leaves_the_owners_before_pr_reviewer_usable",
    ) {
        return;
    }
    let pair = Pair::with_owner_config(&before_pr_owner("opus"), &[Some("sol")]);
    let drain = pair
        .run_drain_with_input(json!({"destination": pair.destination, "allowed_crews": ["sol"]}));

    let pass = pair.pass(&drain);

    assert!(pass["refusal"].is_null(), "{pass}");
    assert_eq!(
        claimed_tasks(&pair),
        BTreeSet::from([pair.tasks[0].clone()])
    );
    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(
        declared_crews(&pulls[0]),
        BTreeSet::from(["sol".to_string()])
    );
}

/// The restriction never stands in for a reviewer that cannot run: with the
/// review crew's provider missing here, a restricted drain refuses before it
/// requests a claim, naming the reviewer.
#[test]
fn a_restricted_drain_refuses_before_claiming_when_the_reviewer_cannot_run() {
    if !isolated(
        module_path!(),
        "a_restricted_drain_refuses_before_claiming_when_the_reviewer_cannot_run",
    ) {
        return;
    }
    let pair = Pair::with_owner_config(&before_pr_owner("opus"), &[Some("sol")]);
    pair.follower_cli("claude", "orbit-test-no-such-provider-cli");
    let drain = pair
        .run_drain_with_input(json!({"destination": pair.destination, "allowed_crews": ["sol"]}));

    let pass = pair.pass(&drain);

    let refusal = pass["refusal"].as_str().unwrap_or_default();
    assert!(
        refusal.contains("before_pr_reviewer_unavailable") && refusal.contains("opus"),
        "{pass}"
    );
    assert!(pair.wire.calls("orbit.task.pull").is_empty(), "{pass}");
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// When nothing the restriction allows can run here, the drain says so and
/// requests nothing, rather than idling or falling back to another crew.
#[test]
fn a_restriction_with_no_runnable_crew_refuses_without_requesting() {
    if !isolated(
        module_path!(),
        "a_restriction_with_no_runnable_crew_refuses_without_requesting",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol"), Some("antigravity")]);
    pair.follower_cli("antigravity", "orbit-test-no-such-provider-cli");
    let drain = pair.run_drain_with_input(
        json!({"destination": pair.destination, "allowed_crews": ["antigravity"]}),
    );

    let pass = pair.pass(&drain);

    let refusal = pass["refusal"].as_str().unwrap_or_default();
    assert!(
        refusal.starts_with("no_runnable_crew:") && refusal.contains("--allow-crew"),
        "{pass}"
    );
    assert_eq!(pass["admitting"], false, "{pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty(), "{pass}");
    assert!(pair.owner_claims().is_empty());
}

/// Unknown and blank names refuse at submission, before the owner is probed
/// or any run exists.
#[test]
fn an_unknown_or_blank_restriction_refuses_before_the_probe() {
    if !isolated(
        module_path!(),
        "an_unknown_or_blank_restriction_refuses_before_the_probe",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let selector = format!("{OWNER}/ws_replica");
    for (names, expected) in [
        (vec!["no-such-crew".to_string()], "no-such-crew"),
        (
            vec!["sol".to_string(), " ".to_string()],
            "must not be empty",
        ),
    ] {
        let refused = pair
            .follower
            .submit_workspace_pull_run(
                orbit_core::WorkspacePullRequest {
                    selector: &selector,
                    for_seconds: Some(60),
                    max_active_leaf_runs: None,
                    allowed_crews: &names,
                    actor: None,
                },
                orbit_types::workflow::JobRunTrigger::cli(),
            )
            .expect_err("an invalid restriction refuses");
        assert!(refused.to_string().contains(expected), "{refused}");
    }
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(
        pair.follower_jobs
            .list_job_runs("workspace_pull_pipeline")
            .unwrap()
            .is_empty()
    );
}

/// Through the real submission and job, the restriction is persisted by
/// canonical name on the run input and only an allowed task is claimed.
#[test]
fn a_submitted_restriction_is_persisted_and_honored_by_the_job() {
    if !isolated(
        module_path!(),
        "a_submitted_restriction_is_persisted_and_honored_by_the_job",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("opus"), Some("sol")]);

    let drain = super::single_pass::run_windowless_drain(&pair, 2, &[" sol ".to_string()]);

    assert_eq!(pair.run_state(&drain), JobRunState::Success);
    let input = pair
        .follower_jobs
        .get_job_run(&drain)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    assert_eq!(input["allowed_crews"], json!(["sol"]), "{input}");
    assert_eq!(
        claimed_tasks(&pair),
        BTreeSet::from([pair.tasks[1].clone()])
    );
    let shown = pair.owner_task(&pair.tasks[0]);
    assert_eq!(shown["status"], "backlog", "{shown}");
    assert_eq!(shown["crew"], "opus", "{shown}");
}
