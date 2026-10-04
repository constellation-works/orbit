//! Install-time `spec.build`: the plan an operator consents to, the build
//! directory, both phases under the build profile, and the declared outputs
//! copied into an install's staging tree.
//!
//! Design: `docs/design/plugins/3_install_time_build.md`. Consent, the
//! refusal of unattended callers, the build record and its witness belong to
//! `orbit-core`; this module only runs what it is handed.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::security::redaction::credential_safe_location;
use orbit_exec::{
    BuildLog, BuildPhaseEnd, BuildPhaseNetwork, BuildPhaseRequest, BuildSandboxSpec,
    PLUGIN_BUILD_DIR_CAP_BYTES, PLUGIN_BUILD_FETCH_PORT, probe_build_sandbox, run_build_phase,
};
use orbit_types::plugin::{
    PluginBuildOutput, PluginBuildOutputRecord, PluginBuildSpec, artifact_digest_preimage,
    format_plugin_build_argv, render_build_argv,
};
use sha2::{Digest, Sha256};

use super::backend::resolve_declared_program;
use super::source::{MAX_UNPACKED_BYTES, ResolvedCommit};

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

    fn locator(&self, name: &str) -> Option<&str> {
        self.locators
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One `spec.build.programs` entry as resolved at consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBuildProgram {
    pub name: String,
    /// Where it was found: the path the phase executes, so a toolchain proxy
    /// that dispatches on its own name (`cargo` → rustup) keeps that name.
    pub invoked: PathBuf,
    /// The canonical executable, recorded and re-checked before each phase.
    pub canonical: PathBuf,
}

/// What a build will run, shown verbatim at consent (§3.8).
#[derive(Debug, Clone)]
pub struct PluginBuildPlan {
    /// The `git+` source, credentials removed.
    pub source: String,
    pub commit: String,
    pub committed_at: i64,
    pub fetch: Option<Vec<String>>,
    pub command: Vec<String>,
    pub programs: Vec<ResolvedBuildProgram>,
    pub toolchain_roots: Vec<PathBuf>,
    pub outputs: Vec<PluginBuildOutput>,
    pub timeout_ms: u64,
    /// `PATH` directories ahead of `/usr/bin:/bin`.
    path_dirs: Vec<PathBuf>,
    /// Locators the phases receive, already decided.
    locators: Vec<(String, String)>,
}

impl PluginBuildPlan {
    /// Every path the sandbox makes readable besides the system runtime.
    pub fn readable(&self) -> Vec<PathBuf> {
        let mut readable: BTreeSet<PathBuf> = BTreeSet::new();
        for program in &self.programs {
            readable.insert(program.invoked.clone());
            readable.insert(program.canonical.clone());
        }
        readable.extend(self.toolchain_roots.iter().cloned());
        readable.into_iter().collect()
    }

    /// The plan as the consent text prints it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "  source:   {:?}", self.source);
        let _ = writeln!(out, "  commit:   {}", self.commit);
        for program in &self.programs {
            let _ = writeln!(
                out,
                "  program:  {:?} -> {:?}",
                program.name,
                program.canonical.display()
            );
        }
        for root in &self.toolchain_roots {
            let _ = writeln!(out, "  toolchain root (read-only): {:?}", root.display());
        }
        if let Some(fetch) = &self.fetch {
            let _ = writeln!(
                out,
                "  phase fetch: {}  [network: outbound TCP to port {PLUGIN_BUILD_FETCH_PORT} only]",
                format_plugin_build_argv(fetch)
            );
        }
        let _ = writeln!(
            out,
            "  phase build: {}  [network: none]",
            format_plugin_build_argv(&self.command)
        );
        let _ = writeln!(
            out,
            "  limits:   {} s per phase; build directory {} GiB; outputs {} MiB",
            self.timeout_ms / 1000,
            PLUGIN_BUILD_DIR_CAP_BYTES / (1024 * 1024 * 1024),
            MAX_UNPACKED_BYTES / (1024 * 1024)
        );
        for output in &self.outputs {
            let _ = writeln!(out, "  output:   {:?} -> {:?}", output.from, output.to);
        }
        let _ = write!(
            out,
            "  writable: only the build directory; reads: system runtime, the programs and \
             toolchain roots above"
        );
        out
    }
}

