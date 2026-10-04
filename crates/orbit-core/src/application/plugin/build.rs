//! Install-time `spec.build`: when a source builds, who may consent, and the
//! build record an install keeps
//! (`docs/design/plugins/3_install_time_build.md`).
//!
//! Only `git+<url>#<full commit id>` builds (§3.1). Any other source whose
//! manifest declares `spec.build` installs only when every declared output
//! already ships in the tree, so a prebuilt archive can carry the same
//! manifest. A building source needs `--allow-build` on this command line
//! (§3.8): no pin, config key, environment variable, routine, job or MCP
//! tool supplies it, and a managed run, an agent sandbox or a plugin backend
//! cannot pass it. macOS refuses a manifest that declares a network `fetch`
//! phase (§3.3); its offline builds run.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_tools::plugin::{
    LoadedPlugin, PluginBuildHostEnv, PluginBuildPlan, PluginBuildResult, PluginBuildRun,
    ResolvedSource, install_plugin_build_outputs, plan_plugin_build, plugin_artifact_digest,
    prebuilt_outputs_present, run_plugin_build,
};
use orbit_types::plugin::{
    PLUGIN_BUILD_CONSENT_FLAG, PluginBuildConsent, PluginBuildProgram, PluginBuildRecord,
    git_commit_source, parse_archive_digest,
};

use crate::OrbitRuntime;
use crate::runtime::plugin::paths::{plugin_namespace_dir, read_pin_file};
use crate::runtime::plugin::sandbox_mask::plugin_trees_masked;

/// Environment names whose presence means no operator is at this command
/// line (§3.8): a managed-run envelope, or a plugin backend child.
const UNATTENDED_ENV: &[&str] = &[
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_RUN_ID",
    "ORBIT_PLUGIN",
    "ORBIT_PLUGIN_CALLBACK",
    "ORBIT_PLUGIN_CALLBACK_FD",
    "ORBIT_PLUGIN_BROKER",
];

/// A build that ran and whose outputs wait to be copied into staging.
pub(super) struct PreparedBuild {
    plan: PluginBuildPlan,
    result: PluginBuildResult,
    log: PathBuf,
}

/// Decide what `spec.build` means for this install, and run the build when
/// the source builds and the operator consented. `None` when nothing is
/// built. Runs before the namespace lock, as source resolution does.
pub(super) fn prepare_build(
    runtime: &OrbitRuntime,
    plugin: &LoadedPlugin,
    resolved: &ResolvedSource,
    source: &str,
    allow_build: bool,
    show_plan: Option<fn(&str)>,
) -> Result<Option<PreparedBuild>, OrbitError> {
    let Some(spec) = &plugin.manifest.spec.build else {
        return Ok(None);
    };
    let name = plugin.namespace();
    let (Some(commit), Some(_)) = (&resolved.commit, git_commit_source(source)) else {
        // §3.1: a source that never builds must already ship its outputs.
        return prebuilt_outputs_present(&resolved.root, &spec.outputs)
            .map(|()| None)
            .map_err(|missing| {
                OrbitError::InvalidInput(format!(
                    "plugin '{name}' declares spec.build, but only a `git+<url>#<full commit id>` \
                     source is built, and this source does not ship the declared output \
                     '{missing}'. Install a prebuilt archive, or name a full commit and pass \
                     {PLUGIN_BUILD_CONSENT_FLAG}"
                ))
            });
    };
    if spec.fetch.is_some() && !orbit_exec::BUILD_FETCH_PHASE_SUPPORTED {
        return Err(OrbitError::PluginBuildFetchUnsupported(format!(
            "plugin '{name}' declares a network `spec.build.fetch` phase, which this platform \
             does not run: macOS has no pid namespace, so a fetch process that leaves its \
             process group would keep its network after the phase. Nothing was built or \
             installed. Build on Linux, install a prebuilt archive, or use a source that \
             vendors its dependencies and builds offline"
        )));
    }
    let global_root = runtime.global_root();
    let forbidden = vec![global_root.clone(), runtime.paths().repo_root.clone()];
    let plan = plan_plugin_build(
        spec,
        source,
        commit,
        &PluginBuildHostEnv::from_process(),
        &forbidden,
    )?;
    if !allow_build {
        return Err(OrbitError::PluginBuildConsentRequired(format!(
            "plugin '{name}' builds from source at install time, which runs code from this \
             repository on this host. Nothing was built or installed. The build plan:\n{}\n\
             To consent to exactly this build, run:\n  orbit plugin add {} {PLUGIN_BUILD_CONSENT_FLAG}\n\
             (or `orbit plugin upgrade {name} {} {PLUGIN_BUILD_CONSENT_FLAG}` for an installed \
             plugin)",
            plan.render(),
            plan.source,
            plan.source
        )));
    }
    if let Some(reason) = unattended_reason(&global_root) {
        return Err(OrbitError::PluginBuildConsentUnavailable(format!(
            "{PLUGIN_BUILD_CONSENT_FLAG} was refused: {reason}. Only an operator at their own \
             command line can consent to a plugin build; nothing was built or installed"
        )));
    }
    if let Some(show_plan) = show_plan {
        show_plan(&format!(
            "Building plugin '{name}' with {PLUGIN_BUILD_CONSENT_FLAG}:\n{}",
            plan.render()
        ));
    }
    let log = build_log_path(&global_root, name);
    let home = std::env::var_os("HOME");
    let result = run_plugin_build(&PluginBuildRun {
        plan: &plan,
        checkout: &commit.checkout,
        namespace_dir: &plugin_namespace_dir(&global_root, name),
        log_path: &log,
        home: home.as_deref(),
    })?;
    Ok(Some(PreparedBuild { plan, result, log }))
}

