//! [ORB-15374] The owner's clock tick files one repair task per red base
//! failure: the first hold for a command and failure set files it, naming
//! the failures, the first red base and the held task, and every later hold
//! with the same failures, on another task or a later base, links to it.

use std::time::{Duration, Instant};

use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{RuntimeHost, baseline_red_hold_update};
use orbit_types::task::{Task, TaskPriority, TaskRelationType};
use orbit_types::workflow::{BaselineRedFailures, BaselineRedHold};
use serde_json::Value;

use super::review_baseline_hold::{git, held_on_red_base};
use super::review_gate_audit::Fixture;

/// The check `held_on_red_base` holds for; it prints `suite::broken` failing.
const CHECK: &str = "sh check.sh";
const REPAIR_REF_SYSTEM: &str = "baseline-red";

fn tick(runtime: &OrbitRuntime) -> orbit_core::application::task::BaselineHoldRefresh {
    runtime
        .run_baseline_hold_tick(Instant::now() + Duration::from_secs(60))
        .expect("the tick runs")
}

fn failing(tests: &[&str]) -> BaselineRedFailures {
    BaselineRedFailures::new(tests.iter().map(|test| format!("test {test}")), "")
}

/// A new task, held by run finalization's hold update for `hold`.
fn held_task(fixture: &Fixture, title: &str, hold: &BaselineRedHold) -> String {
    let task = fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "A task whose delivery found the base red.".into(),
            acceptance_criteria: vec!["The change is delivered.".into()],
            ..Default::default()
        })
        .unwrap();
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &task.id,
        baseline_red_hold_update("task_pr_pipeline", hold),
    )
    .unwrap();
    assert_eq!(
        fixture.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Backlog,
        "the task is held"
    );
    task.id
}

/// Every repair task the tick filed, oldest first.
fn repairs(fixture: &Fixture) -> Vec<Task> {
    let mut repairs = fixture
        .runtime
        .list_tasks_filtered(None, None, None, None, None, Some(REPAIR_REF_SYSTEM))
        .unwrap();
    repairs.sort_by_key(|task| task.created_at);
    repairs
}

fn related(repair: &Task) -> Vec<String> {
    repair
        .relations
        .iter()
        .filter(|relation| relation.relation_type == TaskRelationType::RelatedTo)
        .map(|relation| relation.target.clone())
        .collect()
}

/// The reason of the latest verdict recorded on `task_id`'s hold.
fn latest_verdict(fixture: &Fixture, task_id: &str) -> String {
    let history = fixture.runtime.get_task_history(task_id).unwrap();
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.event == "baseline_red_hold_verdict")
        .unwrap_or_else(|| panic!("no verdict on {task_id}: {history:?}"));
    let verdict: Value = serde_json::from_str(entry.note.as_deref().unwrap()).unwrap();
    verdict["reason"].as_str().unwrap().to_string()
}

/// Commit a change on `main` that leaves the check red, returning the new tip.
fn move_base_still_red(fixture: &Fixture) -> String {
    git(&fixture.repo, &["checkout", "--quiet", "main"]);
    std::fs::write(fixture.repo.join("unrelated.txt"), "moved\n").unwrap();
    git(&fixture.repo, &["add", "unrelated.txt"]);
    git(
        &fixture.repo,
        &["commit", "--quiet", "-m", "move the base, still red"],
    );
    let tip = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["checkout", "--quiet", "candidate"]);
    tip
}

