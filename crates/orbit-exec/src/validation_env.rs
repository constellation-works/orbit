//! The environment owner-side repository tooling runs in [ORB-13987].
//!
//! Required validation (`workflow.required_validation_commands`) and
//! `local_shell` steps run repository checks — `make`, `cargo`, `rg`, `node`,
//! Homebrew tools, whatever a repository calls. Their PATH used to be whatever
//! launched the Orbit worker: a non-interactive ssh session, a systemd unit's
//! `Environment=PATH`, a launchd job's `/usr/bin:/bin:/usr/sbin:/sbin`. Agents
//! never saw the gap because they run commands through a login shell.
//!
//! [`ValidationEnvironment::resolve`] removes the launcher from the answer. It
//! starts from the caller's allowlisted child environment and, by default,
//! overlays the toolchain variables the owner user's *interactive login shell* exports —
//! [`LOGIN_SHELL_TOOLCHAIN_VARS`] only, never the rest of the login
//! environment, so the allowlist model still decides everything else. An
//! operator can prepend or replace PATH entries and disable the login-shell
//! probe. Every resolution records which [`ValidationEnvSource`] decided PATH.
//! Agent sessions receive the configured entries too
//! ([`ValidationEnvPolicy::agent_environment`]), so a reviewer's `python3` is
//! the one validation runs.
//!
//! The probe runs `<shell> -i -l -c "exec /bin/sh -c '<printer>'"`, reading
//! interactive rc files as well as profiles. Startup failure, nonzero exit,
//! timeout or a missing marker triggers a fallback to `<shell> -l -c …`.
//! `workflow.validation_env.interactive = false` uses only that login probe.
//! Each attempt has a bounded timeout and null stdin. The printer writes a
//! marker and then one NUL-terminated record per toolchain variable, ignoring
//! banners and rc output. Successful probes ignore stderr (including bash's
//! job-control warnings). The complete outcome, including probe mode and any
//! fallback reason, is cached per process for [`LOGIN_SHELL_CACHE_TTL`].

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

/// Variables a login shell contributes to the validation environment: PATH
/// and the locators toolchains on it need to find their own installation.
/// Everything else in the login environment stays behind.
pub const LOGIN_SHELL_TOOLCHAIN_VARS: &[&str] = &[
    "PATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "GOPATH",
    "GOROOT",
    "GOBIN",
    "JAVA_HOME",
    "PYENV_ROOT",
    "NVM_DIR",
    "VOLTA_HOME",
    "PNPM_HOME",
    "BUN_INSTALL",
    "HOMEBREW_PREFIX",
    "HOMEBREW_CELLAR",
    "HOMEBREW_REPOSITORY",
];

/// Ceiling for one login-shell probe. Profile files that take longer than this
/// are treated as a failed resolution, not waited on.
pub const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a resolved login environment is reused within one process.
pub const LOGIN_SHELL_CACHE_TTL: Duration = Duration::from_secs(120);

/// Precedes the printed records, so profile output before it is ignored.
const LOGIN_ENV_MARKER: &str = "__ORBIT_VALIDATION_LOGIN_ENV__";

/// Where the validation PATH came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationEnvSource {
    /// The owner user's login shell.
    LoginShell,
    /// `workflow.validation_env.path` (prepended or replacing).
    Config,
    /// Neither: the PATH of whatever launched this process.
    LauncherFallback,
}

impl ValidationEnvSource {
    /// Stable name recorded on runs and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LoginShell => "login_shell",
            Self::Config => "config",
            Self::LauncherFallback => "launcher_fallback",
        }
    }
}

/// The shell startup mode that produced the toolchain environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginShellMode {
    /// Interactive rc files and login profiles (`-i -l -c`).
    InteractiveLogin,
    /// Login profiles only (`-l -c`).
    Login,
}

impl LoginShellMode {
    /// Stable name recorded on runs and in doctor diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InteractiveLogin => "interactive_login",
            Self::Login => "login",
        }
    }
}

