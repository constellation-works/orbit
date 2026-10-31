//! [ORB-16855] `skip-review` and `skip-validation` hold only while the
//! candidate changes nothing outside `workflow.light_delivery_paths`: a
//! documentation-only candidate runs no required command and admits no
//! reviewer, and one that also changes a source path is validated and
//! reviewed as usual. Both decisions are recorded in the task's history.

use std::collections::HashMap;

use chrono::Utc;
use orbit_core::application::automation::evaluate_auto_task;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::{RuntimeHost, execute_deterministic_action};
use orbit_types::task::{
    LIGHT_DELIVERY_APPLIED_EVENT, LIGHT_DELIVERY_IGNORED_EVENT, ReviewSkipProfile, SKIP_REVIEW_TAG,
    SKIP_VALIDATION_TAG,
};
use orbit_types::workflow::automation::DirectLandingRequest;
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

fn git(fixture: &Fixture, args: &[&str]) -> String {
    let mut command = crate::git_test_support::command();
    let output = command
        .args(args)
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// The before-PR fixture requiring `required`, its task tagged with both
/// light-delivery tags by an operator, and its candidate replaced by one
/// commit writing `paths` over `main`.
fn tagged_fixture(required: &str, paths: &[&str]) -> Fixture {
    let fixture = Fixture::new_with_required_commands(&[required]);
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                tags: Some(vec![SKIP_REVIEW_TAG.into(), SKIP_VALIDATION_TAG.into()]),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    git(&fixture, &["reset", "--quiet", "--hard", "main"]);
    for path in paths {
        let file = fixture.repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "changed\n").unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "candidate"]);
    fixture
}

fn validate(fixture: &Fixture) -> Value {
    let mut input = fixture.input.clone();
    input["base_sha"] = json!(git(fixture, &["rev-parse", "main"]));
    input["base_ref"] = json!("main");
    execute_deterministic_action(
        &fixture.runtime,
        "candidate_validate",
        &json!({}),
        &input,
        false,
        &HashMap::new(),
        None,
    )
    .unwrap()
}

/// The `(event, note)` of each light-delivery history entry naming `tag`.
fn decisions(fixture: &Fixture, tag: &str) -> Vec<(String, String)> {
    fixture
        .runtime
        .get_task_history(&fixture.task_id)
        .unwrap()
        .into_iter()
        .filter(|entry| {
            [LIGHT_DELIVERY_APPLIED_EVENT, LIGHT_DELIVERY_IGNORED_EVENT]
                .contains(&entry.event.as_str())
        })
        .map(|entry| (entry.event, entry.note.unwrap_or_default()))
        .filter(|(_, note)| note.contains(&format!("`{tag}`")))
        .collect()
}

fn docs_only_candidate_skips_validation_and_every_review() {
    // A required command that always fails proves none ran.
    let mut fixture = tagged_fixture("false", &["docs/guide.md", "README.md"]);

    let validated = validate(&fixture);
    assert_eq!(validated["decision"], "skipped_light_delivery");
    assert_eq!(validated["commands"], json!([]));
    assert_eq!(validated["validation"], json!([]));

    fixture.admit();
    assert_eq!(fixture.input["admission"]["applies"], false);
    assert_eq!(
        fixture.input["admission"]["reason"],
        "skip_review_light_delivery"
    );
    let settled = fixture.settle().unwrap();
    assert_eq!(
        settled["gate"], "not_required",
        "the PR opens and lands without a reviewed head: {settled}"
    );

    for tag in [SKIP_VALIDATION_TAG, SKIP_REVIEW_TAG] {
        let recorded = decisions(&fixture, tag);
        assert_eq!(recorded.len(), 1, "one decision per tag: {recorded:?}");
        assert_eq!(recorded[0].0, LIGHT_DELIVERY_APPLIED_EVENT);
    }
    // A retried step records the same decision once.
    validate(&fixture);
    assert_eq!(decisions(&fixture, SKIP_VALIDATION_TAG).len(), 1);
}

fn a_path_outside_the_allow_list_ignores_both_tags() {
    let mut fixture = tagged_fixture("true", &["docs/guide.md", "src/lib.rs"]);

    let validated = validate(&fixture);
    assert_eq!(validated["decision"], "passed");
    assert_eq!(validated["commands"], json!(["true"]));

    fixture.admit();
    assert_eq!(fixture.input["admission"]["applies"], true);
    assert_eq!(fixture.input["admission"]["decision"], "admitted");

    for tag in [SKIP_VALIDATION_TAG, SKIP_REVIEW_TAG] {
        let recorded = decisions(&fixture, tag);
        assert_eq!(recorded.len(), 1, "one decision per tag: {recorded:?}");
        assert_eq!(recorded[0].0, LIGHT_DELIVERY_IGNORED_EVENT);
        assert!(
            recorded[0].1.contains("src/lib.rs"),
            "history names the path that voided the tag: {}",
            recorded[0].1
        );
    }
}

