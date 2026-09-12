use std::fs;

use orbit_common::OrbitError;
use tempfile::tempdir;

use crate::store::*;

fn config_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("config.toml")
}

#[test]
fn get_returns_default_when_key_absent() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open empty store");

    let value = store
        .effective_value("workflow.base_branch")
        .expect("get default");
    assert_eq!(value, serde_json::json!("main"));
}

#[test]
fn get_returns_configured_value() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(&path, "[workflow]\nbase_branch = \"agent-main\"\n").expect("write config");

    let store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    let value = store
        .effective_value("workflow.base_branch")
        .expect("get value");
    assert_eq!(value, serde_json::json!("agent-main"));
}

#[test]
fn set_parses_toml_literal_types_not_just_strings() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    store
        .set_value("tasks.id_start", "10000")
        .expect("set integer literal");
    store
        .set_value("scoring.enabled", "true")
        .expect("set bool literal");
    store
        .set_value("execution.env.pass", "[\"HOME\", \"PATH\", \"CODEX_HOME\"]")
        .expect("set array literal");
    store.validate().expect("validate");
    store.save().expect("save");

    let saved = fs::read_to_string(&path).expect("read saved config");
    // Written as real TOML types (unquoted), not `"10000"` / `"true"` strings.
    assert!(saved.contains("id_start = 10000"), "{saved}");
    assert!(saved.contains("enabled = true"), "{saved}");

    let reopened = ConfigStore::open(ConfigScope::Workspace, &path).expect("reopen store");
    assert_eq!(
        reopened
            .effective_value("tasks.id_start")
            .expect("get tasks.id_start"),
        serde_json::json!(10000)
    );
    assert_eq!(
        reopened
            .effective_value("scoring.enabled")
            .expect("get scoring.enabled"),
        serde_json::json!(true)
    );
    assert_eq!(
        reopened
            .effective_value("execution.env.pass")
            .expect("get execution.env.pass"),
        serde_json::json!(["CODEX_HOME", "HOME", "PATH"])
    );
}

#[test]
fn set_descends_through_inline_tables() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(&path, "execution = { env = { pass = [\"A\"] } }\n").expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("execution.env.pass", "[\"A\",\"B\"]")
        .expect("set value through inline tables");
    store.validate().expect("validate");
    store.save().expect("save");

    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(saved.contains("A"), "{saved}");
    assert!(saved.contains("B"), "{saved}");
}

#[test]
fn set_rejects_scalar_ancestor_with_existing_message() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(&path, "execution = 1\n").expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    let error = store
        .set_value("execution.env.pass", "[\"A\"]")
        .expect_err("scalar ancestor must be rejected");

    assert!(
        error
            .to_string()
            .contains("'execution' along its path is already a non-table value"),
        "{error}"
    );
}

#[test]
fn set_falls_back_to_plain_string_for_non_literal_values() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    // Not a valid TOML literal on its own (bare/unquoted identifier with a
    // dot) — must fall back to being stored as the literal string.
    store
        .set_value("workflow.base_branch", "agent-main")
        .expect("set bare string value");
    store.validate().expect("validate");

    let value = store
        .effective_value("workflow.base_branch")
        .expect("get value");
    assert_eq!(value, serde_json::json!("agent-main"));
}

#[test]
fn set_validate_and_save_round_trips() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    store
        .set_value("workflow.base_branch", "agent-main")
        .expect("set value");
    store.validate().expect("validate");
    store.save().expect("save");

    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(saved.contains("base_branch = \"agent-main\""));

    let reopened = ConfigStore::open(ConfigScope::Workspace, &path).expect("reopen store");
    let value = reopened
        .effective_value("workflow.base_branch")
        .expect("get value");
    assert_eq!(value, serde_json::json!("agent-main"));
}

#[test]
fn set_preserves_comments_and_unrelated_formatting() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let original = "# top comment\n[workflow]\n# base branch comment\nbase_branch = \"main\"\ndefault_crew = \"sol\" # inline comment\n";
    fs::write(&path, original).expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("workflow.base_branch", "agent-main")
        .expect("set value");
    store.validate().expect("validate");
    store.save().expect("save");

    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(saved.contains("# top comment"), "saved:\n{saved}");
    assert!(saved.contains("# base branch comment"), "saved:\n{saved}");
    assert!(
        saved.contains("default_crew = \"sol\" # inline comment"),
        "saved:\n{saved}"
    );
    assert!(
        saved.contains("base_branch = \"agent-main\""),
        "saved:\n{saved}"
    );
}

