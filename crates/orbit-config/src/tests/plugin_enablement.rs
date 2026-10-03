//! `[plugin_enablement]`: workspace-only, validated, and never a policy layer.

use super::{roots, write_config};
use crate::{
    ConfigRoots, ResolvedConfig, load_workspace_plugin_enablement, workspace_config_sets_policy,
};

use std::fs;
use tempfile::tempdir;

const PERMISSIVE_GLOBAL: &str = r#"
[execution.codex]
sandbox = "danger-full-access"

[execution.env]
pass = ["GLOBAL_TOKEN"]
"#;

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

#[cfg(unix)]
#[test]
fn a_symlinked_workspace_config_fails_closed_as_a_policy_layer() {
    let workspace = tempdir().expect("workspace tempdir");
    let external = tempdir().expect("external tempdir");
    let external_config = external.path().join("config.toml");
    fs::write(&external_config, "[plugin_enablement]\ngraph = false\n")
        .expect("write external config");
    let workspace_config = workspace.path().join("config.toml");
    std::os::unix::fs::symlink(&external_config, &workspace_config)
        .expect("link workspace config to external config");

    assert!(
        workspace_config_sets_policy(&workspace_config),
        "a symlinked config must fail closed instead of reading outside the selected path"
    );
}
