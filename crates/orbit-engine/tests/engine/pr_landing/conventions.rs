//! Workspace delivery conventions through the public `pr_open` action: a body
//! template read at the base revision, agent-filled sections from the task
//! artifact, and the required-section gap.

use super::*;
use orbit_types::task::names_task_delivery_pull_request;
use orbit_types::workflow::{
    DeliveryConventions, MissingSectionPolicy, PR_SECTION_GAP_EVENT, PR_SECTIONS_ARTIFACT,
    PrBodyTemplateSource, default_branch_template,
};

const TEMPLATE_PATH: &str = ".github/PULL_REQUEST_TEMPLATE.md";
const TEMPLATE: &str = "## Summary\n\n{{title}} ({{task_id}})\n\n## Risk\n\n<!-- describe the risk -->\n\n## Rollback\n\n<!-- how to roll back -->\n";

/// A candidate rebased onto a base carrying the repository template, whose
/// own commit rewrites that template: only the base revision may publish.
fn templated(sandbox: &Path, policy: MissingSectionPolicy) -> (Fixture, DeliveryHost) {
    let mut fx = Fixture::new(sandbox);
    fx.commit_on_base(TEMPLATE_PATH, TEMPLATE);
    fx.rebase_candidate();
    fx.candidate = fx.commit(TEMPLATE_PATH, "## Candidate rewrite {{title}}\n");
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    host.set_conventions(DeliveryConventions {
        pr_body: Some(PrBodyTemplateSource::Repository),
        agent_sections: vec!["Risk".into()],
        required_sections: vec!["Rollback".into()],
        missing_section: policy,
        labels: vec!["corp".into()],
        reviewers: vec!["octo".into()],
        ..DeliveryConventions::default()
    });
    host.put_artifact(
        TASK_ID,
        PR_SECTIONS_ARTIFACT,
        r#"{"sections": {"risk": "Low: one fixture file."}}"#,
    );
    (fx, host)
}

fn gap_notes(host: &DeliveryHost) -> Vec<String> {
    host.history(TASK_ID)
        .into_iter()
        .filter(|entry| entry.event == PR_SECTION_GAP_EVENT)
        .filter_map(|entry| entry.note)
        .collect()
}

#[test]
fn pr_open_renders_the_base_template_and_drafts_an_unfilled_required_section() {
    isolated(
        "conventions::pr_open_renders_the_base_template_and_drafts_an_unfilled_required_section",
        |sandbox| {
            let (fx, host) = templated(sandbox, MissingSectionPolicy::Draft);
            let opened = open_ok(&fx, &host);
            assert_eq!(opened["pr_created"], true);
            assert_eq!(opened["draft"], true);
            assert_eq!(opened["missing_required_sections"], json!(["Rollback"]));
            let body = fx.forge_state("pr-body").unwrap();
            assert!(
                body.starts_with(&format!(
                    "## Summary\n\nLand the reviewed candidate ({TASK_ID})\n\n## Risk\n\nLow: one fixture file.\n\n## Rollback\n\n> **Missing required section.**"
                )),
                "{body}"
            );
            assert!(!body.contains("describe the risk"), "{body}");
            assert!(!body.contains("Candidate rewrite"), "{body}");
            assert!(!body.contains("{{"), "{body}");
            assert!(fx.forge_state("pr-draft").is_some());
            assert_eq!(fx.forge_lines("pr-labels"), ["corp"]);
            assert_eq!(fx.forge_lines("pr-reviewers"), ["octo"]);
            let notes = gap_notes(&host);
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(
                notes[0].contains("`Rollback`") && notes[0].contains("draft"),
                "{notes:?}"
            );

            // Without conventions the same candidate opens ready, unlabelled.
            let (fx, host) = fx_host(sandbox);
            let opened = open_ok(&fx, &host);
            assert_eq!(opened["draft"], false);
            assert!(fx.forge_state("pr-draft").is_none());
            assert!(fx.forge_state("pr-labels").is_none());
            assert!(gap_notes(&host).is_empty());
        },
    );
}

#[test]
fn pr_open_refuses_an_unfilled_required_section_or_an_unknown_template_placeholder() {
    isolated(
        "conventions::pr_open_refuses_an_unfilled_required_section_or_an_unknown_template_placeholder",
        |sandbox| {
            let (fx, host) = templated(sandbox, MissingSectionPolicy::Refuse);
            let error = action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("pr_required_section_missing") && error.contains("`Rollback`"),
                "{error}"
            );
            assert!(!fx.forge_calls().iter().any(|call| call == "pr create"));
            let notes = gap_notes(&host);
            assert!(
                notes.iter().any(|note| note.contains("refuses")),
                "{notes:?}"
            );

            let mut fx = Fixture::new(sandbox);
            fx.commit_on_base(TEMPLATE_PATH, "## Summary\n\n{{ticket}}\n");
            fx.rebase_candidate();
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.set_conventions(DeliveryConventions {
                pr_body: Some(PrBodyTemplateSource::Repository),
                ..DeliveryConventions::default()
            });
            let error = action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("pr.body_template") && error.contains("{{ticket}}"),
                "{error}"
            );
            assert!(!fx.forge_calls().iter().any(|call| call == "pr create"));
        },
    );
}