fn quote_argv(argv: &[String]) -> String {
    format_plugin_build_argv(argv)
}

/// Resolve `spec`'s programs and toolchain roots for a build of `commit`.
///
/// `forbidden` are the paths no root may be or contain besides `$HOME` and
/// the credential paths: the global Orbit root and the workspace roots.
pub fn plan_plugin_build(
    spec: &PluginBuildSpec,
    source: &str,
    commit: &ResolvedCommit,
    env: &PluginBuildHostEnv,
    forbidden: &[PathBuf],
) -> Result<PluginBuildPlan, OrbitError> {
    let mut programs = Vec::new();
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
    let mut path_dirs: Vec<PathBuf> = Vec::new();
    let mut rustup_home: Option<PathBuf> = None;
    for name in &spec.programs {
        let canonical = resolve_declared_program(name, env.path.as_deref()).map_err(|reason| {
            OrbitError::InvalidInput(format!(
                "spec.build.programs: `{name}` did not resolve: {reason}; nothing was built"
            ))
        })?;
        let invoked = invoked_path(name, env.path.as_deref()).unwrap_or_else(|| canonical.clone());
        if let Some(dir) = invoked.parent()
            && !path_dirs.iter().any(|known| known == dir)
        {
            path_dirs.push(dir.to_path_buf());
        }
        match toolchain_for(&invoked, &canonical, env) {
            Toolchain::System => {}
            Toolchain::Prefix(prefix) => {
                roots.insert(prefix);
            }
            Toolchain::Rustup {
                home,
                toolchains,
                settings,
                default_bin,
            } => {
                roots.insert(toolchains);
                if let Some(settings) = settings {
                    roots.insert(settings);
                }
                if let Some(bin) = default_bin
                    && !path_dirs.contains(&bin)
                {
                    path_dirs.push(bin);
                }
                rustup_home = Some(home);
            }
        }
        programs.push(ResolvedBuildProgram {
            name: name.clone(),
            invoked,
            canonical,
        });
    }
    let roots: Vec<PathBuf> = roots.into_iter().collect();
    let mut checked: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    for program in &programs {
        checked.push(&program.invoked);
        checked.push(&program.canonical);
    }
    refuse_unsafe_roots(&checked, env, forbidden)?;

    let mut locators = Vec::new();
    if let Some(home) = &rustup_home {
        locators.push(("RUSTUP_HOME".to_string(), home.display().to_string()));
        if let Some(toolchain) = env.locator("RUSTUP_TOOLCHAIN")
            && !toolchain.contains('/')
        {
            locators.push(("RUSTUP_TOOLCHAIN".to_string(), toolchain.to_string()));
        }
    }
    for name in ["GOROOT", "JAVA_HOME"] {
        if let Some(value) = env.locator(name)
            && let Ok(canonical) = Path::new(value).canonicalize()
            && roots.iter().any(|root| canonical.starts_with(root))
        {
            locators.push((name.to_string(), canonical.display().to_string()));
        }
    }

    Ok(PluginBuildPlan {
        source: credential_safe_location(source),
        commit: commit.id.clone(),
        committed_at: commit.committed_at,
        fetch: spec.fetch.clone(),
        command: spec.command.clone(),
        programs,
        toolchain_roots: roots,
        outputs: spec.outputs.clone(),
        timeout_ms: spec.effective_timeout_ms(),
        path_dirs,
        locators,
    })
}

