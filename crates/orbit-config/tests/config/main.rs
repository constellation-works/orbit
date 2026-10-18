//! Orbit-config integration coverage in one area binary.
//!
//! Layering, crew defaults, review layers and plugin configuration share
//! this target. They do not install signal handlers or mutate the process
//! environment outside `orbit_common::test_env`.

#![allow(clippy::expect_used, missing_docs)]

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
use orbit_config::OperationLayerSource;
use orbit_config::PluginConfigSchema;
use orbit_config::ProviderLimitBudgetUnit;
use orbit_config::ProviderLimitExplicitCrews;
use orbit_config::ProviderLimitPolicy;
use orbit_config::ProviderLimitThreshold;
use orbit_config::RETIRED_SYSTEM_CREW;
use orbit_config::ResolvedConfig;
use orbit_config::WorkspaceInitMode;
use orbit_config::admit_config_key;
use orbit_config::admit_settable_config_key;
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

include!("agent_stall.rs");
include!("config_paths.rs");
include!("config_store.rs");
include!("default_crew.rs");
include!("fallbacks.rs");
include!("ignored_crew_attribution.rs");
include!("plugin_config.rs");
include!("provider_limit.rs");
include!("retired_system_crew.rs");
include!("review_layers.rs");
include!("unknown_keys.rs");
