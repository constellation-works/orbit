//! Readers of a delivery branch find its task under the workspace's
//! `branch.name_template`, and never a task whose id only prefixes it.

use orbit_types::task::names_task_delivery_pull_request;
use orbit_types::workflow::{
    ConventionTemplate, DEFAULT_BRANCH_PREFIX, Placeholder, default_branch_template,
};

fn is_orb_id(candidate: &str) -> bool {
    candidate
        .strip_prefix("ORB-")
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn branch_readers_find_the_task_under_default_and_custom_templates() {
    let default = default_branch_template();
    let custom = ConventionTemplate::parse_branch(
        "branch.name_template",
        "feature/{{ext.jira}}-{{task_id}}-{{hash}}",
    )
    .expect("custom template");
    let body = "Delivers ORB-10.";
    let cases = [
        (&default, "orbit/ORB-10-0123abcd"),
        (&custom, "feature/PROJ-7-ORB-10-0123abcd"),
        (&custom, "feature/-ORB-10-0123abcd"),
    ];
    for (template, branch) in cases {
        let owner = template.branch_task_id(branch, DEFAULT_BRANCH_PREFIX, &is_orb_id);
        assert_eq!(owner.as_deref(), Some("ORB-10"), "{branch}");
        assert!(
            template.branch_names_task(branch, DEFAULT_BRANCH_PREFIX, "ORB-10"),
            "{branch}"
        );
        assert!(
            !template.branch_names_task(branch, DEFAULT_BRANCH_PREFIX, "ORB-1"),
            "{branch}"
        );
        assert!(
            names_task_delivery_pull_request(template, branch, body, "ORB-10"),
            "{branch}"
        );
        assert!(
            !names_task_delivery_pull_request(template, branch, body, "ORB-1"),
            "{branch}"
        );
    }

    // A branch another template shapes is not this template's delivery.
    for (template, branch) in [
        (&default, "feature/PROJ-7-ORB-10-0123abcd"),
        (&custom, "orbit/ORB-10-0123abcd"),
    ] {
        assert_eq!(
            template.branch_task_id(branch, DEFAULT_BRANCH_PREFIX, &is_orb_id),
            None
        );
        assert!(
            !names_task_delivery_pull_request(template, branch, body, "ORB-10"),
            "{branch}"
        );
    }
}

fn assert_rendered_branch_owner(source: &str, external_ref: &str) {
    let template = ConventionTemplate::parse_branch("branch.name_template", source).unwrap();
    let branch = template.render(|placeholder| match placeholder {
        Placeholder::TaskId => "ORB-10".to_string(),
        Placeholder::Hash => "0123abcd".to_string(),
        Placeholder::ExternalRef(_) => external_ref.to_string(),
        _ => DEFAULT_BRANCH_PREFIX.to_string(),
    });
    assert_eq!(
        template
            .branch_task_id(&branch, DEFAULT_BRANCH_PREFIX, &is_orb_id)
            .as_deref(),
        Some("ORB-10"),
        "{branch}"
    );
    for (id, owns) in [("ORB-10", true), ("ORB-1", false)] {
        assert_eq!(
            template.branch_names_task(&branch, DEFAULT_BRANCH_PREFIX, id),
            owns,
            "{branch}: {id}"
        );
        assert_eq!(
            names_task_delivery_pull_request(
                &template,
                &branch,
                "Delivers ORB-10; follows ORB-1.",
                id
            ),
            owns,
            "{branch}: {id}"
        );
    }
}

#[test]
fn branch_readers_round_trip_an_adjacent_hash() {
    assert_rendered_branch_owner("feature/{{task_id}}{{hash}}", "");
}

#[test]
fn branch_readers_round_trip_an_alphanumeric_literal_after_the_task() {
    assert_rendered_branch_owner("feature/{{task_id}}0{{hash}}", "");
}

#[test]
fn branch_readers_ignore_a_sibling_token_in_an_external_ref() {
    assert_rendered_branch_owner("feature/{{ext.jira}}-{{task_id}}-{{hash}}", "X-ORB-1");
}

#[test]
fn branch_readers_round_trip_an_external_ref_suffix_containing_a_slash() {
    assert_rendered_branch_owner("feature/{{task_id}}-{{hash}}-{{ext.jira}}", "team/ISSUE-1");
}

#[test]
fn branch_readers_round_trip_an_external_ref_containing_a_slash_before_the_task() {
    assert_rendered_branch_owner("feature/{{ext.jira}}-{{task_id}}-{{hash}}", "team/ISSUE-1");
}
