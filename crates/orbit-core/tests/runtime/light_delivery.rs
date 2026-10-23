//! [ORB-16855] `skip-review` and `skip-validation` hold only while the
//! candidate changes nothing outside `workflow.light_delivery_paths`: a
//! documentation-only candidate runs no required command and admits no
//! reviewer, and one that also changes a source path is validated and
//! reviewed as usual. Both decisions are recorded in the task's history.

use std::collections::HashMap;

use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::execute_deterministic_action;
use orbit_types::task::{
    LIGHT_DELIVERY_APPLIED_EVENT, LIGHT_DELIVERY_IGNORED_EVENT, SKIP_REVIEW_TAG,
    SKIP_VALIDATION_TAG,
};
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

crate::scenario_table!(
    light_delivery_scenarios,
    docs_only_candidate_skips_validation_and_every_review,
    a_path_outside_the_allow_list_ignores_both_tags,
);
