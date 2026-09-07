use std::fs;

use crate::command::{CommandOutput, Execute};

use super::super::get::ConfigGetArgs;
use super::super::set::ConfigSetArgs;
use super::super::support::ConfigScopeArg;
use super::test_runtime;

fn get_args(key: &str, json: bool) -> ConfigGetArgs {
    ConfigGetArgs {
        key: key.to_string(),
        scope: ConfigScopeArg::Effective,
        json,
    }
}

fn set_args(key: &str, value: &str) -> ConfigSetArgs {
    ConfigSetArgs {
        key: key.to_string(),
        value: value.to_string(),
        global: false,
        seed_from_global: false,
        fresh: false,
    }
}

fn write_sol_crew(path: &std::path::Path) {
    fs::write(
        path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nmodel = \"gpt-5.6-sol\"\nprovider = \"codex\"\n",
    )
    .expect("write sol crew config");
}

fn json_value(output: CommandOutput) -> serde_json::Value {
    let CommandOutput::Payload(payload) = output else {
        panic!("expected JSON payload, got {output:?}");
    };
    let (document, _) = payload.into_view();
    document
}

#[test]
fn get_rejects_misspelled_crew_field() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_sol_crew(&workspace_root.join("config.toml"));

    let error = get_args("crews.sol.effrot", true)
        .execute(&runtime)
        .expect_err("misspelled crew field must fail");
    assert!(error.to_string().contains("effrot"), "{error}");
    assert!(
        error
            .did_you_mean()
            .is_some_and(|suggestions| suggestions.iter().any(|key| key == "crews.sol.effort")),
        "{error:?}"
    );
}

#[test]
fn get_omitted_crew_effort_is_null_not_a_fabricated_default() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_sol_crew(&workspace_root.join("config.toml"));

    let document = json_value(
        get_args("crews.sol.effort", true)
            .execute(&runtime)
            .expect("omitted effort is gettable"),
    );
    assert_eq!(document["key"], "crews.sol.effort");
    assert_eq!(document["value"], serde_json::Value::Null);
}

#[test]
fn get_agrees_with_set_for_sol_crew_effort() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_sol_crew(&workspace_root.join("config.toml"));

    set_args("crews.sol.effort", "high")
        .execute(&runtime)
        .expect("set sol effort");

    let document = json_value(
        get_args("crews.sol.effort", true)
            .execute(&runtime)
            .expect("get configured effort"),
    );
    assert_eq!(document["value"], "high");
}

#[test]
fn get_reads_hand_authored_crew_effort() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    fs::write(
        workspace_root.join("config.toml"),
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nmodel = \"gpt-5.6-sol\"\nprovider = \"codex\"\neffort = \"high\"\n",
    )
    .expect("write hand-authored effort");

    let document = json_value(
        get_args("crews.sol.effort", true)
            .execute(&runtime)
            .expect("get hand-authored effort"),
    );
    assert_eq!(document["value"], "high");
}