/// Where a bare name is found on `PATH`, before canonicalization.
fn invoked_path(name: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    if name.contains('/') {
        return Some(PathBuf::from(name));
    }
    std::env::split_paths(search_path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

enum Toolchain {
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
fn toolchain_for(invoked: &Path, canonical: &Path, env: &PluginBuildHostEnv) -> Toolchain {
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

/// A root or program path is never `$HOME` and never contains it; it is
/// never the global Orbit root, a workspace or a credential path, and is
/// neither inside nor around one of them (§3.2).
fn refuse_unsafe_roots(
    paths: &[&Path],
    env: &PluginBuildHostEnv,
    forbidden: &[PathBuf],
) -> Result<(), OrbitError> {
    // (path, what it is, whether a root inside it is refused too)
    let mut sensitive: Vec<(PathBuf, &str, bool)> = Vec::new();
    if let Some(home) = &env.home {
        sensitive.push((canonical_or_self(home), "your home directory", false));
    }
    for path in forbidden {
        sensitive.push((canonical_or_self(path), "an Orbit root or workspace", true));
    }
    for path in orbit_exec::default_credential_read_denies() {
        sensitive.push((canonical_or_self(&path), "a credential path", true));
    }
    for path in paths {
        for (sensitive_path, what, inside_refused) in &sensitive {
            let around = sensitive_path.starts_with(path);
            let inside = *inside_refused && path.starts_with(sensitive_path);
            if around || inside {
                let relation = if around {
                    "is or contains"
                } else {
                    "is inside"
                };
                return Err(OrbitError::PolicyDenied(format!(
                    "the build would read {}, which {relation} {what} ({}); a build toolchain \
                     must be installed in its own directory. Nothing was built",
                    path.display(),
                    sensitive_path.display()
                )));
            }
        }
    }
    Ok(())
}

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Which phase an environment is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginBuildPhase {
    Fetch,
    Build,
}

impl PluginBuildPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Build => "build",
        }
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

/// A build directory, removed when dropped.
#[derive(Debug)]
pub struct PluginBuildDir {
    path: PathBuf,
}

impl PluginBuildDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PluginBuildDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(
                target: "orbit.tools.plugin",
                path = %self.path.display(),
                "could not remove the plugin build directory: {error}",
            );
        }
    }
}

/// Whether a `.build-<pid>-<nonce>` directory belongs to a build that is
/// still running. Anything else under the prefix is a leftover.
pub fn is_live_plugin_build_dir(name: &OsStr) -> bool {
    let Some(pid) = name
        .to_str()
        .and_then(|name| name.strip_prefix(PLUGIN_BUILD_DIR_PREFIX))
        .and_then(|rest| rest.split_once('-'))
        .and_then(|(pid, _)| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    process_is_alive(pid)
}

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 checks for existence and permission; it sends nothing.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: i32) -> bool {
    false
}

/// What one build produced, ready to be copied into a staging tree.
#[derive(Debug)]
pub struct PluginBuildResult {
    pub dir: PluginBuildDir,
    pub profile: String,
    pub landlock_abi: Option<i64>,
    /// Each phase argv exactly as run.
    pub fetch: Option<Vec<String>>,
    pub command: Vec<String>,
}

/// Inputs to [`run_plugin_build`].
pub struct PluginBuildRun<'a> {
    pub plan: &'a PluginBuildPlan,
    /// The pristine checkout; copied, never handed to the sandbox.
    pub checkout: &'a Path,
    /// `~/.orbit/plugins/<ns>/`.
    pub namespace_dir: &'a Path,
    /// Where the capped log is written, whatever the outcome.
    pub log_path: &'a Path,
    pub home: Option<&'a OsStr>,
}

