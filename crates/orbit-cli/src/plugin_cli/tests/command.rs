//! `orbit <ns> <verb>` is one spelling of `orbit tool run <ns>.<verb>`
//! (§4.6). These drive the augmented clap tree the binary parses against.

use std::path::PathBuf;

use clap::{Command, CommandFactory};
use orbit_core::adapter::command::{PluginCliGroup, PluginCliVerb};
use serde_json::{Value, json};

use super::super::{PLUGIN_HELP_HEADING, augment, help_section, invocation_from_matches};
use crate::command::Cli;
use crate::output::sink::FormatArg;
use crate::{install_format_arg, requested_format};

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
            PluginCliVerb {
                verb: "collisions".to_string(),
                tool_name: "demo.collisions".to_string(),
                description: "Exercise host-owned argument names.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": { "type": "object" },
                        "input-file": { "type": "object" },
                        "dry-run": { "type": "object" },
                        "help": { "type": "object" },
                        "format": { "type": "object" },
                        "root": { "type": "object" },
                        "workspace": { "type": "object" },
                        "version": { "type": "object" }
                    }
                }),
                positional: Vec::new(),
                mutating: false,
            },
        ],
    }
}

fn parser(groups: &[PluginCliGroup]) -> Command {
    install_format_arg(augment(Cli::command(), groups))
}

fn parse(argv: &[&str]) -> Option<super::super::PluginGroupInvocation> {
    let groups = vec![group()];
    let matches = parser(&groups)
        .try_get_matches_from(argv)
        .expect("the augmented tree parses this invocation");
    invocation_from_matches(&groups, &matches)
}

#[test]
fn object_properties_named_after_host_arguments_parse_in_the_complete_command() {
    let groups = vec![group()];
    let matches = parser(&groups)
        .try_get_matches_from([
            "orbit",
            "--root",
            "/var/lib/orbit-test",
            "--workspace",
            "fixture-workspace",
            "demo",
            "collisions",
            "--input-json",
            r#"{"source":"derived"}"#,
            "--input-file-json",
            r#"{"path":"derived"}"#,
            "--dry-run-json",
            r#"{"mode":"derived"}"#,
            "--help-json",
            r#"{"topic":"derived"}"#,
            "--format-json",
            r#"{"style":"derived"}"#,
            "--root-json",
            r#"{"path":"derived"}"#,
            "--workspace-json",
            r#"{"name":"derived"}"#,
            "--version-json",
            r#"{"value":"derived"}"#,
            "--dry-run",
            "--format",
            "json",
        ])
        .expect("host and derived arguments with related names parse together");

    assert_eq!(
        matches.get_one::<PathBuf>("root"),
        Some(&PathBuf::from("/var/lib/orbit-test"))
    );
    assert_eq!(
        matches.get_one::<String>("workspace").map(String::as_str),
        Some("fixture-workspace")
    );
    assert_eq!(requested_format(&matches), Some(FormatArg::Json));

    let invocation = invocation_from_matches(&groups, &matches).expect("a plugin invocation");
    assert!(invocation.tool_run.dry_run);
    assert_eq!(
        input(&invocation),
        json!({
            "input": { "source": "derived" },
            "input-file": { "path": "derived" },
            "dry-run": { "mode": "derived" },
            "help": { "topic": "derived" },
            "format": { "style": "derived" },
            "root": { "path": "derived" },
            "workspace": { "name": "derived" },
            "version": { "value": "derived" }
        })
    );
}

#[test]
fn raw_input_still_expresses_every_property_when_a_derived_name_collides() {
    let invocation = parse(&[
        "orbit",
        "demo",
        "collisions",
        "--input-json",
        r#"{"ignored":true}"#,
        "--input",
        r#"{"input":{"raw":true},"input-file":{"raw":true},"dry-run":{"raw":true},"help":{"raw":true},"format":{"raw":true},"root":{"raw":true},"workspace":{"raw":true},"version":{"raw":true}}"#,
    ])
    .expect("a plugin group invocation");

    assert_eq!(
        input(&invocation),
        json!({
            "input": { "raw": true },
            "input-file": { "raw": true },
            "dry-run": { "raw": true },
            "help": { "raw": true },
            "format": { "raw": true },
            "root": { "raw": true },
            "workspace": { "raw": true },
            "version": { "raw": true }
        })
    );
}

#[test]
fn host_input_file_still_wins_beside_a_colliding_derived_property() {
    let invocation = parse(&[
        "orbit",
        "demo",
        "collisions",
        "--input-file-json",
        r#"{"ignored":true}"#,
        "--input-file",
        "/tmp/plugin-input.json",
    ])
    .expect("a plugin group invocation");

    assert_eq!(
        invocation.tool_run.input_file.as_deref(),
        Some("/tmp/plugin-input.json")
    );
    assert!(invocation.tool_run.input.is_none());
}

#[test]
fn host_help_is_available_beside_a_help_property() {
    let groups = vec![group()];
    let error = parser(&groups)
        .try_get_matches_from(["orbit", "demo", "collisions", "--help"])
        .expect_err("host help exits after rendering");

    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    let help = error.to_string();
    assert!(help.contains("--help-json <JSON>"), "{help}");
    assert!(help.contains("--input <JSON>"), "{help}");
    assert!(help.contains("--input-file <PATH>"), "{help}");
    assert!(help.contains("--dry-run"), "{help}");
    assert!(help.contains("--help"), "{help}");
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
fn group_help_lists_manifest_descriptions() {
    let groups = vec![group()];
    let help = parser(&groups)
        .find_subcommand_mut("demo")
        .expect("the plugin group is a subcommand")
        .render_long_help()
        .to_string();

    assert!(help.contains("Recommend files for a task."), "{help}");
    assert!(help.contains("Rebuild the index."), "{help}");
    assert!(
        help.contains("Exercise host-owned argument names."),
        "{help}"
    );
    assert!(help.contains("demo"), "{help}");
}

#[test]
fn an_unknown_word_is_still_an_unknown_command() {
    let groups = vec![group()];
    let error = parser(&groups)
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
    let error = parser(&[])
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