/// Copy the outputs into `staging`, check them against the workspace pin,
/// and return the record the row and witness keep (§3.6, §3.7).
pub(super) fn finish_build(
    runtime: &OrbitRuntime,
    name: &str,
    prepared: PreparedBuild,
    staging: &Path,
) -> Result<PluginBuildRecord, OrbitError> {
    let PreparedBuild { plan, result, log } = prepared;
    let outputs = install_plugin_build_outputs(result.dir.path(), &plan.outputs, staging)?;
    let artifact_digest = plugin_artifact_digest(&outputs);
    if let Some(pinned) = pinned_artifact_digest(runtime, name)?
        && pinned != artifact_digest
    {
        return Err(OrbitError::PolicyDenied(format!(
            "plugin '{name}' built to artifact digest {artifact_digest}, but this workspace's \
             `.orbit/plugins.yaml` pins {pinned}. Nothing was installed. A build that is not \
             reproducible cannot satisfy an artifact_digest pin; use a reproducible build or a \
             prebuilt archive"
        )));
    }
    Ok(PluginBuildRecord {
        source: plan.source,
        commit: plan.commit,
        fetch: result.fetch,
        command: result.command,
        programs: plan
            .programs
            .iter()
            .map(|program| PluginBuildProgram {
                name: program.name.clone(),
                path: program.canonical.display().to_string(),
            })
            .collect(),
        toolchain_roots: plan
            .toolchain_roots
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        profile: result.profile,
        landlock_abi: result.landlock_abi,
        outputs,
        artifact_digest,
        consent: PluginBuildConsent {
            at: chrono::Utc::now().to_rfc3339(),
            os_user: ["USER", "LOGNAME"]
                .iter()
                .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
                .unwrap_or_else(|| "unknown".to_string()),
            orbit_version: env!("CARGO_PKG_VERSION").to_string(),
            flag: PLUGIN_BUILD_CONSENT_FLAG.to_string(),
        },
        log: log.display().to_string(),
    })
}

/// Why no operator can be consenting here, if so.
fn unattended_reason(global_root: &Path) -> Option<String> {
    if let Some(name) = UNATTENDED_ENV
        .iter()
        .find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        return Some(format!(
            "this process carries `{name}`, so it is a managed run or a plugin backend"
        ));
    }
    plugin_trees_masked(global_root)
        .then(|| "this process runs inside an Orbit agent sandbox".to_string())
}

/// The `artifact_digest` the current workspace pins `name` to, if any.
fn pinned_artifact_digest(
    runtime: &OrbitRuntime,
    name: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(pins) = read_pin_file(&runtime.shared_root())? else {
        return Ok(None);
    };
    let Some(pinned) = pins
        .plugins
        .into_iter()
        .find(|pin| pin.name == name)
        .and_then(|pin| pin.artifact_digest)
    else {
        return Ok(None);
    };
    // The pin file was validated on read, so the digest parses.
    let hex = parse_archive_digest(&pinned).map_err(OrbitError::InvalidInput)?;
    Ok(Some(format!("sha256:{hex}")))
}

/// `<global_root>/state/plugin-builds/<ns>/build.log`: the capped log of the
/// latest build, kept outside the install tree.
pub(crate) fn build_log_path(global_root: &Path, name: &str) -> PathBuf {
    global_root
        .join("state")
        .join("plugin-builds")
        .join(name)
        .join("build.log")
}