/// How `workflow.validation_env.path` combines with the resolved PATH.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ValidationPathMode {
    /// Configured entries come first, then the resolved PATH.
    #[default]
    Prepend,
    /// Configured entries are the whole PATH.
    Replace,
}

impl ValidationPathMode {
    /// Parse the configured spelling (`prepend` or `replace`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "prepend" => Some(Self::Prepend),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }

    /// The configured spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepend => "prepend",
            Self::Replace => "replace",
        }
    }
}

/// Operator policy for the validation environment
/// (`[workflow.validation_env]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationEnvPolicy {
    /// Resolve PATH and toolchain locators from the login shell.
    pub login_shell: bool,
    /// Try an interactive login shell before the login-only fallback.
    pub interactive: bool,
    /// Configured PATH entries; `~/` expands to the environment's `HOME`.
    pub path: Vec<String>,
    /// How [`Self::path`] combines with the resolved PATH.
    pub path_mode: ValidationPathMode,
}

impl Default for ValidationEnvPolicy {
    fn default() -> Self {
        Self {
            login_shell: true,
            interactive: true,
            path: Vec::new(),
            path_mode: ValidationPathMode::Prepend,
        }
    }
}

impl ValidationEnvPolicy {
    /// `env` with [`Self::path`] ahead of its PATH: the environment an agent
    /// session starts with, so the interpreter and tools a reviewer or
    /// implementer runs are the ones required validation runs with
    /// [ORB-15204]. The entries are prepended under either
    /// [`Self::path_mode`], because the rest of PATH still locates the
    /// provider CLI. The login-shell probe is not applied: an agent's shell
    /// reads the same profiles itself. A PATH that already starts with the
    /// entries (a nested Orbit process inside an agent) is kept as is.
    pub fn agent_environment(&self, mut env: Vec<(String, String)>) -> Vec<(String, String)> {
        let configured = self.expanded_path(lookup(&env, "HOME")).join(":");
        if configured.is_empty() {
            return env;
        }
        let path = match lookup(&env, "PATH") {
            Some(inherited)
                if inherited == configured || inherited.starts_with(&format!("{configured}:")) =>
            {
                return env;
            }
            Some(inherited) => format!("{configured}:{inherited}"),
            None => configured,
        };
        env.retain(|(name, _)| name != "PATH");
        env.push(("PATH".to_string(), path));
        env
    }

    /// The non-empty [`Self::path`] entries with `~` expanded against `home`.
    fn expanded_path(&self, home: Option<&str>) -> Vec<String> {
        self.path
            .iter()
            .map(|entry| entry.trim())
            .filter(|entry| !entry.is_empty())
            .map(|entry| expand_home(entry, home))
            .collect()
    }
}

/// Toolchain variables one login shell exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginShellEnv {
    /// Set toolchain variables, by name.
    pub vars: BTreeMap<String, String>,
    /// The successful probe's startup mode.
    pub mode: LoginShellMode,
    /// Why the interactive probe fell back to login-only resolution.
    pub fallback_reason: Option<String>,
}

impl LoginShellEnv {
    /// The login shell's PATH, when it exported one.
    pub fn path(&self) -> Option<&str> {
        self.vars.get("PATH").map(String::as_str)
    }
}

/// The owner user's login shell, and how long a probe of it may take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginShell {
    program: PathBuf,
    timeout: Duration,
}

