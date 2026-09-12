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

#[test]
fn complexity_pools_set_get_show_agree_and_invalid_edits_do_not_write() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let path = workspace_root.join("config.toml");
    fs::write(&path, "[workflow]\ndefault_crew = \"opus\"\n").expect("config");
    for complexity in ["low", "medium", "hard"] {
        let key = format!("workflow.{complexity}_complexity_crews");
        set_args(&key, r#"["terra", "grok", "terra"]"#)
            .execute(&runtime)
            .expect("set pool");
        let value = json_value(get_args(&key, true).execute(&runtime).expect("get pool"));
        assert_eq!(value["value"], serde_json::json!(["grok", "terra"]));
        let effective = orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
            &global_root,
            &workspace_root,
        ))
        .expect("effective");
        let shown = super::super::show::effective_json(&runtime, effective.values());
        assert_eq!(shown["settings"][&key], value["value"]);
        assert_eq!(shown["provenance"][&key]["scope"], "workspace");
        let before = fs::read(&path).expect("before");
        for invalid in [r#"["unknown-pool-crew"]"#, r#"[""]"#] {
            let error = set_args(&key, invalid)
                .execute(&runtime)
                .expect_err("invalid pool");
            assert!(error.to_string().contains(&key), "{error}");
            assert_eq!(fs::read(&path).expect("after"), before);
        }
        set_args(&key, "[]")
            .execute(&runtime)
            .expect("disable pool");
        assert_eq!(
            json_value(get_args(&key, true).execute(&runtime).expect("empty pool"))["value"],
            serde_json::json!([])
        );
    }
}

#[test]
fn get_without_json_flag_still_returns_a_payload() {
    let (_root, runtime, _global_root, _workspace_root) = test_runtime();
    let output = get_args("workflow.base_branch", false)
        .execute(&runtime)
        .expect("effective get");
    let document = json_value(output);
    assert_eq!(document["key"], "workflow.base_branch");
    assert_eq!(document["scope"], "effective");
    assert!(document.get("value").is_some(), "{document}");
}

#[test]
fn get_scoped_workspace_without_config_file_returns_explicit_absent() {
    let (_root, runtime, global_root, _workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        "[scoring]\nenabled = true\n\n[workflow]\nbase_branch = \"custom-global\"\n",
    )
    .expect("write global config");

    let mut args = get_args("scoring.enabled", true);
    args.scope = ConfigScopeArg::Workspace;
    let output = args.execute(&runtime).expect("get workspace scoring");
    let document = json_value(output);

    assert_eq!(document["key"], "scoring.enabled");
    assert_eq!(document["scope"], "workspace");
    assert_eq!(document["value"], serde_json::Value::Null);
    assert_eq!(document["exists"], false);
    assert!(
        document["path"].is_null(),
        "path must be null when layer file is absent: {document}"
    );

    let mut text_args = get_args("scoring.enabled", false);
    text_args.scope = ConfigScopeArg::Workspace;
    let text_output = text_args.execute(&runtime).expect("get workspace text");
    let CommandOutput::Payload(payload) = text_output else {
        panic!("expected payload");
    };
    let (_, view) = payload.into_view();
    let crate::output::payload::View::Blocks(blocks) = view else {
        panic!("expected blocks");
    };
    let crate::output::payload::Block::Text(text) = &blocks[0] else {
        panic!("expected text block");
    };
    assert_eq!(text, "null");
}

#[test]
fn get_scoped_workspace_with_file_distinguishes_set_and_unset_keys() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        "[scoring]\nenabled = true\n\n[workflow]\nbase_branch = \"global-main\"\n",
    )
    .expect("write global config");
    let ws_config = workspace_root.join("config.toml");
    fs::write(&ws_config, "[scoring]\nenabled = false\n").expect("write workspace config");

    let mut set_args = get_args("scoring.enabled", true);
    set_args.scope = ConfigScopeArg::Workspace;
    let set_doc = json_value(set_args.execute(&runtime).expect("get set key"));
    assert_eq!(set_doc["key"], "scoring.enabled");
    assert_eq!(set_doc["scope"], "workspace");
    assert_eq!(set_doc["value"], false);
    assert_eq!(set_doc["exists"], true);
    assert_eq!(set_doc["path"], ws_config.to_string_lossy().as_ref());

    let mut unset_args = get_args("workflow.base_branch", true);
    unset_args.scope = ConfigScopeArg::Workspace;
    let unset_doc = json_value(unset_args.execute(&runtime).expect("get unset key"));
    assert_eq!(unset_doc["key"], "workflow.base_branch");
    assert_eq!(unset_doc["scope"], "workspace");
    assert_eq!(unset_doc["value"], serde_json::Value::Null);
    assert_eq!(unset_doc["exists"], false);
    assert_eq!(unset_doc["path"], ws_config.to_string_lossy().as_ref());
}