/// Snapshot every task and its history, plus the consumer's durable state and
/// audit records, through the public runtime/store APIs.
fn delivery_state(fixture: &Fixture, consumer: &str) -> Value {
    let tasks = fixture.runtime.list_tasks().unwrap();
    let histories = tasks
        .iter()
        .map(|task| {
            (
                task.id.to_string(),
                fixture.runtime.get_task_history(&task.id).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let store = fixture.runtime.automation_store().unwrap();
    json!({
        "tasks": tasks,
        "histories": histories,
        "states": store.automation_states_page("", None, 100).unwrap(),
        "receipts": store.automation_receipts(consumer, 100).unwrap(),
        "recoveries": store.automation_recoveries(consumer, 100).unwrap(),
        "waivers": store.automation_waivers(consumer, 100).unwrap(),
        "definition": fixture.runtime.auto_task_show("preview-review").unwrap(),
    })
}

fn mapped_delivery_preview(guard_exit: Option<u8>) {
    let guard_config = guard_exit.map_or("", |_| "guard = \"guard.sh\"\n");
    let mut fixture = Fixture::new_with_config(
        &[],
        &format!("[workflow.review_skip_tags.mapped]\npaths = [\"**\"]\n{guard_config}"),
    );
    fixture.runtime = fixture
        .runtime
        .with_automation_machine_identity(Some("hm_preview".into()));
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                tags: Some(vec!["mapped".into()]),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    git(&fixture, &["checkout", "--quiet", "main"]);
    let sentinel = fixture.repo.join(".orbit/guard-ran");
    if let Some(exit) = guard_exit {
        // Absolute: the guard runs in a detached checkout of the candidate, not
        // in the owner's checkout, so a relative path would land elsewhere.
        std::fs::write(fixture.repo.join("guard.sh"), format!(
            "#!/bin/sh\nprintf 'base:%s:%s:%s:%s:%s\\n' \"$1\" \"$2\" \"$ORBIT_REVIEW_SKIP_BASE\" \"$ORBIT_REVIEW_SKIP_HEAD\" \"$ORBIT_REVIEW_SKIP_TAG\" > {}\nexit {exit}\n",
            sentinel.display()
        )).unwrap();
        git(&fixture, &["add", "guard.sh"]);
        git(&fixture, &["commit", "--quiet", "-m", "base guard"]);
    }
    fixture
        .runtime
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": "preview-review",
                "schedule": {"deliveries_landed": {
                    "branch": "main", "owner_machine": "hm_preview", "threshold": 2,
                    "max_wait_minutes": 60, "coverage": "landed_code_review_v1",
                }},
                "template": {"title": "Review landed deliveries"},
            }),
        )
        .unwrap();
    fixture
        .runtime
        .auto_task_toggle("preview-review", true)
        .unwrap();
    let definition = fixture
        .runtime
        .auto_task_show("preview-review")
        .unwrap()
        .unwrap();
    let evaluate = |dry_run| {
        let now = std::time::Instant::now();
        orbit_core::test_support::with_delivery_source_clock(
            move || now,
            || evaluate_auto_task(&fixture.runtime, &definition, dry_run, Utc::now()).unwrap(),
        )
    };
    let baseline = evaluate(false);
    assert_eq!(baseline.reason, "baselined", "{baseline:?}");
    let consumer = baseline.state.unwrap().consumer;
    let before = git(&fixture, &["rev-parse", "HEAD"]);
    std::fs::write(fixture.repo.join("candidate.txt"), "landed\n").unwrap();
    // The candidate's replacement must never run: guards come from the base.
    if guard_exit.is_some() {
        std::fs::write(
            fixture.repo.join("guard.sh"),
            "#!/bin/sh\necho candidate > .orbit/guard-ran\nexit 99\n",
        )
        .unwrap();
    }
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "landed candidate"]);
    let after = git(&fixture, &["rev-parse", "HEAD"]);
    fixture
        .runtime
        .record_direct_landing_intent(&DirectLandingRequest {
            run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
            branch: "main".into(),
            before_commit: before.clone(),
            after_commit: after.clone(),
            task_ids: vec![fixture.task_id.clone()],
            handoff_id: None,
        })
        .unwrap();

    let initial = delivery_state(&fixture, &consumer);
    for _ in 0..2 {
        let preview = evaluate(true);
        assert!(
            !sentinel.exists(),
            "preview must not execute the mapped guard"
        );
        assert_eq!(
            delivery_state(&fixture, &consumer),
            initial,
            "preview must leave every task/history and automation checkpoint unchanged"
        );
        let state = preview.state.unwrap();
        assert_eq!(
            state.observed.commit, after,
            "preview observes the new landing"
        );
        assert_eq!(
            state.pending.len(),
            1,
            "preview conservatively retains mapped work"
        );
        assert_eq!(state.pending[0].task_ids, vec![fixture.task_id.clone()]);
    }

    let applied = evaluate(false).state.unwrap();
    assert_eq!(applied.observed.commit, after);
    let recorded = decisions(&fixture, "mapped");
    assert_eq!(
        recorded.len(),
        1,
        "real evaluation records the decision once"
    );
    let passes = guard_exit.is_none_or(|exit| exit == 0);
    assert_eq!(
        recorded[0].0,
        if passes {
            LIGHT_DELIVERY_APPLIED_EVENT
        } else {
            LIGHT_DELIVERY_IGNORED_EVENT
        }
    );
    assert_eq!(applied.pending.len(), usize::from(!passes));
    assert_eq!(
        applied.covered.commit,
        if passes { &after } else { &before }.as_str()
    );
    if guard_exit.is_some() {
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            format!("base:{before}:{after}:{before}:{after}:mapped\n")
        );
    }
    assert_eq!(
        fixture
            .runtime
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap(),
        Some(applied)
    );
}

