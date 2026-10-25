//! Workspace delivery conventions through the public `pr_open` action: a body
//! template read at the base revision, agent-filled sections from the task
//! artifact, and the required-section gap.

use super::*;
use orbit_types::workflow::{
    DeliveryConventions, MissingSectionPolicy, PR_SECTION_GAP_EVENT, PR_SECTIONS_ARTIFACT,
    PrBodyTemplateSource,
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