impl LoginShell {
    /// A specific shell program, for hosts and tests that name one.
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
        }
    }

    /// The current user's login shell: the account database entry, else an
    /// absolute `SHELL` from `env`, else `/bin/sh`. The account entry comes
    /// first because it does not depend on what launched this process.
    pub fn for_current_user(env: &[(String, String)]) -> Self {
        let program = account_shell()
            .or_else(|| {
                lookup(env, "SHELL")
                    .map(PathBuf::from)
                    .filter(|shell| shell.is_absolute())
            })
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Self::new(program, LOGIN_SHELL_TIMEOUT)
    }

    /// The shell program this probe runs.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Probe interactively, falling back to login-only resolution. `base`
    /// supplies the identity, locale and starting PATH profiles build on.
    pub fn resolve(&self, base: &[(String, String)]) -> Result<LoginShellEnv, String> {
        self.resolve_with_interactive(base, true)
    }

    /// Probe now, optionally trying interactive startup before login-only.
    pub(crate) fn resolve_with_interactive(
        &self,
        base: &[(String, String)],
        interactive: bool,
    ) -> Result<LoginShellEnv, String> {
        let fallback_reason = if interactive {
            match self.probe(base, LoginShellMode::InteractiveLogin) {
                Ok(env) => return Ok(env),
                Err(reason) => Some(reason),
            }
        } else {
            None
        };
        match self.probe(base, LoginShellMode::Login) {
            Ok(mut env) => {
                env.fallback_reason = fallback_reason;
                Ok(env)
            }
            Err(error) => Err(match fallback_reason {
                Some(reason) => format!("{reason}; login fallback failed: {error}"),
                None => error,
            }),
        }
    }

    fn probe(
        &self,
        base: &[(String, String)],
        mode: LoginShellMode,
    ) -> Result<LoginShellEnv, String> {
        let shell = self.program.display().to_string();
        let label = match mode {
            LoginShellMode::InteractiveLogin => "interactive login shell",
            LoginShellMode::Login => "login shell",
        };
        let mut args = Vec::new();
        if mode == LoginShellMode::InteractiveLogin {
            args.push("-i".to_string());
        }
        args.extend([
            "-l".to_string(),
            "-c".to_string(),
            format!("exec /bin/sh -c '{}'", printer_script()),
        ]);
        let outcome = run_process(
            &ExecRequest {
                program: shell.clone(),
                args,
                current_dir: lookup(base, "HOME").map(ToOwned::to_owned),
                timeout_ms: Some(u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX)),
                stdin_mode: StdinMode::Null,
                environment_mode: EnvironmentMode::ClearAndSet(probe_environment(
                    base,
                    &self.program,
                )),
                debug: false,
            },
            &NoSandbox,
        )
        .map_err(|error| format!("{label} `{shell}` could not start: {error}"))?;
        if outcome.timed_out {
            return Err(format!(
                "{label} `{shell}` did not finish within {}ms",
                self.timeout.as_millis()
            ));
        }
        if !outcome.success {
            let stderr = outcome.stderr.trim();
            return Err(format!(
                "{label} `{shell}` exited with status {}{}",
                outcome
                    .exit_code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string()),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {}", bounded(stderr, 512))
                }
            ));
        }
        parse_login_env(&outcome.stdout)
            .map(|vars| LoginShellEnv {
                vars,
                mode,
                fallback_reason: None,
            })
            .ok_or_else(|| {
                format!("{label} `{shell}` did not print its environment (marker missing)")
            })
    }

    /// [`Self::resolve`], reused for [`LOGIN_SHELL_CACHE_TTL`] per shell,
    /// starting PATH, HOME, timeout and interactive policy. Failures and
    /// fallback results are cached too, costing at most two probes per window.
    pub fn resolve_cached(&self, base: &[(String, String)]) -> Result<LoginShellEnv, String> {
        self.resolve_cached_with_interactive(base, true)
    }

    /// Cached resolution with the configured interactive-startup policy.
    pub(crate) fn resolve_cached_with_interactive(
        &self,
        base: &[(String, String)],
        interactive: bool,
    ) -> Result<LoginShellEnv, String> {
        type Cache = Mutex<HashMap<String, (Instant, Result<LoginShellEnv, String>)>>;
        static CACHE: OnceLock<Cache> = OnceLock::new();
        let key = format!(
            "{}\0{}\0{}\0{}\0{}",
            self.program.display(),
            lookup(base, "PATH").unwrap_or_default(),
            lookup(base, "HOME").unwrap_or_default(),
            interactive,
            self.timeout.as_nanos()
        );
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(entries) = cache.lock()
            && let Some((at, result)) = entries.get(&key)
            && at.elapsed() < LOGIN_SHELL_CACHE_TTL
        {
            return result.clone();
        }
        // The probe runs without the lock held: a slow profile must not
        // serialize every other caller behind it.
        let result = self.resolve_with_interactive(base, interactive);
        if let Ok(mut entries) = cache.lock() {
            entries.retain(|_, (at, _)| at.elapsed() < LOGIN_SHELL_CACHE_TTL);
            entries.insert(key, (Instant::now(), result.clone()));
        }
        result
    }
}

