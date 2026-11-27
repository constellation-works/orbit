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

// The `include!`d files below share this module scope, so their constants and
// types sit here, ahead of every included function.

// Fixtures of `auto_effort.rs`.
const AUTO_EFFORT_CREW: &str = "[crews.sol]\nmodel = \"gpt-5.5\"\nprovider = \"codex\"\n";

const AUTO_EFFORT_BANDS: &str = "[[workflow.auto_effort.bands.sol]]\neffort = \"low\"\n\
     complexity = [\"low\"]\n\n[[workflow.auto_effort.bands.sol]]\neffort = \"high\"\n\
     complexity = [\"hard\", \"xhard\"]\n";

// Fixtures of `config_paths.rs`.
const GLOBAL: &str = "[execution.codex]\nsandbox = 'danger-full-access'\n\
                      [execution.env]\npass = ['GLOBAL_TOKEN']\n";

const TOGGLES: &str = "[plugin_enablement]\ngraph = false\n";

// Fixtures of `crew_env.rs`.
const CREW_ENV_CREW: &str = "[workflow]\ndefault_crew = \"opus-credits\"\n\n\
     [crews.opus-credits]\nprovider = \"claude\"\nmodel = \"claude-opus-5-5\"\n";

// Fixtures of `default_crew.rs`.
const KEY: &str = "workflow.default_crew";

const PROVIDER_ENV: &str = "CONSTELLATION_DEFAULT_PROVIDER";

const INVALID_PROVIDER: &str = "invalid-provider";

// Fixtures of `human_crews.rs`.
const HUMAN_CREW_AGENTS: &str = "[crews.sol]\nmodel = \"m\"\nprovider = \"codex\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";

// Fixtures of `provider_limit.rs`.
const OVERRIDES: &str = "workflow.provider_limit_overrides";

const BUDGETS: &str = "workflow.provider_limit_budgets";

// Fixtures of `retired_system_crew.rs`.
const RETIRED_SYSTEM_CREW_KEY: &str = "workflow.system_crew";

const LEGACY: &str = r#"[workflow]
default_crew = "opus"
system_crew = "haiku"
low_complexity_crews = ["haiku"]

[crews.opus]
provider = "claude"
model = "fixture-opus"

[crews.haiku]
provider = "claude"
model = "fixture-haiku"

[crews.qa]
provider = "codex"
model = "fixture-qa"
"#;

// Fixtures of `unknown_keys.rs`.
const HOST_EVIDENCE_RULE: &str = "[[review.host_evidence]]\n\
    kind = \"codeql\"\n\
    name = \"Rust CodeQL (Linux)\"\n\
    paths = [\"**/*.rs\"]\n\
    os = \"linux\"\n\
    command = \"scripts/codeql-rust-local.sh\"\n\
    artifact = \"evidence/codeql.json\"\n";

// Fixtures of `config_store.rs`.
struct Roots {
    global: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

impl Roots {
    fn new() -> Self {
        let roots = Self {
            global: tempfile::tempdir().expect("global tempdir"),
            workspace: tempfile::tempdir().expect("workspace tempdir"),
        };
        std::fs::write(roots.workspace_config(), "").expect("empty workspace config");
        roots
    }

    fn workspace_config(&self) -> PathBuf {
        self.workspace.path().join("config.toml")
    }

    fn global_config(&self) -> PathBuf {
        self.global.path().join("config.toml")
    }

    fn config_roots(&self) -> ConfigRoots {
        ConfigRoots::new(self.global.path(), self.workspace.path())
    }

    fn load(&self) -> ResolvedConfig {
        ResolvedConfig::load(&orbit_common::Env::capture(), &self.config_roots())
            .expect("final config loads")
    }

    fn toggles(&self) -> std::collections::BTreeMap<String, bool> {
        load_workspace_plugin_enablement(&self.config_roots()).expect("toggles load")
    }
}

// Fixtures of `mcp_toolsets.rs`.
#[derive(Clone)]
struct McpToolsetsCapture(Arc<Mutex<Vec<u8>>>);

impl Write for McpToolsetsCapture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("capture").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for McpToolsetsCapture {
    type Writer = McpToolsetsCapture;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

// Fixtures of `retired_system_crew.rs`.
#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("capture").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

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