/// Create the build directory, run `fetch` (when declared) and `build`
/// under the build profile, and keep the log. Refuses before creating
/// anything when this host cannot apply the profile.
pub fn run_plugin_build(run: &PluginBuildRun<'_>) -> Result<PluginBuildResult, OrbitError> {
    let plan = run.plan;
    let probe = probe_build_sandbox(plan.fetch.is_some()).map_err(|reason| {
        OrbitError::PolicyDenied(format!(
            "this host cannot apply the plugin build profile: {reason}. There is no weaker \
             fallback; nothing was built"
        ))
    })?;
    for program in &plan.programs {
        let now = program.invoked.canonicalize().ok();
        if now.as_deref() != Some(program.canonical.as_path()) {
            return Err(OrbitError::PolicyDenied(format!(
                "build program `{}` no longer resolves to {} as shown at consent; nothing was built",
                program.name,
                program.canonical.display()
            )));
        }
    }
    let dir = create_build_dir(run.namespace_dir)?;
    let build_dir = dir.path().to_path_buf();
    copy_checkout(run.checkout, &build_dir.join("src"))?;

    let readable = plan.readable();
    let mut log = BuildLog::default();
    log.note(&format!(
        "build of {} at {} under {}",
        plan.source, plan.commit, probe.profile
    ));
    let build_dir_text = build_dir.display().to_string();
    let mut phases = Vec::new();
    if let Some(fetch) = &plan.fetch {
        phases.push((PluginBuildPhase::Fetch, fetch, BuildPhaseNetwork::Https));
    }
    phases.push((
        PluginBuildPhase::Build,
        &plan.command,
        BuildPhaseNetwork::None,
    ));
    let mut ran: Vec<Vec<String>> = Vec::new();
    let mut failure = None;
    for (phase, argv, network) in phases {
        let mut rendered = render_build_argv(argv, &build_dir_text);
        if let Some(first) = rendered.first_mut()
            && let Some(program) = plan.programs.iter().find(|program| program.name == *first)
        {
            *first = program.invoked.display().to_string();
        }
        log.note(&format!(
            "phase {}: {}",
            phase.name(),
            quote_argv(&rendered)
        ));
        let env = plugin_build_environment(plan, &build_dir, phase);
        let request = BuildPhaseRequest {
            sandbox: BuildSandboxSpec {
                build_dir: &build_dir,
                readable: &readable,
                home: run.home,
                network,
            },
            argv: &rendered,
            env: &env,
            cwd: &build_dir.join("src"),
            timeout: Duration::from_millis(plan.timeout_ms),
            build_dir_cap_bytes: PLUGIN_BUILD_DIR_CAP_BYTES,
        };
        let end = run_build_phase(&request, &mut log);
        ran.push(rendered);
        match end {
            Ok(end) if end.succeeded() => log.note(&format!("phase {} succeeded", phase.name())),
            Ok(end) => {
                let reason = phase_failure(end, plan.timeout_ms);
                log.note(&format!("phase {} failed: {reason}", phase.name()));
                failure = Some(format!("the {} phase {reason}", phase.name()));
                break;
            }
            Err(error) => {
                log.note(&format!("phase {} could not run: {error}", phase.name()));
                failure = Some(format!("the {} phase could not run: {error}", phase.name()));
                break;
            }
        }
    }
    write_log(run.log_path, &log)?;
    if let Some(failure) = failure {
        return Err(OrbitError::Execution(format!(
            "plugin build failed: {failure}; log: {}. Nothing was installed",
            run.log_path.display()
        )));
    }
    let command = ran.pop().unwrap_or_default();
    Ok(PluginBuildResult {
        dir,
        profile: probe.profile.to_string(),
        landlock_abi: probe.landlock_abi,
        fetch: ran.pop(),
        command,
    })
}

fn phase_failure(end: BuildPhaseEnd, timeout_ms: u64) -> String {
    match end {
        BuildPhaseEnd::Exited(code) => format!("exited with status {code}"),
        BuildPhaseEnd::Signaled(signal) => format!("was killed by signal {signal}"),
        BuildPhaseEnd::TimedOut => format!("ran past its {} s timeout", timeout_ms / 1000),
        BuildPhaseEnd::BuildDirCapExceeded => format!(
            "outgrew the {} GiB build directory cap",
            PLUGIN_BUILD_DIR_CAP_BYTES / (1024 * 1024 * 1024)
        ),
    }
}