/// One resolved validation environment and how it was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationEnvironment {
    /// The complete child environment.
    pub env: Vec<(String, String)>,
    /// What decided PATH.
    pub source: ValidationEnvSource,
    /// The login shell that was probed, when one was.
    pub login_shell: Option<PathBuf>,
    /// The login shell's PATH, when the probe succeeded.
    pub login_shell_path: Option<String>,
    /// Why the probe failed, when it did.
    pub login_shell_error: Option<String>,
    /// The startup mode that successfully produced the shell environment.
    pub probe_mode: Option<LoginShellMode>,
    /// Why interactive startup fell back to login-only resolution.
    pub fallback_reason: Option<String>,
    /// Whether login-shell resolution was enabled.
    pub login_shell_enabled: bool,
    /// Expanded `workflow.validation_env.path` entries, when configured.
    pub config_path: Vec<String>,
    /// How [`Self::config_path`] was applied.
    pub path_mode: ValidationPathMode,
}

impl ValidationEnvironment {
    /// `base` as is: what a host without a resolver runs commands with.
    pub fn launcher(base: Vec<(String, String)>) -> Self {
        Self {
            env: base,
            source: ValidationEnvSource::LauncherFallback,
            login_shell: None,
            login_shell_path: None,
            login_shell_error: None,
            probe_mode: None,
            fallback_reason: None,
            login_shell_enabled: false,
            config_path: Vec::new(),
            path_mode: ValidationPathMode::Prepend,
        }
    }

    /// Resolve the environment for `policy`, probing `shell` (cached) when
    /// login-shell resolution is enabled.
    pub fn resolve(
        base: Vec<(String, String)>,
        policy: &ValidationEnvPolicy,
        shell: &LoginShell,
    ) -> Self {
        let login = policy.login_shell.then(|| {
            shell
                .resolve_cached_with_interactive(&base, policy.interactive)
                .map(|env| (shell.program(), env))
        });
        Self::compose(base, policy, login)
    }

    /// Combine `base`, a login-shell probe result (`None` when disabled) and
    /// the configured PATH entries.
    pub fn compose(
        base: Vec<(String, String)>,
        policy: &ValidationEnvPolicy,
        login: Option<Result<(&Path, LoginShellEnv), String>>,
    ) -> Self {
        let mut env: BTreeMap<String, String> = base.into_iter().collect();
        let mut source = ValidationEnvSource::LauncherFallback;
        let mut login_shell = None;
        let mut login_shell_path = None;
        let mut login_shell_error = None;
        let mut probe_mode = None;
        let mut fallback_reason = None;
        match login {
            Some(Ok((program, resolved))) => {
                login_shell = Some(program.to_path_buf());
                probe_mode = Some(resolved.mode);
                fallback_reason = resolved.fallback_reason.clone();
                login_shell_path = resolved.path().map(ToOwned::to_owned);
                for name in LOGIN_SHELL_TOOLCHAIN_VARS {
                    if let Some(value) = resolved.vars.get(*name) {
                        env.insert((*name).to_string(), value.clone());
                    }
                }
                if login_shell_path.is_some() {
                    source = ValidationEnvSource::LoginShell;
                }
            }
            Some(Err(error)) => login_shell_error = Some(error),
            None => {}
        }
        let config_path = policy.expanded_path(env.get("HOME").map(String::as_str));
        if !config_path.is_empty() {
            let configured = config_path.join(":");
            let path = match (policy.path_mode, env.get("PATH")) {
                (ValidationPathMode::Prepend, Some(resolved)) if !resolved.is_empty() => {
                    format!("{configured}:{resolved}")
                }
                _ => configured,
            };
            env.insert("PATH".to_string(), path);
            source = ValidationEnvSource::Config;
        }
        Self {
            env: env.into_iter().collect(),
            source,
            login_shell,
            login_shell_path,
            login_shell_error,
            probe_mode,
            fallback_reason,
            login_shell_enabled: policy.login_shell,
            config_path,
            path_mode: policy.path_mode,
        }
    }

