//! Bare task/prose mentions must never notify unrelated forge accounts.
use super::*;

const TITLE: &str = "opus@auto @auto @orbit-tools/cli dev+orbit@example.com `@kept`";
const SAFE_TITLE: &str =
    "opus@\u{200d}auto @\u{200d}auto @\u{200d}orbit-tools/cli dev+orbit@example.com `@kept`";
const PROSE: &str = "@auto @astrojs/core dev+orbit@example.com\n\n`@literal` and ``a ` @literal``\n\n`multiline\n@literal`\n\n````md\n@fenced\n```\n@fenced\n````\n\n~~~text\n@tilde\n~~~\n\nCo-authored-by: @author <author@example.com>\n\nAfter @auto";
const SAFE_PROSE: &str = "@\u{200d}auto @\u{200d}astrojs/core dev+orbit@example.com\n\n`@literal` and ``a ` @literal``\n\n`multiline\n@literal`\n\n````md\n@fenced\n```\n@fenced\n````\n\n~~~text\n@tilde\n~~~\n\nCo-authored-by: @author <author@example.com>\n\nAfter @\u{200d}auto";
// A bare URL and a Markdown link destination keep their `@` path segments and
// query values byte-for-byte; the visible `@auto` still receives the joiner.
const URL_TITLE: &str = "Link https://medium.com/@someone/post for @auto";
const SAFE_URL_TITLE: &str = "Link https://medium.com/@someone/post for @\u{200d}auto";
const URL_PROSE: &str = "[pkg](https://www.npmjs.com/package/@orbit-tools/cli) and https://medium.com/@someone/post and https://x.io/a?b=c@d, then @auto";
const SAFE_URL_PROSE: &str = "[pkg](https://www.npmjs.com/package/@orbit-tools/cli) and https://medium.com/@someone/post and https://x.io/a?b=c@d, then @\u{200d}auto";

#[test]
fn pr_open_neutralizes_forge_mentions_in_generated_and_override_text() {
    isolated(
        "mentions::pr_open_neutralizes_forge_mentions_in_generated_and_override_text",
        |sandbox| {
            for source in ["generated", "template", "override"] {
                let (fx, host) = fx_host(sandbox);
                {
                    let mut tasks = locked(&host.tasks);
                    let task = tasks.get_mut(TASK_ID).unwrap();
                    task.title = TITLE.into();
                    task.description = PROSE.into();
                }
                if source == "template" {
                    host.set_conventions(orbit_types::workflow::DeliveryConventions {
                        pr_title: Some(
                            orbit_types::workflow::ConventionTemplate::parse(
                                "pr.title_template",
                                "{{title}}",
                                orbit_types::workflow::TemplateSurface::Text,
                            )
                            .unwrap(),
                        ),
                        ..Default::default()
                    });
                }
                let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
                if source == "override" {
                    input["title"] = json!(TITLE);
                    input["body"] = json!(PROSE);
                }
                action_ok(&host, "pr_open", &input);
                assert_eq!(
                    fx.forge_state("pr-title").as_deref(),
                    Some(SAFE_TITLE),
                    "{source}"
                );
                let body = fx.forge_state("pr-body").unwrap();
                assert!(body.contains(SAFE_PROSE), "{source}: {body}");
                if source != "override" {
                    assert!(body.contains(SAFE_TITLE), "{source}: {body}");
                }
            }
        },
    );
}

#[test]
fn pr_open_neutralizes_forge_mentions_in_a_refreshed_draft_and_truncated_body() {
    isolated(
        "mentions::pr_open_neutralizes_forge_mentions_in_a_refreshed_draft_and_truncated_body",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            host.set_conventions(orbit_types::workflow::DeliveryConventions {
                agent_sections: vec!["Rollback".into()],
                required_sections: vec!["Rollback".into()],
                ..Default::default()
            });
            assert_eq!(open_ok(&fx, &host)["draft"], true);
            host.put_artifact(
                TASK_ID,
                orbit_types::workflow::PR_SECTIONS_ARTIFACT,
                &json!({"sections": {"Rollback": PROSE}}).to_string(),
            );
            let reused = open_ok(&fx, &host);
            assert_eq!(reused["pr_reused"], true);
            assert!(fx.forge_state("pr-body").unwrap().contains(SAFE_PROSE));
            assert!(fx.forge_state("pr-draft").is_none());

            // Truncation removes the closing backtick. The exposed token
            // must be neutralized while keeping GitHub's UTF-8 byte limit.
            let (fx, host) = fx_host(sandbox);
            let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
            input["body"] = json!(format!("`@auto {} `", "x".repeat(65_536)));
            action_ok(&host, "pr_open", &input);
            let body = fx.forge_state("pr-body").unwrap();
            assert!(body.starts_with("`@\u{200d}auto "), "{}", &body[..40]);
            assert!(body.len() <= 65_536, "{} bytes", body.len());
        },
    );
}

#[test]
fn pr_open_neutralizes_mentions_at_url_and_link_boundaries() {
    isolated(
        "mentions::pr_open_neutralizes_mentions_at_url_and_link_boundaries",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
            input["title"] =
                json!("[docs](https://example.com)@auto ://@auto ](@auto) https://x.io/a?b=c@d");
            input["body"] = json!(
                "[pkg](https://www.npmjs.com/package/@orbit-tools/cli)@auto and [docs](https://example.com)@auto"
            );
            action_ok(&host, "pr_open", &input);
            assert_eq!(
                fx.forge_state("pr-title").as_deref(),
                Some(
                    "[docs](https://example.com)@\u{200d}auto ://@\u{200d}auto ](@\u{200d}auto) https://x.io/a?b=c@d"
                )
            );
            let body = fx.forge_state("pr-body").unwrap();
            assert!(
                body.contains(
                    "[pkg](https://www.npmjs.com/package/@orbit-tools/cli)@\u{200d}auto and [docs](https://example.com)@\u{200d}auto"
                ),
                "{body}"
            );
        },
    );
}

#[test]
fn pr_open_leaves_at_signs_inside_urls_unchanged() {
    isolated(
        "mentions::pr_open_leaves_at_signs_inside_urls_unchanged",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
            input["title"] = json!(URL_TITLE);
            input["body"] = json!(URL_PROSE);
            action_ok(&host, "pr_open", &input);
            assert_eq!(fx.forge_state("pr-title").as_deref(), Some(SAFE_URL_TITLE));
            let body = fx.forge_state("pr-body").unwrap();
            assert!(body.contains(SAFE_URL_PROSE), "{body}");
        },
    );
}