fn write_log(path: &Path, log: &BuildLog) -> Result<(), OrbitError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
    }
    std::fs::write(path, log.render())
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}

/// `<namespace_dir>/.build-<pid>-<nonce>/` with `src/`, `home/`, `tmp/`,
/// mode 0700, created one component at a time without following a link.
fn create_build_dir(namespace_dir: &Path) -> Result<PluginBuildDir, OrbitError> {
    std::fs::create_dir_all(namespace_dir)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", namespace_dir.display())))?;
    let namespace_dir = namespace_dir
        .canonicalize()
        .map_err(|error| OrbitError::Io(format!("resolve {}: {error}", namespace_dir.display())))?;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce)
        .map_err(|error| OrbitError::Io(format!("build directory nonce: {error}")))?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let path = namespace_dir.join(format!(
        "{PLUGIN_BUILD_DIR_PREFIX}{}-{nonce}",
        std::process::id()
    ));
    private_dir(&path)?;
    let dir = PluginBuildDir { path };
    for child in ["src", "home", "tmp"] {
        private_dir(&dir.path.join(child))?;
    }
    Ok(dir)
}

/// `mkdir` that fails on an existing path, so a planted link is never used.
fn private_dir(path: &Path) -> Result<(), OrbitError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", path.display())))
}

/// Copy the pristine checkout into the build's `src/`: regular files with
/// their permission bits, directories, and symbolic links as links. `.git`
/// was already removed; anything else (sockets, devices) is skipped.
fn copy_checkout(source: &Path, target: &Path) -> Result<(), OrbitError> {
    let io = |what: &str, path: &Path, error: std::io::Error| {
        OrbitError::Io(format!("{what} {}: {error}", path.display()))
    };
    for entry in std::fs::read_dir(source).map_err(|error| io("read", source, error))? {
        let entry = entry.map_err(|error| io("read", source, error))?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| io("stat", &path, error))?;
        if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let link = std::fs::read_link(&path).map_err(|error| io("read", &path, error))?;
                std::os::unix::fs::symlink(link, &destination)
                    .map_err(|error| io("create", &destination, error))?;
            }
        } else if file_type.is_dir() {
            std::fs::create_dir(&destination).map_err(|error| io("create", &destination, error))?;
            copy_checkout(&path, &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(&path, &destination).map_err(|error| io("copy", &path, error))?;
        }
    }
    Ok(())
}

/// Copy each declared output from the build directory into `staging` (the
/// install's copy of the pristine plugin root) and record it (§3.4, §3.6).
///
/// `from` must be a physical regular file inside the build directory with
/// one link and no link anywhere on its path; `to` must not name a file the
/// pristine root holds. The installed mode keeps only the owner-execute bit
/// as `0755` or `0644`, so the digest does not depend on a host's umask.
/// The hash is taken from the staged copy, the bytes that are installed.
pub fn install_plugin_build_outputs(
    build_dir: &Path,
    outputs: &[PluginBuildOutput],
    staging: &Path,
) -> Result<Vec<PluginBuildOutputRecord>, OrbitError> {
    let mut total: u64 = 0;
    let mut records = Vec::new();
    for output in outputs {
        let from = physical_output(build_dir, &output.from)?;
        let to = staging.join(&output.to);
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(OrbitError::PolicyDenied(format!(
                "build output '{}' would replace a file the reviewed plugin tree ships; nothing \
                 was installed",
                output.to
            )));
        }
        let metadata = std::fs::symlink_metadata(&from)
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", from.display())))?;
        total = total.saturating_add(metadata.len());
        if total > MAX_UNPACKED_BYTES {
            return Err(OrbitError::PolicyDenied(format!(
                "the build outputs exceed {} MiB together; nothing was installed",
                MAX_UNPACKED_BYTES / (1024 * 1024)
            )));
        }
        if let Some(parent) = to.parent() {
            refuse_linked_parent(staging, parent)?;
            std::fs::create_dir_all(parent)
                .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
        }
        std::fs::copy(&from, &to)
            .map_err(|error| OrbitError::Io(format!("copy {}: {error}", from.display())))?;
        let mode = installed_mode(&metadata);
        set_mode(&to, mode)?;
        records.push(PluginBuildOutputRecord {
            to: output.to.clone(),
            mode,
            sha256: file_sha256(&to)?,
        });
    }
    Ok(records)
}

