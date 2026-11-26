//! Orbit-config integration coverage in one area binary.
//!
//! Layering, crew defaults, review layers and plugin configuration share
//! this target. They do not install signal handlers or mutate the process
//! environment outside `orbit_common::test_env`.

#![allow(clippy::expect_used, missing_docs)]

mod admission {
    use orbit_common::OrbitError;
    use orbit_config::{ConfigScope, ConfigStore};
    use std::fs;
    use tempfile::tempdir;

    fn config_path(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("config.toml")
    }

    #[test]
    fn set_rejects_invalid_value_and_leaves_file_byte_identical() {
        let dir = tempdir().expect("tempdir");
        let path = config_path(dir.path());
        let original = "[execution.codex]\nsandbox = \"workspace-write\"\n";
        fs::write(&path, original).expect("write config");

        let mut store =
            ConfigStore::open(&orbit_common::Env::capture(), ConfigScope::Workspace, &path)
                .expect("open store");
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

    #[cfg(unix)]
    #[test]
    fn open_refuses_a_symlinked_config_file() {
        let dir = tempdir().expect("tempdir");
        let real = dir.path().join("real.toml");
        fs::write(&real, "[workflow]\nbase_branch = \"agent-main\"\n").expect("write real config");
        let link = config_path(dir.path());
        std::os::unix::fs::symlink(&real, &link).expect("symlink config leaf");

        let error =
            match ConfigStore::open(&orbit_common::Env::capture(), ConfigScope::Workspace, &link) {
                Err(error) => error,
                Ok(_) => panic!("a symlinked config file must be refused"),
            };
        assert!(
            error
                .to_string()
                .contains("config path must not be a symlink"),
            "{error}"
        );
    }
}

#[path = "default_crew/crew_pools.rs"]
mod crew_pools;
#[path = "review_layers/deprecated_aliases.rs"]
mod deprecated_aliases;
#[path = "default_crew/execution_env.rs"]
mod execution_env;
#[path = "default_crew/layering.rs"]
mod layering;
#[path = "review_layers/machine.rs"]
mod machine;
#[path = "review_layers/operation.rs"]
mod operation;
#[path = "config_paths/plugin_enablement.rs"]
mod plugin_enablement;
#[path = "default_crew/seed.rs"]
mod seed;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_common::test_env::assert_child_test_passed;
use orbit_common::test_env::run_child_test;
use orbit_config::AutoConcurrencySettings;
use orbit_config::ConfigRoots;
use orbit_config::ConfigScope;
use orbit_config::ConfigSnapshot;
use orbit_config::ConfigStore;
use orbit_config::ConfigValueSourceKind;
use orbit_config::CrewSelection;
use orbit_config::OperationLayerSource;
use orbit_config::PRE_LANDING_REVIEW_CREW_KEY;
use orbit_config::PluginConfigSchema;
use orbit_config::ProviderLimitBudgetUnit;
use orbit_config::ProviderLimitExplicitCrews;
use orbit_config::ProviderLimitPolicy;
use orbit_config::ProviderLimitTarget;
use orbit_config::ProviderLimitThreshold;
use orbit_config::RETIRED_SYSTEM_CREW;
use orbit_config::REVIEW_CREW_KEY;
use orbit_config::ResolvedConfig;
use orbit_config::TaskTypeDefinition;
use orbit_config::UsagePressurePolicy;
use orbit_config::WorkspaceInitMode;
use orbit_config::admit_config_key;
use orbit_config::admit_settable_config_key;
use orbit_config::classify_task_type;
use orbit_config::load_effective_config;
use orbit_config::load_workspace_plugin_enablement;
use orbit_config::names_retired_system_crew;
use orbit_config::plugin_enablement_key;
use orbit_config::register_plugin_config_schemas;
use orbit_config::workspace_config_sets_policy;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use tracing_subscriber::fmt::MakeWriter;

include!("auto_effort.rs");
include!("config_paths.rs");
include!("config_store.rs");
include!("crew_env.rs");
include!("default_crew.rs");
include!("delivery_conventions.rs");
include!("fallbacks.rs");
include!("human_crews.rs");
include!("ignored_crew_attribution.rs");
include!("plugin_config.rs");
include!("task_types.rs");
include!("provider_limit.rs");
include!("retired_crew_memory_key.rs");
include!("retired_system_crew.rs");
include!("mcp_toolsets.rs");
include!("review_layers.rs");
include!("unknown_keys.rs");
