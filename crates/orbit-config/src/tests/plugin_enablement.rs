//! `[plugin_enablement]`: workspace-only, validated, and never a policy layer.

use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;

use super::{roots, write_config};
use crate::{
    ConfigRoots, ConfigScope, ConfigStore, ResolvedConfig, WorkspaceInitMode,
    load_workspace_plugin_enablement, plugin_enablement_key, workspace_config_sets_policy,
};

const PERMISSIVE_GLOBAL: &str = r#"
[execution.codex]
sandbox = "danger-full-access"

[execution.env]
pass = ["GLOBAL_TOKEN"]
"#;

#[test]
fn workspace_toggles_resolve_and_read_back_without_the_rest_of_the_config() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        workspace.path(),
        "[scoring]\nenabled = false\n[plugin_enablement]\ngraph = false\nnotes = true\n",
    );
    let roots = roots(global.path(), workspace.path());

    let expected = BTreeMap::from([("graph".to_string(), false), ("notes".to_string(), true)]);
    let config = ResolvedConfig::load(&roots).expect("load");
    assert_eq!(config.plugin_enablement, expected);
    assert_eq!(
        load_workspace_plugin_enablement(&roots).expect("toggles"),
        expected
    );
}

#[test]
fn global_plugin_enablement_is_refused_at_load() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(global.path(), "[plugin_enablement]\ngraph = false\n");

    let error = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("a global toggle table is refused");
    assert!(matches!(error, orbit_common::OrbitError::InvalidInput(_)));

    // A global-only runtime reads no workspace layer, so it has no toggles
    // even when the one file it reads is the global file.
    let only = tempdir().expect("root tempdir");
    assert!(
        load_workspace_plugin_enablement(&ConfigRoots::global_only(only.path()))
            .expect("toggles")
            .is_empty()
    );
}

#[test]
fn global_store_validation_refuses_the_toggle_table() {
    let global = tempdir().expect("global tempdir");
    let path = global.path().join("config.toml");
    fs::write(&path, "[plugin_enablement]\ngraph = true\n").expect("write global");

    let store = ConfigStore::open(ConfigScope::Global, &path).expect("open");
    assert!(store.validate().is_err());

    let workspace = tempdir().expect("workspace tempdir");
    let workspace_path = workspace.path().join("config.toml");
    fs::write(&workspace_path, "[plugin_enablement]\ngraph = true\n").expect("write workspace");
    ConfigStore::open(ConfigScope::Workspace, &workspace_path)
        .expect("open")
        .validate()
        .expect("the workspace file may carry toggles");
}

#[test]
fn a_toggle_only_workspace_file_keeps_the_security_keys_inherited() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(global.path(), PERMISSIVE_GLOBAL);
    write_config(workspace.path(), "[plugin_enablement]\ngraph = false\n");

    let config = ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("load");
    assert_eq!(config.codex_execution.sandbox(), "danger-full-access");
    assert!(
        config
            .snapshot
            .execution_env_pass
            .contains(&"GLOBAL_TOKEN".to_string())
    );
    assert!(!workspace_config_sets_policy(
        &workspace.path().join("config.toml")
    ));

    // Any real setting makes the file a policy layer again, and a file with
    // no toggle table stays one however little it holds.
    for body in [
        "[scoring]\nenabled = false\n[plugin_enablement]\ngraph = false\n",
        "# operator notes\n",
    ] {
        write_config(workspace.path(), body);
        let config = ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("load");
        assert_eq!(
            config.codex_execution.sandbox(),
            "workspace-write",
            "{body}"
        );
        assert!(
            workspace_config_sets_policy(&workspace.path().join("config.toml")),
            "{body}"
        );
    }
}

#[test]
fn a_malformed_toggle_is_refused() {
    let global = tempdir().expect("global tempdir");
    for body in [
        "[plugin_enablement]\ngraph = \"off\"\n",
        "[plugin_enablement]\n\"Not A Plugin\" = false\n",
        "plugin_enablement = true\n",
    ] {
        let workspace = tempdir().expect("workspace tempdir");
        write_config(workspace.path(), body);
        let roots = roots(global.path(), workspace.path());
        assert!(
            ResolvedConfig::load(&roots).is_err(),
            "load accepted {body}"
        );
        assert!(
            load_workspace_plugin_enablement(&roots).is_err(),
            "toggle read accepted {body}"
        );
    }
}

#[test]
fn the_first_setting_in_a_toggle_only_file_still_fails_closed_and_keeps_the_toggles() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_path = workspace.path().join("config.toml");
    let global = tempdir().expect("global tempdir");
    let global_path = global.path().join("config.toml");
    fs::write(&global_path, "[workflow]\nbase_branch = \"trunk\"\n").expect("write global");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &workspace_path).expect("open");
    store
        .set_document_value(&plugin_enablement_key("graph"), "false")
        .expect("set toggle");
    store.save().expect("save toggle");

    assert!(
        ConfigStore::open_for_workspace_set(
            &workspace_path,
            &global_path,
            WorkspaceInitMode::RequireExisting,
        )
        .is_err(),
        "a toggle-only file is not an existing policy layer"
    );

    let store = ConfigStore::open_for_workspace_set(
        &workspace_path,
        &global_path,
        WorkspaceInitMode::SeedFromGlobal,
    )
    .expect("seed from global");
    store.save().expect("save seeded");
    let roots = roots(global.path(), workspace.path());
    assert_eq!(
        load_workspace_plugin_enablement(&roots).expect("toggles"),
        BTreeMap::from([("graph".to_string(), false)]),
    );
    assert_eq!(
        ResolvedConfig::load(&roots)
            .expect("load")
            .workflow_base_branch,
        "trunk"
    );
}
