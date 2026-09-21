//! The schema-to-flag mapping is a documented contract (§4.6), so these
//! pin the shape a manifest author reads about: which spelling each JSON
//! Schema type gets, and what the parsed flags become as tool input.

use clap::Command;
use serde_json::json;

use super::super::schema::{FlagKind, clap_arg, derive_args, input_from_matches};

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "What to look for." },
            "max_depth": { "type": "integer" },
            "ratio": { "type": "number" },
            "loud": { "type": "boolean" },
            "mode": { "type": "string", "enum": ["fast", "thorough"] },
            "tags": { "type": "array", "items": { "type": "string" } },
            "filters": { "type": "object", "properties": { "since": { "type": "string" } } },
            "input": { "type": "string" }
        },
        "required": ["query"]
    })
}

#[test]
fn every_top_level_type_maps_to_its_documented_spelling() {
    let args = derive_args(&schema(), &[]);
    let spelling = |property: &str| {
        args.iter()
            .find(|arg| arg.property == property)
            .map(|arg| (arg.long.clone(), arg.kind))
    };

    assert_eq!(
        spelling("query"),
        Some(("query".to_string(), FlagKind::Str))
    );
    assert_eq!(
        spelling("max_depth"),
        Some(("max-depth".to_string(), FlagKind::Integer))
    );
    assert_eq!(
        spelling("ratio"),
        Some(("ratio".to_string(), FlagKind::Number))
    );
    assert_eq!(spelling("loud"), Some(("loud".to_string(), FlagKind::Bool)));
    assert_eq!(spelling("mode"), Some(("mode".to_string(), FlagKind::Str)));
    assert_eq!(
        spelling("tags"),
        Some(("tags".to_string(), FlagKind::StrList))
    );
    // Nested objects are `--<name>-json`, never a flattened flag set.
    assert_eq!(
        spelling("filters"),
        Some(("filters-json".to_string(), FlagKind::Json))
    );
    // A property named like one of the CLI's own flags keeps its place in
    // the schema and stays reachable through `--input`.
    assert_eq!(spelling("input"), None);
}

#[test]
fn cli_positional_promotes_named_properties_in_order() {
    let args = derive_args(&schema(), &["query".to_string(), "mode".to_string()]);
    let promoted: Vec<&str> = args
        .iter()
        .filter(|arg| arg.positional)
        .map(|arg| arg.property.as_str())
        .collect();
    assert_eq!(promoted, ["query", "mode"]);
    assert_eq!(
        args[0].property, "query",
        "positional order follows cli.positional"
    );
    assert_eq!(args[1].property, "mode");
}

#[test]
fn parsed_flags_become_the_tool_input() {
    let args = derive_args(&schema(), &["query".to_string()]);
    let mut command = Command::new("recommend").no_binary_name(true);
    for arg in &args {
        command = command.arg(clap_arg(arg));
    }
    let matches = command
        .try_get_matches_from([
            "leakage",
            "--max-depth",
            "3",
            "--ratio",
            "0.5",
            "--loud",
            "--mode",
            "thorough",
            "--tags",
            "one",
            "--tags",
            "two",
            "--filters-json",
            r#"{"since":"2026-01-01"}"#,
        ])
        .expect("derived flags parse");

    assert_eq!(
        input_from_matches(&args, &matches).expect("assemble tool input"),
        json!({
            "query": "leakage",
            "max_depth": 3,
            "ratio": 0.5,
            "loud": true,
            "mode": "thorough",
            "tags": ["one", "two"],
            "filters": { "since": "2026-01-01" }
        })
    );
}

#[test]
fn an_unset_flag_contributes_no_key() {
    let args = derive_args(&schema(), &[]);
    let mut command = Command::new("recommend").no_binary_name(true);
    for arg in &args {
        command = command.arg(clap_arg(arg));
    }
    let matches = command
        .try_get_matches_from(["--query", "only"])
        .expect("a single flag parses");

    assert_eq!(
        input_from_matches(&args, &matches).expect("assemble tool input"),
        json!({ "query": "only" }),
        "an absent boolean must not be sent as false; the tool's own schema owns its defaults"
    );
}

#[test]
fn an_enum_property_refuses_a_value_outside_its_schema() {
    let args = derive_args(&schema(), &[]);
    let mut command = Command::new("recommend").no_binary_name(true);
    for arg in &args {
        command = command.arg(clap_arg(arg));
    }
    let error = command
        .try_get_matches_from(["--mode", "sideways"])
        .expect_err("an unknown enum value is refused");
    assert!(
        error.to_string().contains("thorough"),
        "the refusal must list the schema's values: {error}"
    );
}

#[test]
fn a_malformed_json_flag_names_the_flag() {
    let args = derive_args(&schema(), &[]);
    let mut command = Command::new("recommend").no_binary_name(true);
    for arg in &args {
        command = command.arg(clap_arg(arg));
    }
    let matches = command
        .try_get_matches_from(["--filters-json", "{not json"])
        .expect("clap accepts any string");
    let error = input_from_matches(&args, &matches).expect_err("invalid JSON is refused");
    assert!(
        error.to_string().contains("--filters-json"),
        "the diagnostic must name the flag: {error}"
    );
}