fn unguarded_mapped_delivery_preview_is_read_only() {
    mapped_delivery_preview(None);
}

#[cfg(unix)]
fn guarded_mapped_delivery_preview_is_read_only() {
    for exit in [0, 1] {
        mapped_delivery_preview(Some(exit));
    }
}

#[cfg(unix)]
crate::scenario_table!(
    guarded_delivery_preview_scenarios,
    guarded_mapped_delivery_preview_is_read_only,
);

/// A mapped guard runs in a checkout of the judged candidate [ORB-18116]: an
/// owner whose own checkout sits on the base, holding an edit, and a later
/// tree reaches the follower's decision, because the guard (read from the
/// base) sees the candidate's `HEAD` and files. The owner's branch, `HEAD`
/// and edits are left as they were.
fn a_mapped_guard_sees_the_candidate_not_the_owners_checkout() {
    const TAG: &str = "docs-review";
    let fixture = Fixture::new_with_required_commands(&["true"]);
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                tags: Some(vec![TAG.into()]),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    git(&fixture, &["reset", "--quiet", "--hard", "main"]);
    git(&fixture, &["checkout", "--quiet", "main"]);
    // The guard exists at the base. It requires the candidate's `HEAD` and a
    // file only the candidate holds.
    let guard = fixture.repo.join("scripts/skip-guard.sh");
    std::fs::create_dir_all(guard.parent().unwrap()).unwrap();
    std::fs::write(
        &guard,
        "#!/bin/sh\ntest \"$(git rev-parse HEAD)\" = \"$2\" && grep -q candidate-only docs/note.md\n",
    )
    .unwrap();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "guard"]);
    let base = git(&fixture, &["rev-parse", "HEAD"]);

    git(&fixture, &["checkout", "--quiet", "-b", "candidate-branch"]);
    std::fs::create_dir_all(fixture.repo.join("docs")).unwrap();
    std::fs::write(fixture.repo.join("docs/note.md"), "candidate-only\n").unwrap();
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "--quiet", "-m", "candidate"]);
    let head = git(&fixture, &["rev-parse", "HEAD"]);
    // A later tree the guard must not read instead.
    std::fs::write(fixture.repo.join("docs/note.md"), "later\n").unwrap();
    git(&fixture, &["commit", "--quiet", "-am", "later"]);
    let later = git(&fixture, &["rev-parse", "HEAD"]);

    // The owner's checkout: the base, with an edit it must keep.
    git(&fixture, &["checkout", "--quiet", "main"]);
    std::fs::write(fixture.repo.join("owner-edit.txt"), "owner work\n").unwrap();
    let status = git(&fixture, &["status", "--short"]);

    let decide = |head: &str| {
        orbit_engine::review_skip_decision(
            &fixture.runtime,
            &fixture.repo,
            &base,
            head,
            std::slice::from_ref(&fixture.task_id),
            TAG,
            &ReviewSkipProfile {
                paths: vec!["docs/**".into(), "scripts/**".into()],
                guard: Some("scripts/skip-guard.sh".into()),
            },
        )
        .unwrap()
        .expect("the task carries the tag")
    };
    assert!(
        decide(&head).applies(),
        "the guard saw the candidate's HEAD and files"
    );
    assert!(
        !decide(&later).applies(),
        "the guard judges the supplied head, not a later tree"
    );
    assert_eq!(git(&fixture, &["rev-parse", "HEAD"]), base);
    assert_eq!(git(&fixture, &["branch", "--show-current"]), "main");
    assert_eq!(git(&fixture, &["status", "--short"]), status);
}

crate::scenario_table!(
    light_delivery_scenarios,
    docs_only_candidate_skips_validation_and_every_review,
    a_path_outside_the_allow_list_ignores_both_tags,
    unguarded_mapped_delivery_preview_is_read_only,
    a_mapped_guard_sees_the_candidate_not_the_owners_checkout,
);
