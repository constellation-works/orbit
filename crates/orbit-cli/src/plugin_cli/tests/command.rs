//! `orbit <ns> <verb>` is one spelling of `orbit tool run <ns>.<verb>`
//! (§4.6). These drive the augmented clap tree the binary parses against.

use clap::CommandFactory;
use orbit_core::adapter::command::{PluginCliGroup, PluginCliVerb};
use serde_json::{Value, json};

use super::super::{PLUGIN_HELP_HEADING, augment, help_section, invocation_from_matches};
use crate::command::Cli;

fn group() -> PluginCliGroup {
    PluginCliGroup {
        namespace: "demo".to_string(),
        version: "1.2.3".to_string(),
        description: "Fixture plugin".to_string(),
        verbs: vec![
            PluginCliVerb {
                verb: "recommend".to_string(),
                tool_name: "demo.recommend".to_string(),
                description: "Recommend files for a task.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "max_depth": { "type": "integer" },
                        "loud": { "type": "boolean" },
                        "mode": { "type": "string", "enum": ["fast", "thorough"] },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "filters": { "type": "object" }
                    }
                }),
                positional: vec!["query".to_string()],
                mutating: false,
            },
            PluginCliVerb {
                verb: "maintain".to_string(),
                tool_name: "demo.maintain".to_string(),
                description: "Rebuild the index.".to_string(),
                input_schema: json!({ "type": "object", "properties": {} }),
                positional: Vec::new(),
                mutating: true,
            },
        ],
    }
}

fn parse(argv: &[&str]) -> Option<super::super::PluginGroupInvocation> {
    let groups = vec![group()];
    let matches = augment(Cli::command(), &groups)
        .try_get_matches_from(argv)
        .expect("the augmented tree parses this invocation");
    invocation_from_matches(&groups, &matches)
}

fn input(invocation: &super::super::PluginGroupInvocation) -> Value {
    serde_json::from_str(
        invocation
            .tool_run
            .input
            .as_deref()
            .expect("the invocation carries tool input"),
    )
    .expect("the assembled input is JSON")
}

#[test]
fn flags_and_positionals_become_the_same_tool_call_as_tool_run() {
    let invocation = parse(&[
        "orbit",
        "demo",
        "recommend",
        "leakage",
        "--max-depth",
        "2",
        "--loud",
        "--mode",
        "fast",
        "--tags",
        "rust",
        "--filters-json",
        r#"{"since":"2026-01-01"}"#,
    ])
    .expect("a plugin group invocation");

    assert_eq!(invocation.namespace, "demo");
    assert_eq!(invocation.verb, "recommend");
    assert_eq!(invocation.tool_run.name, "demo.recommend");
    assert!(invocation.input_error.is_none());
    assert_eq!(
        input(&invocation),
        json!({
            "query": "leakage",
            "max_depth": 2,
            "loud": true,
            "mode": "fast",
            "tags": ["rust"],
            "filters": { "since": "2026-01-01" }
        })
    );
}

#[test]
fn input_overrides_every_derived_flag() {
    let invocation = parse(&[
        "orbit",
        "demo",
        "recommend",
        "ignored",
        "--max-depth",
        "9",
        "--input",
        r#"{"query":"explicit"}"#,
    ])
    .expect("a plugin group invocation");

    assert_eq!(
        input(&invocation),
        json!({ "query": "explicit" }),
        "`--input` is the payload, not a payload merged with the flags beside it"
    );
}

#[test]
fn input_file_is_passed_through_untouched() {
    let invocation = parse(&[
        "orbit",
        "demo",
        "recommend",
        "--input-file",
        "/tmp/does-not-need-to-exist.json",
    ])
    .expect("a plugin group invocation");

    assert_eq!(
        invocation.tool_run.input_file.as_deref(),
        Some("/tmp/does-not-need-to-exist.json")
    );
    assert!(
        invocation.tool_run.input.is_none(),
        "the file is read by the same loader `orbit tool run --input-file` uses"
    );
}

#[test]
fn dry_run_reaches_the_tool_run_arguments() {
    let invocation =
        parse(&["orbit", "demo", "maintain", "--dry-run"]).expect("a plugin group invocation");
    assert!(invocation.tool_run.dry_run);
    assert_eq!(invocation.tool_run.name, "demo.maintain");
}

#[test]
fn a_malformed_json_flag_is_reported_as_tool_input() {
    let invocation = parse(&["orbit", "demo", "recommend", "--filters-json", "{oops"])
        .expect("a plugin group invocation");
    let message = invocation
        .input_error
        .expect("a malformed --<name>-json is carried as an input error");
    assert!(message.contains("--filters-json"), "{message}");
}

#[test]
fn a_verb_lists_with_its_manifest_description_and_the_group_has_no_other_verbs() {
    let groups = vec![group()];
    let help = augment(Cli::command(), &groups)
        .find_subcommand_mut("demo")
        .expect("the plugin group is a subcommand")
        .render_long_help()
        .to_string();

    assert!(help.contains("Recommend files for a task."), "{help}");
    assert!(help.contains("Rebuild the index."), "{help}");
    assert!(help.contains("demo"), "{help}");
}

#[test]
fn an_unknown_word_is_still_an_unknown_command() {
    let groups = vec![group()];
    let error = augment(Cli::command(), &groups)
        .try_get_matches_from(["orbit", "not-a-plugin", "recommend"])
        .expect_err("there is no passthrough for an unknown top-level word");
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::InvalidSubcommand,
        "{error}"
    );
}

#[test]
fn a_group_absent_from_the_host_is_absent_from_the_tree() {
    // The disabled-plugin case: `host_plugin_cli_groups` reports only active
    // plugins, so the CLI sees no group at all and clap answers as it would
    // for any other unknown word.
    let error = augment(Cli::command(), &[])
        .try_get_matches_from(["orbit", "demo", "recommend"])
        .expect_err("a plugin with no group is not a command");
    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
}

#[test]
fn the_root_help_section_lists_each_group() {
    let section = help_section(&[group()]);
    assert!(
        section.starts_with(&format!("\n{PLUGIN_HELP_HEADING}\n")),
        "{section}"
    );
    assert!(section.contains("demo"), "{section}");
    assert!(section.contains("Fixture plugin"), "{section}");
    assert!(help_section(&[]).is_empty(), "no plugins, no section");
}
