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
fn malformed_derived_flags_are_omitted_without_breaking_valid_flags() {
    let schema = json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "string" },
            "taskId": { "type": "string" },
            "###": { "type": "object" },
            "query": { "type": "string" }
        }
    });
    let args = derive_args(&schema, &[]);
    assert_eq!(
        args.iter()
            .map(|arg| arg.property.as_str())
            .collect::<Vec<_>>(),
        ["query"],
        "ambiguous and empty shortcuts degrade to --input while valid flags remain"
    );

    let mut plugin_verb = Command::new("recommend");
    for arg in &args {
        plugin_verb = plugin_verb.arg(clap_arg(arg));
    }
    Command::new("orbit")
        .subcommand(Command::new("task").subcommand(Command::new("list")))
        .subcommand(Command::new("demo").subcommand(plugin_verb))
        .try_get_matches_from(["orbit", "task", "list"])
        .expect("a malformed plugin schema cannot make built-in commands fail clap validation");
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
fn a_promoted_property_keeps_its_ordinary_flag_too() {
    // The scaffold template promises a promoted property "is still available
    // as `--<name>`"; a manifest author who reads that must get both forms.
    let args = derive_args(&schema(), &["query".to_string()]);
    let flag_entries: Vec<&super::super::schema::DerivedArg> = args
        .iter()
        .filter(|arg| arg.property == "query" && !arg.positional)
        .collect();
    assert_eq!(
        flag_entries.len(),
        1,
        "query keeps exactly one non-positional --query entry: {args:?}"
    );
    assert_eq!(flag_entries[0].long, "query");

    let mut command = Command::new("recommend").no_binary_name(true);
    for arg in &args {
        command = command.arg(clap_arg(arg));
    }
    let matches = command
        .try_get_matches_from(["--query", "leakage"])
        .expect("the promoted property still parses through its --query flag");
    assert_eq!(
        input_from_matches(&args, &matches).expect("assemble tool input"),
        json!({ "query": "leakage" })
    );
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
fn a_boolean_flag_does_not_swallow_the_positional_that_follows_it() {
    // Regression: `orbit shapes recommend --loud leakage` used to fail with
    // "invalid value 'leakage' for '--loud [<BOOL>]'" because the optional
    // value consumed the next bare token, positional or not.
    let args = derive_args(&schema(), &["query".to_string()]);
    let mut command = Command::new("recommend").no_binary_name(true);
    for arg in &args {
        command = command.arg(clap_arg(arg));
    }
    let matches = command
        .try_get_matches_from(["--loud", "leakage"])
        .expect("a boolean flag with no explicit value leaves the next positional alone");

    assert_eq!(
        input_from_matches(&args, &matches).expect("assemble tool input"),
        json!({ "query": "leakage", "loud": true })
    );
}

#[test]
fn a_boolean_flag_still_accepts_an_explicit_value_with_equals() {
    let args = derive_args(&schema(), &[]);
    let mut command = Command::new("recommend").no_binary_name(true);
    for arg in &args {
        command = command.arg(clap_arg(arg));
    }
    let matches = command
        .try_get_matches_from(["--query", "only", "--loud=false"])
        .expect("an explicit boolean value using = still parses");

    assert_eq!(
        input_from_matches(&args, &matches).expect("assemble tool input"),
        json!({ "query": "only", "loud": false })
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

fn numeric_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "ratio": { "type": "number" },
            "weights": { "type": "array", "items": { "type": "number" } },
            "threshold": { "type": "number" }
        }
    })
}

fn numeric_command(args: &[super::super::schema::DerivedArg]) -> Command {
    let mut command = Command::new("score").no_binary_name(true);
    for arg in args {
        command = command.arg(clap_arg(arg));
    }
    command
}

#[test]
fn non_finite_numbers_are_refused_for_every_numeric_surface() {
    // Regression: `f64` parsing accepts these, and input assembly used to
    // drop them, so `--ratio NaN` sent `{}` and `--weights 1 --weights NaN
    // --weights 2` sent `[1, 2]` — the backend ran on input the caller never
    // gave.
    let args = derive_args(&numeric_schema(), &["threshold".to_string()]);
    for raw in ["NaN", "nan", "inf", "infinity", "1e999", "-1e999", "-inf"] {
        let cases: [(Vec<String>, &str); 4] = [
            (vec![format!("--ratio={raw}")], "--ratio"),
            (vec![format!("--threshold={raw}")], "--threshold"),
            (vec![raw.to_string()], "THRESHOLD"),
            (
                vec![
                    "--weights".to_string(),
                    "1".to_string(),
                    format!("--weights={raw}"),
                    "--weights".to_string(),
                    "2".to_string(),
                ],
                "--weights",
            ),
        ];
        for (argv, named) in cases {
            if raw.starts_with('-') && !argv[0].starts_with("--") {
                // A bare leading-dash token is a flag, not a positional value.
                continue;
            }
            let error = numeric_command(&args)
                .try_get_matches_from(&argv)
                .expect_err("a non-finite number is refused before any input is assembled");
            assert!(
                error.to_string().contains(named),
                "the refusal must name the argument {named} for {argv:?}: {error}"
            );
        }
    }
}

#[test]
fn finite_repeated_numbers_keep_their_order_and_count() {
    let args = derive_args(&numeric_schema(), &["threshold".to_string()]);
    let matches = numeric_command(&args)
        .try_get_matches_from([
            "0.25",
            "--ratio",
            "1e308",
            "--weights",
            "1.5",
            "--weights=-2",
            "--weights",
            "1.5",
            "--weights",
            "0",
            "--weights",
            "2e3",
        ])
        .expect("finite numbers parse");

    assert_eq!(
        input_from_matches(&args, &matches).expect("assemble tool input"),
        json!({
            "threshold": 0.25,
            "ratio": 1e308,
            "weights": [1.5, -2.0, 1.5, 0.0, 2000.0]
        }),
        "every supplied element arrives, duplicates included, in the order given"
    );
}