    /// The PATH commands run with, when one is set.
    pub fn path(&self) -> Option<&str> {
        lookup(&self.env, "PATH")
    }

    /// Executable candidates in PATH order. Duplicate or symlinked entries
    /// pointing to the same executable are reported once, so doctor does not
    /// mistake aliases such as `/bin` and `/usr/bin` for shadowed tools.
    pub fn program_paths(&self, program: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut targets = Vec::new();
        for entry in self.path().unwrap_or_default().split(':') {
            if entry.is_empty() {
                continue;
            }
            let candidate = Path::new(entry).join(program);
            if !is_executable(&candidate) {
                continue;
            }
            let target = candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.clone());
            if !targets.contains(&target) {
                targets.push(target);
                found.push(candidate);
            }
        }
        found
    }

    /// Login-shell PATH entries the resolved PATH does not contain. Empty
    /// when the probe did not run or succeed.
    pub fn missing_login_path_entries(&self) -> Vec<String> {
        let Some(login) = self.login_shell_path.as_deref() else {
            return Vec::new();
        };
        let resolved: Vec<&str> = self.path().unwrap_or_default().split(':').collect();
        let mut missing = Vec::new();
        for entry in login.split(':').filter(|entry| !entry.is_empty()) {
            if !resolved.contains(&entry) && !missing.iter().any(|seen| seen == entry) {
                missing.push(entry.to_string());
            }
        }
        missing
    }

    /// Why required validation may not find the user's toolchain, or `None`
    /// when the environment came from the login shell (or configuration) and
    /// keeps every login-shell PATH entry.
    pub fn preflight_warning(&self) -> Option<String> {
        let path = self.path().unwrap_or("<unset>");
        if let Some(error) = &self.login_shell_error {
            return Some(format!(
                "required validation could not resolve the login-shell environment ({error}); \
                 it runs with {} PATH={path} (source: {}). Fix the shell profile, or set \
                 `workflow.validation_env.path`.",
                if self.source == ValidationEnvSource::Config {
                    "the configured"
                } else {
                    "the launcher's"
                },
                self.source.as_str()
            ));
        }
        if !self.login_shell_enabled && self.source == ValidationEnvSource::LauncherFallback {
            return Some(format!(
                "login-shell resolution is disabled (`workflow.validation_env.login_shell = \
                 false`) and `workflow.validation_env.path` is empty, so required validation \
                 runs with whatever PATH launched the worker: PATH={path} (source: \
                 launcher_fallback)."
            ));
        }
        let missing = self.missing_login_path_entries();
        if !missing.is_empty() {
            return Some(format!(
                "the required-validation PATH (source: {}) lacks login-shell PATH entries: {}; \
                 commands found only there will fail. PATH={path}",
                self.source.as_str(),
                missing.join(", ")
            ));
        }
        None
    }
}