#[test]
fn set_rejects_invalid_value_and_leaves_file_byte_identical() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let original = "[execution.codex]\nsandbox = \"workspace-write\"\n";
    fs::write(&path, original).expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("execution.codex.sandbox", "not-a-real-mode")
        .expect("set_value only mutates the in-memory document");

    let error = store
        .validate()
        .expect_err("invalid sandbox mode must fail validation");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");

    // `set_value`/`validate` never touch disk; only `save` (which the
    // caller must not call after a failed `validate`) does. Confirm the
    // file on disk is untouched, byte for byte.
    let after = fs::read(&path).expect("read config after failed validate");
    assert_eq!(after, original.as_bytes());
}

#[test]
fn set_rejects_unknown_key_with_suggestions() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    let error = store
        .set_value("workflow.not_a_real_key", "value")
        .expect_err("unknown key must be rejected");
    match error {
        OrbitError::InvalidInputDiagnostic {
            message,
            did_you_mean,
        } => {
            assert!(message.contains("workflow.not_a_real_key"));
            assert!(did_you_mean.contains(&"workflow.base_branch".to_string()));
        }
        other => panic!("expected InvalidInputDiagnostic, got {other:?}"),
    }
}

#[test]
fn get_rejects_unknown_key() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    let error = store
        .effective_value("nope.not.real")
        .expect_err("unknown key must be rejected");
    assert!(error.did_you_mean().is_some());
}

#[test]
fn open_for_workspace_set_fails_closed_without_flag() {
    let dir = tempdir().expect("tempdir");
    let workspace_path = config_path(dir.path());
    let global_dir = tempdir().expect("global tempdir");
    let global_path = config_path(global_dir.path());

    let result = ConfigStore::open_for_workspace_set(
        &workspace_path,
        &global_path,
        WorkspaceInitMode::RequireExisting,
    );
    let error = match result {
        Err(err) => err,
        Ok(_) => panic!("missing workspace config must fail closed"),
    };
    let message = error.to_string();
    assert!(message.contains("--seed-from-global"), "{message}");
    assert!(message.contains("--fresh"), "{message}");
    assert!(!workspace_path.exists());
}

#[test]
fn open_for_workspace_set_seeds_from_global() {
    let dir = tempdir().expect("tempdir");
    let workspace_path = config_path(dir.path());
    let global_dir = tempdir().expect("global tempdir");
    let global_path = config_path(global_dir.path());
    fs::write(&global_path, "[workflow]\nbase_branch = \"main\"\n").expect("write global config");

    let store = ConfigStore::open_for_workspace_set(
        &workspace_path,
        &global_path,
        WorkspaceInitMode::SeedFromGlobal,
    )
    .expect("seed from global");

    let value = store
        .effective_value("workflow.base_branch")
        .expect("get value");
    assert_eq!(value, serde_json::json!("main"));
}

#[test]
fn get_returns_default_log_rotation_values_when_absent() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open empty store");

    assert_eq!(
        store
            .effective_value("runtime.log_retention_days")
            .expect("get retention default"),
        serde_json::json!(7)
    );
    assert_eq!(
        store
            .effective_value("runtime.log_max_total_mb")
            .expect("get total mb default"),
        serde_json::json!(500)
    );
    assert_eq!(
        store
            .effective_value("runtime.log_max_file_mb")
            .expect("get file mb default"),
        serde_json::json!(100)
    );
}

#[test]
fn get_returns_configured_log_rotation_values() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(
        &path,
        "[runtime]\nlog_retention_days = 14\nlog_max_total_mb = 200\nlog_max_file_mb = 20\n",
    )
    .expect("write config");

    let store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    assert_eq!(
        store
            .effective_value("runtime.log_retention_days")
            .expect("get retention"),
        serde_json::json!(14)
    );
    assert_eq!(
        store
            .effective_value("runtime.log_max_total_mb")
            .expect("get total mb"),
        serde_json::json!(200)
    );
    assert_eq!(
        store
            .effective_value("runtime.log_max_file_mb")
            .expect("get file mb"),
        serde_json::json!(20)
    );
}

#[test]
fn set_log_rotation_writes_and_round_trips() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    store
        .set_value("runtime.log_retention_days", "30")
        .expect("set retention");
    store
        .set_value("runtime.log_max_total_mb", "1000")
        .expect("set total mb");
    store
        .set_value("runtime.log_max_file_mb", "50")
        .expect("set file mb");
    store.validate().expect("validate");
    store.save().expect("save");

    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(
        saved.contains("log_retention_days = 30"),
        "expected integer literal, got:\n{saved}"
    );
    assert!(saved.contains("log_max_total_mb = 1000"), "{saved}");
    assert!(saved.contains("log_max_file_mb = 50"), "{saved}");

    let reopened = ConfigStore::open(ConfigScope::Workspace, &path).expect("reopen store");
    assert_eq!(
        reopened
            .effective_value("runtime.log_retention_days")
            .expect("get retention"),
        serde_json::json!(30)
    );
    assert_eq!(
        reopened
            .effective_value("runtime.log_max_total_mb")
            .expect("get total mb"),
        serde_json::json!(1000)
    );
    assert_eq!(
        reopened
            .effective_value("runtime.log_max_file_mb")
            .expect("get file mb"),
        serde_json::json!(50)
    );
}

