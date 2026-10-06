//! Install-time `spec.build`: the plan an operator consents to, the build
//! directory, both phases under the build profile, and the declared outputs
//! copied into an install's staging tree.
//!
//! Design: `docs/design/plugins/3_install_time_build.md`. Consent, the
//! refusal of unattended callers, the build record and its witness belong to
//! `orbit-core`; this module only runs what it is handed.

mod dir;
mod env;
mod install;
mod plan;
mod run;

pub use self::dir::{PluginBuildDir, is_live_plugin_build_dir};
pub use self::env::{
    PLUGIN_BUILD_DENIED_ENV, PLUGIN_BUILD_DENIED_ENV_PREFIXES, PLUGIN_BUILD_DIR_PREFIX,
    PLUGIN_BUILD_TOOLCHAIN_LOCATORS, PluginBuildHostEnv, is_denied_build_env,
    plugin_build_environment,
};
pub use self::install::{
    install_plugin_build_outputs, installed_artifact_digest, plugin_artifact_digest,
    prebuilt_outputs_present,
};
pub use self::plan::{PluginBuildPlan, ResolvedBuildProgram, plan_plugin_build};
pub use self::run::{PluginBuildPhase, PluginBuildResult, PluginBuildRun, run_plugin_build};