#[test]
fn the_first_hold_files_one_repair_task_and_later_holds_link_to_it_across_tasks_and_bases() {
    if !super::dispatch_admission::isolated(
        "baseline_red_repair::the_first_hold_files_one_repair_task_and_later_holds_link_to_it_across_tasks_and_bases",
    ) {
        return;
    }
    // The fixture's own task is held by a hold written before holds kept
    // their failures: it has nothing to key a repair by.
    let (fixture, legacy) = held_on_red_base();
    assert!(legacy.failures.is_empty());
    let red = legacy.base_sha.clone();
    let hold = |run_id: &str, base_sha: &str, tests: &[&str]| BaselineRedHold {
        base_ref: "main".into(),
        base_sha: base_sha.into(),
        command: CHECK.into(),
        run_id: run_id.into(),
        selection: None,
        failures: failing(tests),
    };

    // The first hold naming `suite::broken` files the repair task.
    let first = held_task(
        &fixture,
        "First held task",
        &hold("jrun-red-1", &red, &["suite::broken"]),
    );
    let refresh = tick(&fixture.runtime);
    assert_eq!(refresh.repairs_filed.len(), 1, "{refresh:?}");
    let filed = repairs(&fixture);
    assert_eq!(filed.len(), 1, "one repair task: {filed:?}");
    let repair = &filed[0];
    assert_eq!(repair.id, refresh.repairs_filed[0]);
    assert_eq!(repair.status, TaskStatus::Backlog);
    assert_eq!(repair.priority, TaskPriority::High);
    assert!(
        repair.title.contains("`suite::broken`"),
        "the title names the test: {}",
        repair.title
    );
    for named in ["`test suite::broken`", CHECK, red.as_str(), first.as_str()] {
        assert!(
            repair.description.contains(named),
            "the repair task names {named}: {}",
            repair.description
        );
    }
    assert_eq!(related(repair), vec![first.clone()]);
    assert!(
        !related(repair).contains(&fixture.task_id),
        "a hold without failures files and links nothing"
    );
    assert!(
        latest_verdict(&fixture, &first).contains("failing: `suite::broken`"),
        "the hold's verdict names the failing test: {}",
        latest_verdict(&fixture, &first)
    );

    // A second task held for the same failure links to it; nothing is filed.
    let second = held_task(
        &fixture,
        "Second held task",
        &hold("jrun-red-2", &red, &["suite::broken"]),
    );
    let refresh = tick(&fixture.runtime);
    assert!(refresh.repairs_filed.is_empty(), "{refresh:?}");
    assert_eq!(refresh.repairs_linked, vec![second.clone()], "{refresh:?}");
    let filed = repairs(&fixture);
    assert_eq!(filed.len(), 1, "{filed:?}");
    assert_eq!(related(&filed[0]), vec![first.clone(), second.clone()]);
    let comments = fixture.runtime.get_task_comments(&filed[0].id).unwrap();
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains(&second)),
        "the repair task comments the newly held task: {comments:?}"
    );

    // A tick with nothing new changes nothing.
    let refresh = tick(&fixture.runtime);
    assert!(
        refresh.repairs_filed.is_empty() && refresh.repairs_linked.is_empty(),
        "{refresh:?}"
    );
    assert_eq!(repairs(&fixture).len(), 1);

    // The base moves and stays red the same way: a task held on the new tip
    // links to the same repair task, which still names the first red base.
    let moved = move_base_still_red(&fixture);
    fixture.runtime.refresh_baseline_holds(None).unwrap();
    assert!(
        latest_verdict(&fixture, &first).contains(&format!(
            "moved to {moved}, where required validation `{CHECK}` still fails; failing: \
             `suite::broken`"
        )),
        "the moved base's verdict names what still fails: {}",
        latest_verdict(&fixture, &first)
    );
    let third = held_task(
        &fixture,
        "Third held task",
        &hold("jrun-red-3", &moved, &["suite::broken"]),
    );
    let refresh = tick(&fixture.runtime);
    assert!(refresh.repairs_filed.is_empty(), "{refresh:?}");
    assert_eq!(refresh.repairs_linked, vec![third.clone()], "{refresh:?}");
    let filed = repairs(&fixture);
    assert_eq!(filed.len(), 1, "{filed:?}");
    assert_eq!(related(&filed[0]), vec![first, second, third]);
    assert!(filed[0].description.contains(&red));

    // Another failure set on the same command is another repair.
    let other = held_task(
        &fixture,
        "Fourth held task",
        &hold("jrun-red-4", &moved, &["suite::broken", "suite::other"]),
    );
    let refresh = tick(&fixture.runtime);
    assert_eq!(refresh.repairs_filed.len(), 1, "{refresh:?}");
    let filed = repairs(&fixture);
    assert_eq!(filed.len(), 2, "{filed:?}");
    assert_eq!(related(&filed[1]), vec![other]);
}

#[test]
fn a_command_that_names_no_test_files_one_repair_from_its_output_tail() {
    if !super::dispatch_admission::isolated(
        "baseline_red_repair::a_command_that_names_no_test_files_one_repair_from_its_output_tail",
    ) {
        return;
    }
    let (fixture, legacy) = held_on_red_base();
    let output = "checking README.md\nREADME.md: lint is red\nmake: *** [Makefile:3: ci-lint] \
                  Error 2\n";
    let hold = BaselineRedHold {
        base_ref: "main".into(),
        base_sha: legacy.base_sha.clone(),
        command: "make ci-lint".into(),
        run_id: "jrun-lint-1".into(),
        selection: None,
        failures: BaselineRedFailures::new(Vec::new(), output),
    };
    assert!(hold.failures.identities.is_empty());

    let first = held_task(&fixture, "First lint-held task", &hold);
    let refresh = tick(&fixture.runtime);
    assert_eq!(refresh.repairs_filed.len(), 1, "{refresh:?}");
    let second = held_task(
        &fixture,
        "Second lint-held task",
        &BaselineRedHold {
            run_id: "jrun-lint-2".into(),
            ..hold.clone()
        },
    );
    let refresh = tick(&fixture.runtime);
    assert_eq!(refresh.repairs_linked, vec![second.clone()], "{refresh:?}");

    let filed = repairs(&fixture);
    assert_eq!(filed.len(), 1, "{filed:?}");
    let repair = &filed[0];
    assert!(
        repair.title.contains("`make ci-lint`"),
        "with no test to name, the title names the command: {}",
        repair.title
    );
    assert!(
        repair.description.contains("README.md: lint is red"),
        "the repair task quotes the base output's tail: {}",
        repair.description
    );
    assert_eq!(related(repair), vec![first.clone(), second]);
    assert!(
        latest_verdict(&fixture, &first).contains("`README.md: lint is red`"),
        "the verdict names the output's tail: {}",
        latest_verdict(&fixture, &first)
    );
}
