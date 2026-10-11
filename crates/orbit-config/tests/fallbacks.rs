//! Crew fallback predictions compared with actual field removal and reload.
#![allow(clippy::expect_used, missing_docs)]

use std::path::Path;
use std::process::Command;

use orbit_common::OrbitError;
use orbit_common::test_env::{assert_child_test_passed, run_child_test};
use orbit_config::{ConfigRoots, ConfigValueSourceKind, load_effective_config};
use serde_json::json;

fn isolated(test: &str) -> bool {
    const CHILD: &str = "ORBIT_CONFIG_FALLBACK_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let directory = tempfile::tempdir().expect("child directory");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", test, "--nocapture"])
        .env_clear()
        .env(CHILD, "1")
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("TMPDIR", directory.path())
        .env("TEMP", directory.path())
        .env("TMP", directory.path());
    let result = run_child_test(&mut command, test, directory.path());
    assert_child_test_passed(test, result.status, &result.stdout, &result.stderr);
    false
}

fn write_config(root: &Path, body: &str) {
    std::fs::write(root.join("config.toml"), body).expect("write config");
}

fn remove_field(root: &Path, crew: &str, field: &str) {
    let path = root.join("config.toml");
    let mut document: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).expect("read config"))
            .expect("parse config");
    document["crews"][crew]
        .as_table_mut()
        .expect("crew table")
        .remove(field)
        .expect("field was set");
    std::fs::write(path, toml::to_string(&document).expect("serialize config"))
        .expect("write field removal");
}

#[test]
fn optional_crew_fallbacks_match_entry_defaults_after_removal() {
    if !isolated("optional_crew_fallbacks_match_entry_defaults_after_removal") {
        return;
    }
    let global = tempfile::tempdir().expect("global directory");
    let workspace = tempfile::tempdir().expect("workspace directory");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let body = "[workflow]\ndefault_crew = 'custom'\n\
        [crews.custom]\nprovider = 'codex'\nmodel = 'custom-model'\n\
        enabled = false\ndescription = 'custom description'\ntags = ['custom']\neffort = 'high'\n";

    for root in [global.path(), workspace.path()] {
        for (field, expected) in [
            ("enabled", json!(true)),
            ("description", json!(null)),
            ("tags", json!([])),
            ("effort", json!(null)),
        ] {
            write_config(global.path(), "");
            write_config(workspace.path(), "");
            write_config(root, body);
            let key = format!("crews.custom.{field}");
            let effective = load_effective_config(&roots).expect("config loads before removal");
            let row = effective
                .values()
                .iter()
                .find(|row| row.key == key)
                .expect("crew field");
            let fallback = row.fallback.as_ref().expect("optional entry default");
            assert_eq!(fallback.layer, ConfigValueSourceKind::BuiltIn);
            assert_eq!(fallback.value, expected);

            remove_field(root, "custom", field);
            let reloaded = load_effective_config(&roots).expect("optional field removal loads");
            assert_eq!(reloaded.value_for(&key), Some(fallback.value.clone()));
        }
    }
}

#[test]
fn required_crew_fields_without_valid_lower_values_have_no_fallback() {
    if !isolated("required_crew_fields_without_valid_lower_values_have_no_fallback") {
        return;
    }
    let global = tempfile::tempdir().expect("global directory");
    let workspace = tempfile::tempdir().expect("workspace directory");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    for crew in ["custom", "sol"] {
        for field in ["model", "provider"] {
            // A lower file may be absent or hold an invalid value currently
            // masked by the workspace. Neither promises a shipped fallback.
            for (root, invalid_global) in [
                (global.path(), false),
                (workspace.path(), false),
                (workspace.path(), true),
            ] {
                write_config(global.path(), "");
                write_config(workspace.path(), "");
                let body = format!(
                    "[workflow]\ndefault_crew = '{crew}'\n\
                     [crews.{crew}]\nmodel = 'custom-model'\nprovider = 'codex'\n"
                );
                if invalid_global {
                    write_config(global.path(), &format!("[crews.{crew}]\n{field} = ''\n"));
                }
                write_config(root, &body);
                let key = format!("crews.{crew}.{field}");
                let effective = load_effective_config(&roots).expect("config loads before removal");
                let row = effective
                    .values()
                    .iter()
                    .find(|row| row.key == key)
                    .expect("crew field");
                assert!(
                    row.fallback.is_none(),
                    "invalid required field removal has no prediction"
                );
                remove_field(root, crew, field);
                let error =
                    load_effective_config(&roots).expect_err("required field removal fails");
                let OrbitError::InvalidInput(message) = error else {
                    panic!("wrong refusal: {error}");
                };
                assert!(
                    message.contains(&format!("[crews.{crew}].{field}")),
                    "{message}"
                );
            }
        }
    }
}

#[test]
fn workspace_crew_fallbacks_match_admitted_global_fields_after_removal() {
    if !isolated("workspace_crew_fallbacks_match_admitted_global_fields_after_removal") {
        return;
    }
    let global = tempfile::tempdir().expect("global directory");
    let workspace = tempfile::tempdir().expect("workspace directory");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    write_config(
        global.path(),
        "[workflow]\ndefault_crew = 'custom'\n\
         [crews.custom]\nprovider = ' codex '\nmodel = ' global-model '\n\
         enabled = false\ndescription = ' global description '\n\
         tags = [' z ', 'a', 'a', '']\neffort = 'low'\n",
    );
    for field in [
        "model",
        "provider",
        "enabled",
        "description",
        "tags",
        "effort",
    ] {
        write_config(
            workspace.path(),
            "[crews.custom]\nprovider = 'codex'\nmodel = 'workspace-model'\n\
             enabled = true\ndescription = 'workspace description'\ntags = ['workspace']\neffort = 'high'\n",
        );
        let key = format!("crews.custom.{field}");
        let effective = load_effective_config(&roots).expect("merged crew loads");
        let row = effective
            .values()
            .iter()
            .find(|row| row.key == key)
            .expect("crew field");
        assert_eq!(row.source.kind(), ConfigValueSourceKind::Workspace);
        let fallback = row.fallback.as_ref().expect("global field fallback");
        assert_eq!(fallback.layer, ConfigValueSourceKind::Global);
        remove_field(workspace.path(), "custom", field);
        let reloaded = load_effective_config(&roots).expect("inherits global field after removal");
        assert_eq!(reloaded.value_for(&key), Some(fallback.value.clone()));
        let row = reloaded
            .values()
            .iter()
            .find(|row| row.key == key)
            .expect("inherited crew field");
        assert_eq!(row.source.kind(), ConfigValueSourceKind::Global);
    }
}
