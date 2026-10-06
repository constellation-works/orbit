//! Build environment and toolchain policy for an install-time plugin build.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::plan::PluginBuildPlan;
use super::run::PluginBuildPhase;

/// Prefix of a build directory's name under `~/.orbit/plugins/<ns>/`. The
/// full name is `.build-<pid>-<nonce>`, so pruning can tell a live build from
/// a leftover one without trusting anything the build could write.
pub const PLUGIN_BUILD_DIR_PREFIX: &str = ".build-";

/// Toolchain locators a build may receive (§3.5), each only under the rules
/// in [`plugin_build_environment`].
pub const PLUGIN_BUILD_TOOLCHAIN_LOCATORS: &[&str] = &[
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_HOME",
    "GOROOT",
    "JAVA_HOME",
];

/// Names never passed to a build, whatever the locator list later contains.
pub const PLUGIN_BUILD_DENIED_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "SSH_AUTH_SOCK",
    "GIT_ASKPASS",
    "NPM_TOKEN",
];

/// Prefixes of names never passed to a build.
pub const PLUGIN_BUILD_DENIED_ENV_PREFIXES: &[&str] = &["AWS_", "CARGO_REGISTRIES_"];

/// Whether `name` is on the build environment's fixed denylist.
pub fn is_denied_build_env(name: &str) -> bool {
    PLUGIN_BUILD_DENIED_ENV.contains(&name)
        || PLUGIN_BUILD_DENIED_ENV_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// System trees whose programs need no toolchain root: the build profile
/// already reads them.
const SYSTEM_PREFIXES: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/System"];

/// The operator environment a plan is resolved against: read once, by the
/// consenting command.
#[derive(Debug, Clone, Default)]
pub struct PluginBuildHostEnv {
    pub path: Option<std::ffi::OsString>,
    pub home: Option<PathBuf>,
    /// The operator's values for [`PLUGIN_BUILD_TOOLCHAIN_LOCATORS`].
    pub locators: Vec<(String, String)>,
}

impl PluginBuildHostEnv {
    pub fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            home: std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from),
            locators: PLUGIN_BUILD_TOOLCHAIN_LOCATORS
                .iter()
                .filter_map(|name| {
                    std::env::var(name)
                        .ok()
                        .filter(|value| !value.is_empty())
                        .map(|value| ((*name).to_string(), value))
                })
                .collect(),
        }
    }

    pub(super) fn locator(&self, name: &str) -> Option<&str> {
        self.locators
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Where a bare name is found on `PATH`, before canonicalization.
pub(super) fn invoked_path(name: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    if name.contains('/') {
        return Some(PathBuf::from(name));
    }
    std::env::split_paths(search_path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

pub(super) enum Toolchain {
    System,
    Prefix(PathBuf),
    Rustup {
        home: PathBuf,
        toolchains: PathBuf,
        settings: Option<PathBuf>,
        default_bin: Option<PathBuf>,
    },
}

/// The installation a program runs from (§3.2).
pub(super) fn toolchain_for(
    invoked: &Path,
    canonical: &Path,
    env: &PluginBuildHostEnv,
) -> Toolchain {
    let cargo_bin = env
        .locator("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env.home.as_ref().map(|home| home.join(".cargo")))
        .map(|cargo_home| cargo_home.join("bin"));
    let rustup_home = env
        .locator("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| env.home.as_ref().map(|home| home.join(".rustup")))
        .and_then(|home| home.canonicalize().ok());
    if let (Some(cargo_bin), Some(home)) = (cargo_bin, rustup_home)
        && invoked.parent() == Some(cargo_bin.as_path())
        && home.join("toolchains").is_dir()
    {
        let settings = home.join("settings.toml");
        let default_bin = std::fs::read_to_string(&settings).ok().and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.trim().strip_prefix("default_toolchain")?;
                let name = value.trim().strip_prefix('=')?.trim().trim_matches('"');
                (!name.is_empty() && !name.contains('/'))
                    .then(|| home.join("toolchains").join(name).join("bin"))
            })
        });
        return Toolchain::Rustup {
            toolchains: home.join("toolchains"),
            settings: settings.is_file().then_some(settings),
            default_bin: default_bin.filter(|bin| bin.is_dir()),
            home,
        };
    }
    if SYSTEM_PREFIXES
        .iter()
        .any(|prefix| canonical.starts_with(prefix))
    {
        return Toolchain::System;
    }
    match canonical.parent() {
        Some(bin) if bin.file_name() == Some(OsStr::new("bin")) => {
            bin.parent().map_or(Toolchain::System, |prefix| {
                Toolchain::Prefix(prefix.to_path_buf())
            })
        }
        _ => Toolchain::System,
    }
}

/// The complete environment of one phase (§3.5): built from nothing, never
/// from the operator's environment beyond the locators the plan decided.
pub fn plugin_build_environment(
    plan: &PluginBuildPlan,
    build_dir: &Path,
    phase: PluginBuildPhase,
) -> Vec<(String, String)> {
    let build = build_dir.display().to_string();
    let mut path: Vec<String> = plan
        .path_dirs
        .iter()
        .map(|dir| dir.display().to_string())
        .collect();
    path.extend(["/usr/bin".to_string(), "/bin".to_string()]);
    let mut env = vec![
        ("PATH".to_string(), path.join(":")),
        ("HOME".to_string(), format!("{build}/home")),
        ("TMPDIR".to_string(), format!("{build}/tmp")),
        ("LANG".to_string(), "C.UTF-8".to_string()),
        ("TZ".to_string(), "UTC".to_string()),
        (
            "SOURCE_DATE_EPOCH".to_string(),
            plan.committed_at.to_string(),
        ),
        ("ORBIT_BUILD_DIR".to_string(), build.clone()),
        ("ORBIT_BUILD_SRC".to_string(), format!("{build}/src")),
        ("ORBIT_BUILD_PHASE".to_string(), phase.name().to_string()),
        ("CARGO_HOME".to_string(), format!("{build}/home/.cargo")),
    ];
    env.extend(plan.locators.iter().cloned());
    env.retain(|(name, _)| !is_denied_build_env(name));
    env
}