/// Whether `program` resolves to an executable file through `path`.
pub fn program_on_path(program: &str, path: &str) -> bool {
    if program.contains('/') {
        return is_executable(Path::new(program));
    }
    path.split(':')
        .filter(|entry| !entry.is_empty())
        .any(|entry| is_executable(&Path::new(entry).join(program)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The POSIX printer the login shell execs. It contains no single quote and
/// no backslash escape a shell would rewrite inside single quotes (fish
/// treats only `\\` and `\'` specially), so one quoting works for bash, zsh,
/// ksh, dash and fish.
fn printer_script() -> String {
    let mut script = format!("printf \"%s\\0\" {LOGIN_ENV_MARKER}");
    for name in LOGIN_SHELL_TOOLCHAIN_VARS {
        script.push_str(&format!(
            "; printf \"%s=%s=%s\\0\" {name} \"${{{name}+1}}\" \"${{{name}-}}\""
        ));
    }
    script
}

/// Read the printer's records: `NAME=<1 when set>=<value>`, after the marker.
fn parse_login_env(stdout: &str) -> Option<BTreeMap<String, String>> {
    let mut records = stdout.split('\0');
    records
        .by_ref()
        .find(|record| record.ends_with(LOGIN_ENV_MARKER))?;
    let mut vars = BTreeMap::new();
    for record in records {
        let Some((name, rest)) = record.split_once('=') else {
            continue;
        };
        let Some((set, value)) = rest.split_once('=') else {
            continue;
        };
        if set == "1" && LOGIN_SHELL_TOOLCHAIN_VARS.contains(&name) {
            vars.insert(name.to_string(), value.to_string());
        }
    }
    Some(vars)
}

/// The environment the probe's shell starts from: identity, locale and the
/// launcher PATH its profile files extend. `TERM=dumb` keeps profile files
/// from driving a terminal that is not there.
fn probe_environment(base: &[(String, String)], program: &Path) -> Vec<(String, String)> {
    let mut env: BTreeMap<String, String> = base
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "HOME" | "USER" | "LOGNAME" | "PATH" | "LANG" | "LC_ALL" | "TMPDIR" | "TZ"
            )
        })
        .cloned()
        .collect();
    if !env.contains_key("HOME")
        && let Some(home) = account_home()
    {
        env.insert("HOME".to_string(), home.display().to_string());
    }
    env.entry("PATH".to_string())
        .or_insert_with(|| "/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    env.insert("SHELL".to_string(), program.display().to_string());
    env.insert("TERM".to_string(), "dumb".to_string());
    env.into_iter().collect()
}

fn expand_home(entry: &str, home: Option<&str>) -> String {
    match (entry.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ if entry == "~" => home.unwrap_or(entry).to_string(),
        _ => entry.to_string(),
    }
}

fn lookup<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

fn bounded(text: &str, limit: usize) -> &str {
    &text[..orbit_common::text::floor_char_boundary(text, limit)]
}

#[cfg(unix)]
fn account_entry() -> Option<(PathBuf, PathBuf)> {
    use std::ffi::CStr;

    // SAFETY: getuid cannot fail. getpwuid_r writes into caller-owned buffers;
    // the strings are read only after a successful lookup with a non-null
    // result and copied out before either buffer is dropped.
    let uid = unsafe { libc::getuid() };
    let mut buf = vec![0 as libc::c_char; 1024];
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == 0 {
            if result.is_null() || pwd.pw_shell.is_null() || pwd.pw_dir.is_null() {
                return None;
            }
            let shell = unsafe { CStr::from_ptr(pwd.pw_shell) }.to_str().ok()?;
            let home = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_str().ok()?;
            return Some((PathBuf::from(shell), PathBuf::from(home)));
        }
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return None;
    }
}

#[cfg(not(unix))]
fn account_entry() -> Option<(PathBuf, PathBuf)> {
    None
}

fn account_shell() -> Option<PathBuf> {
    account_entry()
        .map(|(shell, _)| shell)
        .filter(|shell| shell.is_absolute() && is_executable(shell))
}

fn account_home() -> Option<PathBuf> {
    account_entry()
        .map(|(_, home)| home)
        .filter(|home| home.is_absolute())
}

#[cfg(all(test, unix))]
#[path = "tests/validation_env.rs"]
mod tests;
