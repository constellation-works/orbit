//! [ORB-15374] The owner's clock tick files one repair task per red base
//! failure: the first hold for a command and failure set files it, naming
//! the failures, the first red base and the held task, and every later hold
//! with the same failures, on another task or a later base, links to it.

use std::time::{Duration, Instant};

use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{BaselineHoldStatus, RuntimeHost, baseline_red_hold_update};
use orbit_types::task::{ExternalRef, Task, TaskPriority, TaskRelationType};
use orbit_types::workflow::{BaselineRedFailures, BaselineRedHold};
use serde_json::Value;

use super::review_baseline_hold::{git, held_on_red_base};
use super::review_gate_audit::Fixture;

/// The check `held_on_red_base` holds for; it prints `suite::broken` failing.
const CHECK: &str = "sh check.sh";
const REPAIR_REF_SYSTEM: &str = "baseline-red";
/// A check that passes on any tree.
const PASSING: &str = "#!/bin/sh\nexit 0\n";

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

#[test]
fn complete_failure_sets_key_repairs_before_display_truncation() {
    if !super::dispatch_admission::isolated(
        "baseline_red_repair::complete_failure_sets_key_repairs_before_display_truncation",
    ) {
        return;
    }
    let (fixture, legacy) = held_on_red_base();
    let common = (0..20)
        .map(|i| format!("test runtime suite::a_{i:02}"))
        .collect::<Vec<_>>();
    let mut left = common.clone();
    left.push("test runtime suite::z_left".into());
    let mut right = common;
    right.push("test runtime suite::z_right".into());
    // Both records have 20 entries and one omitted identity. The long-name
    // pair instead differs beyond a UTF-8 display cut in its only entry.
    let prefix = format!("test suite::long_{}", "é".repeat(130));
    let cases = [
        (left.clone(), right),
        (
            vec![format!("{prefix}left")],
            vec![format!("{prefix}right")],
        ),
    ];
    let hold = |identities: Vec<String>| BaselineRedHold {
        base_ref: "main".into(),
        base_sha: legacy.base_sha.clone(),
        command: CHECK.into(),
        run_id: "jrun-truncated".into(),
        selection: None,
        failures: BaselineRedFailures::new(identities, ""),
    };
    let left_hold = hold(left.clone());
    let mut first = String::new();
    for (i, (left, right)) in cases.into_iter().enumerate() {
        let left = hold(left);
        let right = hold(right);
        assert_eq!(left.failures.identities, right.failures.identities);
        assert_eq!(left.failures.omitted, right.failures.omitted);
        assert_eq!(left.failures.tail, right.failures.tail);
        assert_eq!(left.failures.describe(), right.failures.describe());
        assert_ne!(
            left.failures.identity_digest,
            right.failures.identity_digest
        );
        for failures in [&left.failures, &right.failures] {
            assert!(failures.identities.len() <= 20);
            assert!(failures.identities.iter().all(|name| name.len() <= 243));
            assert!(serde_json::to_vec(failures).unwrap().len() < 6000);
        }
        let left_id = held_task(&fixture, &format!("Left collision case {i}"), &left);
        let right_id = held_task(&fixture, &format!("Right collision case {i}"), &right);
        if i == 0 {
            first = left_id.clone();
        }
        let refresh = tick(&fixture.runtime);
        assert_eq!(refresh.repairs_filed.len(), 2, "{refresh:?}");
        let filed = repairs(&fixture);
        assert_eq!(filed.len(), (i + 1) * 2, "{filed:?}");
        for held_id in [left_id, right_id] {
            assert_eq!(
                filed
                    .iter()
                    .filter(|repair| related(repair) == vec![held_id.clone()])
                    .count(),
                1,
                "each complete set gets its own repair: {filed:?}"
            );
        }
    }
    let filed = repairs(&fixture);
    let keys = filed
        .iter()
        .flat_map(|repair| repair.external_refs.iter())
        .map(|reference| (&reference.system, &reference.id))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), 4, "the exact repair references are distinct");

    // Reordering, whitespace, duplicate names and the bare libtest echo of
    // a qualified nextest name leave the complete normalized set unchanged.
    let reordered = left
        .iter()
        .rev()
        .map(|name| format!("  {name}  "))
        .chain([left[0].clone(), "test suite::z_left".into(), " ".into()])
        .collect();
    let second_hold = hold(reordered);
    assert_eq!(second_hold.failures, left_hold.failures);
    let second = held_task(&fixture, "Reordered held task", &second_hold);
    let refresh = tick(&fixture.runtime);
    assert!(refresh.repairs_filed.is_empty(), "{refresh:?}");
    assert_eq!(refresh.repairs_linked, vec![second.clone()], "{refresh:?}");

    let moved = move_base_still_red(&fixture);
    fixture.runtime.refresh_baseline_holds(None).unwrap();
    let third = held_task(
        &fixture,
        "Same complete set on a new base",
        &BaselineRedHold {
            base_sha: moved,
            run_id: "jrun-truncated-later".into(),
            ..left_hold
        },
    );
    let refresh = tick(&fixture.runtime);
    assert!(refresh.repairs_filed.is_empty(), "{refresh:?}");
    assert_eq!(refresh.repairs_linked, vec![third.clone()], "{refresh:?}");
    let filed = repairs(&fixture);
    assert_eq!(filed.len(), 4);
    let repair = filed
        .iter()
        .find(|repair| related(repair).contains(&first))
        .unwrap();
    assert_eq!(related(repair), vec![first, second, third]);
}