#[test]
fn set_log_rotation_rejects_out_of_range_via_validate() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");

    // per-file budget above total must fail through the same
    // LogRotationConfig::from_parts pipeline the runtime uses at load.
    store
        .set_value("runtime.log_max_total_mb", "10")
        .expect("set_value only mutates in-memory");
    store
        .set_value("runtime.log_max_file_mb", "50")
        .expect("set_value only mutates in-memory");
    let error = store
        .validate()
        .expect_err("per-file budget above total must fail validation");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");
    assert!(error.to_string().contains("log_max_file_mb"), "{error}");
}

#[test]
fn keys_registry_lists_all_runtime_log_keys() {
    use crate::registry::CONFIG_KEY_REGISTRY;
    let keys: Vec<&str> = CONFIG_KEY_REGISTRY.iter().map(|entry| entry.key).collect();
    assert!(keys.contains(&"runtime.log_retention_days"), "{keys:?}");
    assert!(keys.contains(&"runtime.log_max_total_mb"), "{keys:?}");
    assert!(keys.contains(&"runtime.log_max_file_mb"), "{keys:?}");
    for key in [
        "runtime.log_retention_days",
        "runtime.log_max_total_mb",
        "runtime.log_max_file_mb",
    ] {
        let entry = CONFIG_KEY_REGISTRY
            .iter()
            .find(|e| e.key == key)
            .expect("registered");
        assert_eq!(entry.value_type, "integer", "key: {key}");
        assert!(!entry.description.is_empty(), "key: {key}");
    }
}

#[test]
fn admission_registry_snapshot_and_lookup_are_complete() {
    use crate::registry::{CONFIG_KEY_REGISTRY, ConfigSnapshot};

    let snapshot = ConfigSnapshot::default();
    let values = snapshot.all_values();
    assert_eq!(values.len(), CONFIG_KEY_REGISTRY.len());
    for (descriptor, (key, value)) in CONFIG_KEY_REGISTRY.iter().zip(values) {
        assert_eq!(descriptor.key, key);
        assert_eq!(snapshot.value_for(key), Some(value), "key: {key}");
        assert!(!descriptor.value_type.is_empty(), "key: {key}");
        assert!(!descriptor.description.is_empty(), "key: {key}");
    }
    assert!(
        CONFIG_KEY_REGISTRY
            .windows(2)
            .all(|pair| pair[0].key < pair[1].key),
        "registry order drives stable config keys/show output"
    );
}

fn sol_crew_document() -> &'static str {
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nmodel = \"gpt-5.6-sol\"\nprovider = \"codex\"\n# keep this comment\n"
}

#[test]
fn get_crew_effort_is_null_when_omitted_and_does_not_invent_a_default() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(&path, sol_crew_document()).expect("write config");

    let store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    assert_eq!(
        store
            .effective_value("crews.sol.effort")
            .expect("omitted effort is an admitted key"),
        serde_json::Value::Null
    );
}

#[test]
fn set_crew_effort_round_trips_and_preserves_unrelated_toml() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(&path, sol_crew_document()).expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("crews.sol.effort", "high")
        .expect("set crew effort");
    store.validate().expect("validate");
    store.save().expect("save");

    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(saved.contains("effort = \"high\""), "{saved}");
    assert!(saved.contains("# keep this comment"), "{saved}");
    assert!(saved.contains("default_crew = \"sol\""), "{saved}");
    assert!(saved.contains("model = \"gpt-5.6-sol\""), "{saved}");

    let reopened = ConfigStore::open(ConfigScope::Workspace, &path).expect("reopen store");
    assert_eq!(
        reopened
            .effective_value("crews.sol.effort")
            .expect("get configured effort"),
        serde_json::json!("high")
    );
}

#[test]
fn hand_authored_crew_effort_is_readable_without_config_set() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(
        &path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nmodel = \"gpt-5.6-sol\"\nprovider = \"codex\"\neffort = \"high\"\n",
    )
    .expect("write config");

    let store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    assert_eq!(
        store
            .effective_value("crews.sol.effort")
            .expect("hand-authored effort is readable"),
        serde_json::json!("high")
    );
}

#[test]
fn set_crew_effort_rejects_invalid_value_and_leaves_file_byte_identical() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let original = sol_crew_document();
    fs::write(&path, original).expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("crews.sol.effort", "medium-low")
        .expect("set_value only mutates in-memory");
    let error = store
        .validate()
        .expect_err("invalid effort must fail validation");
    assert!(
        error
            .to_string()
            .contains("expected one of low, medium, high, xhigh, max"),
        "{error}"
    );
    assert_eq!(
        fs::read(&path).expect("read after failed validate"),
        original.as_bytes()
    );
}

