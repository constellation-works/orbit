//! Readers of a delivery branch find its task under the workspace's
//! `branch.name_template`, and never a task whose id only prefixes it.

use orbit_types::task::names_task_delivery_pull_request;
use orbit_types::workflow::{ConventionTemplate, DEFAULT_BRANCH_PREFIX, default_branch_template};

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