#[test]
fn persisted_holds_without_a_digest_find_their_existing_repair() {
    if !super::dispatch_admission::isolated(
        "baseline_red_repair::persisted_holds_without_a_digest_find_their_existing_repair",
    ) {
        return;
    }
    let (fixture, legacy) = held_on_red_base();
    // A persisted hold from before the digest field existed, rather than a
    // new constructor whose identity could change alongside the key code.
    let text = format!(
        "[baseline_red] {} old hold",
        serde_json::json!({
            "base_ref": "main", "base_sha": legacy.base_sha, "command": CHECK,
            "run_id": "jrun-legacy", "failures": {"identities": ["test suite::legacy"]}
        })
    );
    let hold = BaselineRedHold::from_text(&text).expect("older holds stay readable");
    assert!(hold.failures.identity_digest.is_none());
    // Frozen pre-digest key for (sh check.sh, test suite::legacy, 0, empty tail).
    let repair = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Existing legacy repair".into(),
            description: "An open repair filed before failure digests.".into(),
            acceptance_criteria: vec!["The check passes.".into()],
            status: Some(TaskStatus::Backlog),
            external_refs: vec![
                ExternalRef::try_new(
                    REPAIR_REF_SYSTEM.into(),
                    "20b0aef7dc300e9deecff765".into(),
                    None,
                )
                .unwrap(),
            ],
            ..Default::default()
        })
        .unwrap();
    let first = held_task(&fixture, "Legacy held task", &hold);
    let second = held_task(
        &fixture,
        "Another legacy held task",
        &BaselineRedHold {
            run_id: "jrun-legacy-later".into(),
            ..hold
        },
    );
    let refresh = tick(&fixture.runtime);
    assert!(refresh.repairs_filed.is_empty(), "{refresh:?}");
    let filed = repairs(&fixture);
    assert_eq!(filed.len(), 1, "legacy repair is reused: {filed:?}");
    assert_eq!(filed[0].id, repair.id);
    let related = related(&filed[0])
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(related, [first, second].into_iter().collect());
}

/// [ORB-15412] A standing hold whose base already has a cached passing
/// result lifts on the tick without filing a repair for the failure the base
/// has since resolved.
#[test]
fn a_cached_green_base_lifts_a_hold_without_filing_its_repair() {
    if !super::dispatch_admission::isolated(
        "baseline_red_repair::a_cached_green_base_lifts_a_hold_without_filing_its_repair",
    ) {
        return;
    }
    let (fixture, legacy) = held_on_red_base();
    let hold = BaselineRedHold {
        base_ref: "main".into(),
        base_sha: legacy.base_sha.clone(),
        command: CHECK.into(),
        run_id: "jrun-green-1".into(),
        selection: None,
        failures: failing(&["suite::broken"]),
    };
    let held = held_task(&fixture, "Held task whose base turns green", &hold);

    // `main` fixes the failure, and the real status path judges the new tip
    // and records that passing result in the base-result cache.
    git(&fixture.repo, &["checkout", "--quiet", "main"]);
    std::fs::write(fixture.repo.join("check.sh"), PASSING).unwrap();
    git(
        &fixture.repo,
        &["commit", "--quiet", "-am", "base fixes the failure"],
    );
    git(&fixture.repo, &["checkout", "--quiet", "candidate"]);
    assert!(
        matches!(
            orbit_engine::baseline_hold_status(&fixture.runtime, &fixture.repo, &hold),
            BaselineHoldStatus::Lifted(_)
        ),
        "the passing tip lifts the hold"
    );

    let refresh = tick(&fixture.runtime);
    assert!(refresh.lifted.contains(&held), "{refresh:?}");
    assert!(
        refresh.repairs_filed.is_empty() && refresh.repairs_linked.is_empty(),
        "{refresh:?}"
    );
    let filed = repairs(&fixture);
    assert!(
        filed.is_empty(),
        "no repair for a resolved failure: {filed:?}"
    );
}