#[test]
fn set_crew_effort_rejects_unsupported_provider_before_save() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let original = "[workflow]\ndefault_crew = \"gemini\"\n\n[crews.gemini]\nmodel = \"gemini\"\nprovider = \"gemini\"\n";
    fs::write(&path, original).expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("crews.gemini.effort", "high")
        .expect("set_value only mutates in-memory");
    let error = store
        .validate()
        .expect_err("unsupported provider must fail closed");
    assert!(
        error
            .to_string()
            .contains("does not support configured reasoning effort"),
        "{error}"
    );
    assert_eq!(
        fs::read(&path).expect("read after failed validate"),
        original.as_bytes()
    );
}

#[test]
fn set_rejects_unknown_crew_field_before_mutating_document() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let original = sol_crew_document();
    fs::write(&path, original).expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    let error = store
        .set_value("crews.sol.effrot", "high")
        .expect_err("misspelled crew field must be rejected");
    match error {
        OrbitError::InvalidInputDiagnostic {
            message,
            did_you_mean,
        } => {
            assert!(message.contains("effrot"), "{message}");
            assert!(
                did_you_mean.contains(&"crews.sol.effort".to_string()),
                "{did_you_mean:?}"
            );
        }
        other => panic!("expected InvalidInputDiagnostic, got {other:?}"),
    }
    assert_eq!(
        fs::read(&path).expect("read after rejected set"),
        original.as_bytes()
    );
}

#[test]
fn grok_crew_effort_round_trips_supported_value() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    fs::write(
        &path,
        "[workflow]\ndefault_crew = \"grok\"\n\n[crews.grok]\nmodel = \"grok-4.6\"\nprovider = \"grok\"\n",
    )
    .expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("crews.grok.effort", "xhigh")
        .expect("set grok effort");
    store.validate().expect("validate grok effort");
    store.save().expect("save");

    let reopened = ConfigStore::open(ConfigScope::Workspace, &path).expect("reopen store");
    assert_eq!(
        reopened
            .effective_value("crews.grok.effort")
            .expect("get grok effort"),
        serde_json::json!("xhigh")
    );
}

#[test]
fn open_for_workspace_set_fresh_starts_empty() {
    let dir = tempdir().expect("tempdir");
    let workspace_path = config_path(dir.path());
    let global_dir = tempdir().expect("global tempdir");
    let global_path = config_path(global_dir.path());
    fs::write(&global_path, "[workflow]\nbase_branch = \"agent-main\"\n")
        .expect("write global config");

    let store = ConfigStore::open_for_workspace_set(
        &workspace_path,
        &global_path,
        WorkspaceInitMode::Fresh,
    )
    .expect("fresh store");

    let value = store
        .effective_value("workflow.base_branch")
        .expect("get default");
    assert_eq!(value, serde_json::json!("main"));
}

#[test]
fn exists_on_disk_and_explicit_value_for_missing_and_present_keys() {
    let dir = tempdir().expect("tempdir");
    let missing_path = config_path(dir.path());
    let store = ConfigStore::open(ConfigScope::Workspace, &missing_path).expect("open store");

    assert!(!store.exists_on_disk());
    assert!(!store.is_key_set("scoring.enabled"));
    assert_eq!(
        store
            .explicit_value("scoring.enabled")
            .expect("query unset"),
        None
    );

    let present_dir = tempdir().expect("present tempdir");
    let present_path = config_path(present_dir.path());
    fs::write(
        &present_path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[scoring]\nenabled = false\n\n[crews.sol]\nmodel = \"gpt-5.6-sol\"\nprovider = \"codex\"\n",
    )
    .expect("write config");

    let present_store =
        ConfigStore::open(ConfigScope::Workspace, &present_path).expect("open present store");
    assert!(present_store.exists_on_disk());
    assert!(present_store.is_key_set("scoring.enabled"));
    assert_eq!(
        present_store
            .explicit_value("scoring.enabled")
            .expect("query set"),
        Some(serde_json::json!(false))
    );
    assert!(!present_store.is_key_set("workflow.base_branch"));
    assert_eq!(
        present_store
            .explicit_value("workflow.base_branch")
            .expect("query unset in file"),
        None
    );
    assert!(present_store.is_key_set("crews.sol.model"));
    assert_eq!(
        present_store
            .explicit_value("crews.sol.model")
            .expect("query crew model"),
        Some(serde_json::json!("gpt-5.6-sol"))
    );
    assert!(!present_store.is_key_set("crews.sol.effort"));
    assert_eq!(
        present_store
            .explicit_value("crews.sol.effort")
            .expect("query unset crew effort"),
        None
    );
}