fn physical_output(build_dir: &Path, relative: &str) -> Result<PathBuf, OrbitError> {
    let refusal = |reason: &str| {
        OrbitError::PolicyDenied(format!(
            "build output '{relative}' {reason}; nothing was installed"
        ))
    };
    let mut path = build_dir.to_path_buf();
    let components: Vec<Component<'_>> = Path::new(relative).components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(refusal("is not a plain relative path"));
        };
        path.push(name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| refusal("was not produced by the build"))?;
        let last = index + 1 == components.len();
        if metadata.file_type().is_symlink() {
            return Err(refusal("is or passes through a symbolic link"));
        }
        if !last && !metadata.is_dir() {
            return Err(refusal("passes through something that is not a directory"));
        }
        if last {
            if !metadata.is_file() {
                return Err(refusal("is not a regular file"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(refusal("is a hard link"));
                }
            }
        }
    }
    Ok(path)
}

fn refuse_linked_parent(staging: &Path, parent: &Path) -> Result<(), OrbitError> {
    let Ok(relative) = parent.strip_prefix(staging) else {
        return Err(OrbitError::PolicyDenied(
            "a build output must stay inside the plugin root".to_string(),
        ));
    };
    let mut path = staging.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(OrbitError::PolicyDenied(format!(
                    "build output directory {} is not a directory in the plugin tree",
                    path.display()
                )));
            }
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(unix)]
fn installed_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o100 != 0 {
        0o755
    } else {
        0o644
    }
}

#[cfg(not(unix))]
fn installed_mode(_metadata: &std::fs::Metadata) -> u32 {
    0o644
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| OrbitError::Io(format!("chmod {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), OrbitError> {
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, OrbitError> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| OrbitError::Io(format!("open {}: {error}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// `sha256:<hex>` over `outputs` (§3.6).
pub fn plugin_artifact_digest(outputs: &[PluginBuildOutputRecord]) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(artifact_digest_preimage(outputs).as_bytes())
    )
}

/// The artifact digest of the outputs as installed under `install_path` now,
/// with their current modes: what doctor compares to the record. `Err`
/// names the output that cannot be read.
pub fn installed_artifact_digest(
    install_path: &Path,
    recorded: &[PluginBuildOutputRecord],
) -> Result<String, String> {
    let mut current = Vec::new();
    for output in recorded {
        let path = install_path.join(&output.to);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("output '{}' is unreadable: {error}", output.to))?;
        if !metadata.is_file() {
            return Err(format!(
                "output '{}' is no longer a regular file",
                output.to
            ));
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o7777
        };
        #[cfg(not(unix))]
        let mode = output.mode;
        current.push(PluginBuildOutputRecord {
            to: output.to.clone(),
            mode,
            sha256: file_sha256(&path).map_err(|error| error.to_string())?,
        });
    }
    Ok(plugin_artifact_digest(&current))
}

/// Whether every declared output is already a regular file in `root`: the
/// requirement for a manifest with `spec.build` installed from a source that
/// never builds (§3.1).
pub fn prebuilt_outputs_present(root: &Path, outputs: &[PluginBuildOutput]) -> Result<(), String> {
    for output in outputs {
        let path = root.join(&output.to);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            _ => return Err(output.to.clone()),
        }
    }
    Ok(())
}