/// A repository template with no task placeholder still publishes the task
/// id, and that body is what supersession recognizes. Author and branch
/// checks stay in force, and a body with no whole-token id is left open.
#[test]
fn pr_open_publishes_task_identity_when_the_repository_template_omits_it() {
    isolated(
        "conventions::pr_open_publishes_task_identity_when_the_repository_template_omits_it",
        |sandbox| {
            let mut fx = Fixture::new(sandbox);
            fx.commit_on_base(TEMPLATE_PATH, "## Summary\n\n{{title}}\n");
            fx.rebase_candidate();
            fx.candidate = fx.commit(TEMPLATE_PATH, "## Candidate rewrite {{task_id}}\n");
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.set_conventions(DeliveryConventions {
                pr_body: Some(PrBodyTemplateSource::Repository),
                signature: false,
                pr_orchestration_trailer: false,
                ..DeliveryConventions::default()
            });
            host.set_delivery_identity(&["orbit"], true);

            let identity = format!("<!-- orbit-delivery-task: {TASK_ID} -->");
            let rendered = format!("## Summary\n\nLand the reviewed candidate\n\n{identity}");
            let bare = "## Summary\n\nLand the reviewed candidate";
            let lookalike = format!(
                "## Summary\n\nLand the reviewed candidate\n\n<!-- orbit-delivery-task: {TASK_ID}0 -->"
            );
            let delivery_branch = format!("orbit/{TASK_ID}-0123abcd");
            let listed = json!([
                {
                    "number": 7,
                    "title": "Earlier",
                    "headRefName": delivery_branch,
                    "author": {"login": "orbit"},
                    "body": rendered,
                },
                {
                    "number": 8,
                    "title": "Human",
                    "headRefName": format!("orbit/{TASK_ID}-aaaaaaaa"),
                    "author": {"login": "alice"},
                    "body": rendered,
                },
                {
                    "number": 9,
                    "title": "Other branch",
                    "headRefName": format!("orbit/{TASK_ID}0-bbbbbbbb"),
                    "author": {"login": "orbit"},
                    "body": rendered,
                },
                {
                    "number": 10,
                    "title": "No identity",
                    "headRefName": format!("orbit/{TASK_ID}-cccccccc"),
                    "author": {"login": "orbit"},
                    "body": bare,
                },
                {
                    "number": 11,
                    "title": "Lookalike",
                    "headRefName": format!("orbit/{TASK_ID}-dddddddd"),
                    "author": {"login": "orbit"},
                    "body": lookalike,
                },
            ]);
            std::fs::write(fx.forge.join("open-prs.json"), listed.to_string()).unwrap();

            let opened = open_ok(&fx, &host);
            assert_eq!(opened["pr_created"], true, "{opened}");
            let body = fx.forge_state("pr-body").unwrap();
            assert_eq!(body, rendered);
            assert!(!body.contains("Candidate rewrite"), "{body}");
            assert!(!body.contains("{{"), "{body}");
            let branch = default_branch_template();
            assert!(names_task_delivery_pull_request(
                &branch,
                &delivery_branch,
                &body,
                TASK_ID
            ));
            assert!(
                !names_task_delivery_pull_request(&branch, BRANCH, &body, TASK_ID),
                "a branch the template does not name is not this delivery"
            );
            assert!(
                !names_task_delivery_pull_request(&branch, &delivery_branch, bare, TASK_ID),
                "template text alone does not name the task"
            );

            let closed = opened["closed_stale_pull_requests"].as_array().unwrap();
            assert_eq!(closed.len(), 1, "{opened}");
            assert_eq!(closed[0]["number"], "7", "{opened}");
            assert_eq!(closed[0]["task_id"], TASK_ID, "{opened}");
            assert_eq!(closed[0]["decision"], "closed", "{opened}");
            assert_eq!(
                fx.forge_calls()
                    .iter()
                    .filter(|call| *call == "pr close")
                    .count(),
                1,
                "only the earlier delivery pull request is closed: {:?}",
                fx.forge_calls()
            );
        },
    );
}
