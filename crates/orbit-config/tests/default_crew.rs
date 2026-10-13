//! Crew admission, seed compatibility, layering and environment projection
//! through public config APIs. Cases that own the environment run in children.
#![allow(clippy::expect_used, missing_docs)]

use std::path::Path;
use std::process::Command;

use orbit_common::OrbitError;
use orbit_common::test_env::{assert_child_test_passed, run_child_test};
use orbit_config::{
    ConfigRoots, ConfigSnapshot, ConfigValueSourceKind, ResolvedConfig, load_effective_config,
};

const KEY: &str = "workflow.default_crew";
const PROVIDER_ENV: &str = "CONSTELLATION_DEFAULT_PROVIDER";
const INVALID_PROVIDER: &str = "invalid-provider";

fn write_config(root: &Path, body: &str) {
    std::fs::create_dir_all(root).expect("config directory");
    std::fs::write(root.join("config.toml"), body).expect("config document");
}

fn assert_selected(
    roots: &ConfigRoots,
    expected: &str,
    source: ConfigValueSourceKind,
    source_path: Option<&Path>,
) {
    let resolved = ResolvedConfig::load(roots).expect("runtime admission");
    assert_eq!(resolved.default_crew.as_deref(), Some(expected));
    assert_eq!(
        resolved.snapshot.workflow_default_crew,
        resolved.default_crew
    );
    let effective = load_effective_config(roots).expect("effective admission");
    let row = effective
        .values()
        .iter()
        .find(|row| row.key == KEY)
        .expect("default-crew provenance");
    assert_eq!(row.value, serde_json::json!(expected));
    assert_eq!(effective.value_for(KEY), Some(row.value.clone()));
    assert_eq!(row.source.kind(), source);
    assert_eq!(row.source.path(), source_path);
    assert_eq!(
        row.built_in,
        ConfigSnapshot::default()
            .value_for(KEY)
            .expect("shipped crew")
    );
    if source == ConfigValueSourceKind::Environment {
        let fallback = row.fallback.as_ref().expect("built-in fallback");
        assert_eq!(fallback.layer, ConfigValueSourceKind::BuiltIn);
        assert_eq!(fallback.value, row.built_in);
    } else if source == ConfigValueSourceKind::BuiltIn {
        assert!(row.fallback.is_none());
    }
}

fn assert_invalid_provider(error: OrbitError) {
    let OrbitError::InvalidInput(message) = error else {
        panic!("expected provider admission refusal, got {error}");
    };
    assert!(
        message.contains("CONSTELLATION_DEFAULT_PROVIDER has invalid value"),
        "wrong admission cause: {message}"
    );
}

#[test]
fn default_crew_admission_and_provenance_do_not_depend_on_file_presence() {
    const CHILD: &str = "ORBIT_CONFIG_DEFAULT_CREW_CHILD";
    const TEST: &str = "default_crew_admission_and_provenance_do_not_depend_on_file_presence";
    if std::env::var_os(CHILD).is_none() {
        for provider in [
            None,
            Some("codex"),
            Some(""),
            Some(" \t "),
            Some(INVALID_PROVIDER),
        ] {
            let output = tempfile::tempdir().expect("child output directory");
            let mut command = Command::new(std::env::current_exe().expect("test binary"));
            command
                .args(["--exact", TEST, "--nocapture"])
                .env_clear()
                .env(CHILD, "1")
                .env("HOME", output.path())
                .env("USERPROFILE", output.path())
                .env("TMPDIR", output.path())
                .env("TEMP", output.path())
                .env("TMP", output.path());
            if let Some(provider) = provider {
                command.env(PROVIDER_ENV, provider);
            }
            let result = run_child_test(&mut command, TEST, output.path());
            assert_child_test_passed(TEST, result.status, &result.stdout, &result.stderr);
        }
        return;
    }

    let provider = std::env::var(PROVIDER_ENV).ok();
    let shipped = ConfigSnapshot::default();
    assert_eq!(
        shipped.workflow_default_crew.as_deref(),
        Some("opus"),
        "ORB-15556: the built-in snapshot must stay independent of the host environment"
    );
    let (expected, source) = if provider.as_deref() == Some("codex") {
        // ORB-15556: the documented codex mapping was bypassed for absent files.
        ("sol", ConfigValueSourceKind::Environment)
    } else {
        (
            shipped
                .workflow_default_crew
                .as_deref()
                .expect("shipped crew"),
            ConfigValueSourceKind::BuiltIn,
        )
    };

    for (global, workspace) in [
        (None, None),
        (Some(""), None),
        (None, Some("")),
        (Some(""), Some("")),
        (None, Some("[plugin_enablement]\ngraph = false\n")),
    ] {
        let directory = tempfile::tempdir().expect("fixture directory");
        let roots = ConfigRoots::new(
            directory.path().join("global"),
            directory.path().join("workspace"),
        );
        if let Some(body) = global {
            write_config(roots.global(), body);
        }
        if let Some(body) = workspace {
            write_config(roots.workspace(), body);
        }
        if provider.as_deref() == Some(INVALID_PROVIDER) {
            assert_invalid_provider(ResolvedConfig::load(&roots).expect_err("invalid provider"));
            assert_invalid_provider(load_effective_config(&roots).expect_err("invalid provider"));
        } else {
            assert_selected(&roots, expected, source, None);
            if workspace.is_some_and(|body| !body.is_empty()) {
                assert_eq!(
                    ResolvedConfig::load(&roots)
                        .expect("plugin-only file")
                        .plugin_enablement
                        .get("graph"),
                    Some(&false)
                );
            }
        }

        // Either file wins over valid or invalid environment input, and the
        // effective value names the file that actually supplied it.
        write_config(roots.global(), "[workflow]\ndefault_crew = 'opus'\n");
        let global_path = roots.global().join("config.toml");
        assert_selected(
            &roots,
            "opus",
            ConfigValueSourceKind::Global,
            Some(&global_path),
        );
        write_config(roots.workspace(), "[workflow]\ndefault_crew = 'luna'\n");
        let workspace_path = roots.workspace().join("config.toml");
        assert_selected(
            &roots,
            "luna",
            ConfigValueSourceKind::Workspace,
            Some(&workspace_path),
        );
    }
}

#[path = "default_crew/crew_pools.rs"]
mod crew_pools;

#[path = "default_crew/layering.rs"]
mod layering;

#[path = "default_crew/seed.rs"]
mod seed;

#[path = "default_crew/execution_env.rs"]
mod execution_env;
