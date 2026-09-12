use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_exec::{
    BwrapProbeOutcome, LinuxBwrapMountAuthority, LinuxBwrapPlan, LinuxBwrapSpawnRequest,
    MacosLoginKeychainAccess, MacosSandboxSpawnRequest, UnsatisfiedWriteGrant,
    compile_linux_bwrap_argv_with_authority, compile_macos_sandbox_profile,
    linux_bwrap_write_grant_diagnostic, macos_login_keychain_access,
    prepare_linux_bwrap_write_grants, probe_bwrap, sandbox_exec_available,
    sandbox_exec_unavailable_message, spawn_under_linux_bwrap, spawn_under_macos_sandbox,
};
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::Provider;
use tempfile::NamedTempFile;

use super::super::dispatcher::ResolvedSandbox;

const ORBIT_BIN_ENV: &str = "ORBIT_BIN";
pub(super) const CODEX_CA_CERTIFICATE_ENV: &str = "CODEX_CA_CERTIFICATE";
pub(super) const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";
const DEFAULT_MACOS_CA_CERTIFICATE: &str = "/etc/ssl/cert.pem";

/// Conventional `$HOME` bin directories searched when a provider launcher
/// is not on the inherited `PATH`, and backfilled into a spawned agent's
/// `PATH` when those entries are absent. Keep this list shared so lookup
/// and child-env construction cannot drift. [ORB-10909]
const CONVENTIONAL_HOME_BIN_DIRS: &[&str] = &[".local/bin", ".orbit/bin", ".cargo/bin", "bin"];

/// Portable supported-launcher prefixes searched after `PATH` and `$HOME`
/// bins, and backfilled into the spawned agent `PATH`. These are OS package
/// prefixes, not user directories: Apple Silicon Homebrew, then the Intel
/// Homebrew / `/usr/local` prefix. launchd's default `PATH` omits both, which
/// is why a scheduled Mac drain cannot find `/opt/homebrew/bin/codex` while
/// an interactive shell can. [ORB-11808]
pub(crate) const SUPPORTED_SYSTEM_BIN_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Typed spawn failure with a retryability classification (ORB-10006).
///
/// `permanent: true` marks failures that retrying cannot fix — the step
/// retry wrapper fails fast on them instead of burning attempts. Only
/// clearly-deterministic failures are classified permanent (executable
/// missing, permission denied, sandbox profile rejected); everything else
/// stays transient so the step-level retry keeps its pre-ORB-10006 reach.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct SpawnError {
    pub(crate) permanent: bool,
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxDispatchMetadata {
    pub(crate) backend: Option<String>,
    pub(crate) trusted_wrapper: Option<String>,
    pub(crate) probe_outcome: Option<String>,
    pub(crate) write_enforcement: String,
    pub(crate) read_enforcement: String,
}

pub(crate) struct PreparedSandbox<'a> {
    pub(crate) effective: Option<&'a ResolvedSandbox>,
    pub(crate) metadata: SandboxDispatchMetadata,
}

impl PreparedSandbox<'_> {
    /// The deliberate absence of a sandbox for an operator-admitted
    /// trusted-host invocation [ORB-11354].
    ///
    /// Named distinctly from the `None` arm of
    /// [`prepare_sandbox_for_dispatch`] — which means "this executor declares
    /// no sandbox" — so a run trail distinguishes an executor that never had
    /// one from an operator who explicitly removed it.
    pub(crate) fn none_trusted_host() -> Self {
        Self {
            effective: None,
            metadata: SandboxDispatchMetadata {
                backend: Some("none-trusted-host".to_string()),
                trusted_wrapper: None,
                probe_outcome: None,
                write_enforcement: "write_unrestricted_trusted_host".to_string(),
                read_enforcement: "read_unrestricted_trusted_host".to_string(),
            },
        }
    }
}

/// Resolve availability before provider argv construction. This ordering is
/// security-sensitive: provider-native flags are neutralized only when the
/// outer wrapper is actually usable, while an explicitly allowed bare
/// fallback keeps those flags intact. Explicit off disables both layers.
pub(crate) fn prepare_sandbox_for_dispatch(
    sandbox: Option<&ResolvedSandbox>,
) -> Result<PreparedSandbox<'_>, SpawnError> {
    match sandbox {
        Some(sandbox) if sandbox.kind == ExecutorSandboxKind::Off => Ok(PreparedSandbox {
            effective: None,
            metadata: SandboxDispatchMetadata {
                backend: Some("off".to_string()),
                trusted_wrapper: None,
                probe_outcome: None,
                write_enforcement: "write_unrestricted".to_string(),
                read_enforcement: "read_unrestricted".to_string(),
            },
        }),
        Some(sandbox) if sandbox.kind == ExecutorSandboxKind::LinuxBwrap => {
            let probe = probe_bwrap();
            prepare_linux_sandbox_for_dispatch_with_probe(sandbox, probe)
        }
        Some(sandbox) => Ok(PreparedSandbox {
            effective: Some(sandbox),
            metadata: SandboxDispatchMetadata {
                backend: Some(sandbox.kind.as_str().to_string()),
                trusted_wrapper: None,
                probe_outcome: None,
                write_enforcement: "write_enforced".to_string(),
                read_enforcement: "read_delegated".to_string(),
            },
        }),
        None => Ok(PreparedSandbox {
            effective: None,
            metadata: SandboxDispatchMetadata {
                backend: None,
                trusted_wrapper: None,
                probe_outcome: None,
                write_enforcement: "write_delegated".to_string(),
                read_enforcement: "read_delegated".to_string(),
            },
        }),
    }
}

pub(crate) fn prepare_linux_sandbox_for_dispatch_with_probe<'a>(
    sandbox: &'a ResolvedSandbox,
    probe: BwrapProbeOutcome,
) -> Result<PreparedSandbox<'a>, SpawnError> {
    if probe.available {
        Ok(PreparedSandbox {
            effective: Some(sandbox),
            metadata: SandboxDispatchMetadata {
                backend: Some("linux-bwrap".to_string()),
                trusted_wrapper: Some(probe.trusted_path),
                probe_outcome: Some(probe.detail),
                write_enforcement: "write_enforced".to_string(),
                read_enforcement: "read_delegated".to_string(),
            },
        })
    } else if sandbox.allow_fallback {
        tracing::warn!(
            target: "orbit.engine.cli_runner",
            reason = %probe.detail,
            "linux-bwrap unavailable; falling back to bare exec because executor declares allow_fallback"
        );
        Ok(PreparedSandbox {
            effective: None,
            metadata: SandboxDispatchMetadata {
                backend: Some("bare-fallback".to_string()),
                trusted_wrapper: Some(probe.trusted_path),
                probe_outcome: Some(probe.detail),
                write_enforcement: "write_delegated".to_string(),
                read_enforcement: "read_delegated".to_string(),
            },
        })
    } else {
        Err(SpawnError::permanent(format!(
            "{}; declare allow_fallback: true to permit bare exec",
            probe.detail
        )))
    }
}

impl SpawnError {
    pub(crate) fn transient(message: String) -> Self {
        Self {
            permanent: false,
            message,
        }
    }

    pub(crate) fn permanent(message: String) -> Self {
        Self {
            permanent: true,
            message,
        }
    }

    /// Classify an OS spawn error. `NotFound` (ENOENT) and
    /// `PermissionDenied` (EACCES) are deterministic; resource-exhaustion
    /// signals (EAGAIN, ENOMEM, EMFILE, ENFILE, ...) and anything
    /// unrecognized stay transient — conservative in the direction of
    /// preserving retries.
    pub(crate) fn from_spawn_io(program: &str, err: &std::io::Error) -> Self {
        let message = format!("failed to spawn `{program}`: {err}");
        match err.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                Self::permanent(message)
            }
            _ => Self::transient(message),
        }
    }
}

/// Resolve a provider launcher without relying solely on the parent process's
/// ambient `PATH`.
///
/// Every CLI-backed provider enters through this resolver so service, routine,
/// dashboard, and interactive dispatches use the same lookup policy.
pub(crate) fn resolve_provider_launcher(
    provider: &str,
    program: &str,
    cwd: Option<&Path>,
) -> Result<String, SpawnError> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_provider_launcher_with(provider, program, path.as_deref(), home.as_deref(), cwd)
}

/// Pin tools invoked by an agent to the Orbit build that dispatched it.
///
/// Long-lived services may retain a `PATH` whose first `orbit` is an older
/// Cargo install even after the operator deploys `~/.orbit/bin/orbit`. Export
/// the selected binary for hook scripts and put its directory first for bare
/// `orbit tool ...` invocations inside the provider (including Bubblewrap).
pub(crate) fn orbit_tool_env() -> Result<Vec<(String, String)>, SpawnError> {
    let current_exe = std::env::current_exe().map_err(|error| {
        SpawnError::permanent(format!(
            "resolve dispatching Orbit executable for agent tool environment: {error}"
        ))
    })?;
    let configured = std::env::var_os(ORBIT_BIN_ENV);
    let inherited_path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    orbit_tool_env_with(
        configured.as_deref(),
        &current_exe,
        inherited_path.as_deref(),
        home.as_deref(),
    )
}

// pub(crate) widened for sibling tests under the repository's enforced test layout.
pub(crate) fn orbit_tool_env_with(
    configured: Option<&OsStr>,
    current_exe: &Path,
    inherited_path: Option<&OsStr>,
    home: Option<&Path>,
) -> Result<Vec<(String, String)>, SpawnError> {
    let selected = configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| current_exe.to_path_buf());
    let selected_text = selected.to_string_lossy().into_owned();

    let Some(bin_dir) = selected
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(vec![(ORBIT_BIN_ENV.to_string(), selected_text)]);
    };

    let mut path_entries = vec![bin_dir.to_path_buf()];
    let mut seen = HashSet::new();
    seen.insert(bin_dir.to_path_buf());
    if let Some(inherited_path) = inherited_path {
        for entry in std::env::split_paths(inherited_path) {
            if seen.insert(entry.clone()) {
                path_entries.push(entry);
            }
        }
    }
    // Service/routine dispatch often inherits a PATH that omits cargo and
    // other login-shell bins. Backfill the same conventional home dirs and
    // supported system prefixes `resolve_provider_launcher_with` already
    // searches so `cargo test` and Homebrew-installed tools inside the
    // spawned agent are not "command not found". [ORB-10909] [ORB-11808]
    if let Some(home) = home {
        for dir in conventional_home_bin_dirs(home) {
            if seen.insert(dir.clone()) {
                path_entries.push(dir);
            }
        }
    }
    for dir in supported_system_bin_dirs() {
        if seen.insert(dir.clone()) {
            path_entries.push(dir);
        }
    }
    let pinned_path = std::env::join_paths(path_entries)
        .map_err(|error| {
            SpawnError::permanent(format!(
                "construct agent PATH pinned to `{}`: {error}",
                selected.display()
            ))
        })?
        .into_string()
        .map_err(|_| {
            SpawnError::permanent(format!(
                "agent PATH pinned to `{}` is not valid Unicode",
                selected.display()
            ))
        })?;

    Ok(vec![
        (ORBIT_BIN_ENV.to_string(), selected_text),
        ("PATH".to_string(), pinned_path),
    ])
}

fn conventional_home_bin_dirs(home: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    CONVENTIONAL_HOME_BIN_DIRS
        .iter()
        .map(|relative| home.join(relative))
}

fn supported_system_bin_dirs() -> impl Iterator<Item = PathBuf> {
    SUPPORTED_SYSTEM_BIN_DIRS.iter().map(PathBuf::from)
}

fn is_launchable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// pub(crate) widened for sibling tests under the repository's enforced test layout.
pub(crate) fn resolve_provider_launcher_with(
    provider: &str,
    program: &str,
    path: Option<&OsStr>,
    home: Option<&Path>,
    cwd: Option<&Path>,
) -> Result<String, SpawnError> {
    resolve_provider_launcher_with_extra_dirs(
        provider,
        program,
        path,
        home,
        cwd,
        supported_system_bin_dirs(),
    )
}

/// Test-injectable resolver: production calls
/// [`resolve_provider_launcher_with`], which supplies the supported system
/// prefixes. Tests pass a temporary Homebrew-style prefix rather than
/// writing into `/opt/homebrew/bin`.
pub(crate) fn resolve_provider_launcher_with_extra_dirs(
    provider: &str,
    program: &str,
    path: Option<&OsStr>,
    home: Option<&Path>,
    cwd: Option<&Path>,
    extra_bin_dirs: impl IntoIterator<Item = PathBuf>,
) -> Result<String, SpawnError> {
    let configured = Path::new(program);
    if configured.components().count() > 1 {
        return Ok(program.to_string());
    }

    let mut search_dirs = Vec::new();
    let mut seen = HashSet::new();
    if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            let dir = if dir.is_relative() {
                cwd.map_or(dir.clone(), |cwd| cwd.join(&dir))
            } else {
                dir
            };
            if seen.insert(dir.clone()) {
                search_dirs.push(dir);
            }
        }
    }
    if let Some(home) = home {
        for dir in conventional_home_bin_dirs(home) {
            if seen.insert(dir.clone()) {
                search_dirs.push(dir);
            }
        }
    }
    for dir in extra_bin_dirs {
        if seen.insert(dir.clone()) {
            search_dirs.push(dir);
        }
    }

    let mut searched = Vec::with_capacity(search_dirs.len());
    for dir in search_dirs {
        let candidate = dir.join(program);
        searched.push(candidate.clone());
        if is_launchable_file(&candidate) {
            return Ok(candidate.to_string_lossy().into_owned());
        }
        #[cfg(windows)]
        if configured.extension().is_none() {
            for extension in windows_executable_extensions() {
                let candidate = dir.join(format!("{program}{extension}"));
                searched.push(candidate.clone());
                if is_launchable_file(&candidate) {
                    return Ok(candidate.to_string_lossy().into_owned());
                }
            }
        }
    }

    let searched = if searched.is_empty() {
        "<no PATH or HOME search locations available>".to_string()
    } else {
        searched
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(SpawnError::permanent(format!(
        "provider launcher `{program}` for provider `{provider}` was not found; searched: {searched}"
    )))
}

#[cfg(windows)]
fn windows_executable_extensions() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[derive(Debug)]
pub(crate) struct SpawnedChild {
    pub(crate) child: Child,
    /// Sandbox profile tempfile, if any. Held until the supervisor returns
    /// so the kernel can keep reading the SBPL profile while the child runs.
    pub(crate) _profile_temp: Option<NamedTempFile>,
    /// Linux mount-source descriptors retained until the provider exits.
    ///
    /// Closing a duplicate SQLite database descriptor can release this
    /// process's POSIX locks even while the host lease connection remains
    /// open, so the complete plan shares the sandboxed child's lifetime.
    pub(crate) _linux_mount_plan: Option<LinuxBwrapPlan>,
}

pub(crate) fn spawn_child_with_optional_sandbox(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&Path>,
    sandbox: Option<&ResolvedSandbox>,
    provider: &str,
) -> Result<SpawnedChild, SpawnError> {
    match sandbox {
        Some(sb) if sb.kind == ExecutorSandboxKind::MacosSandboxExec => {
            spawn_macos_sandboxed(program, args, env, cwd, sb, provider)
        }
        Some(sb) if sb.kind == ExecutorSandboxKind::LinuxBwrap => {
            spawn_linux_bwrap(program, args, env, cwd, sb)
        }
        Some(sb) => Err(SpawnError::permanent(format!(
            "unsupported sandbox backend `{}`",
            sb.kind
        ))),
        None => spawn_bare(program, args, env, cwd),
    }
}

/// Materialize the profile's narrow write grants, then compile argv.
///
/// Preparation happens here, at every spawn, rather than once during worktree
/// setup: this is the only layer that sees the *effective* profile — policy
/// rules absolutized against the subprocess cwd plus the host-appended run
/// roots — so it is the only layer whose grant set cannot drift from what the
/// kernel will enforce. Re-deriving per spawn is also what lets a run whose
/// needs grow mid-run pick up anchors on its next provider launch.
///
/// Anchors are only created inside the managed worktree, which is trusted and
/// disposable. Creating one grants nothing new: the effective profile already
/// decided the path is writable.
fn spawn_linux_bwrap(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&Path>,
    sandbox: &ResolvedSandbox,
) -> Result<SpawnedChild, SpawnError> {
    if let Some(worktree) = sandbox.managed_worktree.then_some(cwd).flatten() {
        let prepared = prepare_linux_bwrap_write_grants(&sandbox.fs_profile, worktree)
            .map_err(|error| SpawnError::permanent(error.to_string()))?;
        if !prepared.created.is_empty() {
            tracing::info!(
                target: "orbit.engine.cli_runner",
                anchors = ?prepared.created,
                "materialized policy-granted sandbox write anchors before launch"
            );
        }
        report_unsatisfied_grants(&prepared.unsatisfied);
    }
    let authority = linux_bwrap_mount_authority(sandbox);
    let plan = compile_linux_bwrap_argv_with_authority(
        &sandbox.fs_profile,
        program,
        args,
        cwd,
        sandbox.managed_worktree,
        authority,
    )
    .map_err(|error| SpawnError::permanent(error.to_string()))?;
    reject_unsatisfiable_managed_grants(sandbox.managed_worktree, &plan.dropped_grants)?;
    report_unsatisfied_grants(&plan.dropped_grants);
    let child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env,
        cwd,
        stdin: Stdio::piped(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .map_err(|error| SpawnError::transient(error.to_string()))?;
    Ok(SpawnedChild {
        child,
        _profile_temp: None,
        _linux_mount_plan: Some(plan),
    })
}

/// Borrow the runtime owner's exact descriptors for the mount plan. `File`
/// duplication is forbidden here because closing any duplicate for a SQLite
/// database can release unrelated POSIX locks owned by this process.
pub(crate) fn linux_bwrap_mount_authority(
    sandbox: &ResolvedSandbox,
) -> Vec<LinuxBwrapMountAuthority> {
    sandbox
        .runtime_write_authority
        .iter()
        .map(|grant| LinuxBwrapMountAuthority {
            destination: grant.path.clone(),
            source: Arc::clone(&grant.handle),
        })
        .collect()
}

/// Inside a managed worktree, preparation should have satisfied every grant.
/// Anything still unmountable is a defect in the grant set, and failing here —
/// before the provider starts — keeps the denial attributable to a path and a
/// rule instead of surfacing as an EROFS mid-turn.
///
/// Split out of the spawn path so the rejection is provable without launching
/// Bubblewrap: the guarantee this check carries is that an unsatisfiable grant
/// can never reach the provider, and a test that has to spawn a real sandbox to
/// observe it would silently skip on any host without bwrap.
// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn reject_unsatisfiable_managed_grants(
    managed_worktree: bool,
    dropped_grants: &[UnsatisfiedWriteGrant],
) -> Result<(), SpawnError> {
    if !managed_worktree || dropped_grants.is_empty() {
        return Ok(());
    }
    Err(SpawnError::permanent(format!(
        "linux-bwrap could not apply {} policy write grant(s): {}",
        dropped_grants.len(),
        describe_grants(dropped_grants)
    )))
}

/// Host-owned anchors outside the managed worktree are the host's to create,
/// so a miss there is reported rather than fatal — but never silently.
fn report_unsatisfied_grants(grants: &[UnsatisfiedWriteGrant]) {
    if grants.is_empty() {
        return;
    }
    tracing::warn!(
        target: "orbit.engine.cli_runner",
        detail = %describe_grants(grants),
        "policy grants a sandbox write path that could not be mounted"
    );
}

fn describe_grants(grants: &[UnsatisfiedWriteGrant]) -> String {
    grants
        .iter()
        .map(UnsatisfiedWriteGrant::describe)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Turn a child-reported EROFS into a policy-owned denial when the failing
/// program included the attempted path in stderr. This runs after the real
/// Bubblewrap child exits, so it covers the production invocation boundary
/// rather than merely explaining a path supplied by a unit test.
pub(super) fn linux_bwrap_failed_write_diagnostic(
    profile: &ResolvedFsProfile,
    stderr: &[u8],
    cwd: Option<&Path>,
) -> Result<Option<String>, OrbitError> {
    let stderr = String::from_utf8_lossy(stderr);
    for line in stderr.lines().rev() {
        if !line.contains("Read-only file system") && !line.contains("EROFS") {
            continue;
        }
        for candidate in failed_write_path_candidates(line).into_iter().rev() {
            let path = Path::new(&candidate);
            let attempted = if path.is_absolute() {
                path.to_path_buf()
            } else if let Some(cwd) = cwd {
                cwd.join(path)
            } else {
                continue;
            };
            if let Some(diagnostic) = linux_bwrap_write_grant_diagnostic(profile, &attempted)? {
                return Ok(Some(format!(
                    "Orbit linux-bwrap policy denied the attempted write: {diagnostic}"
                )));
            }
        }
    }
    Ok(None)
}

fn failed_write_path_candidates(line: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for quote in ['\'', '"', '`'] {
        let mut remainder = line;
        while let Some(start) = remainder.find(quote) {
            let after_start = &remainder[start + quote.len_utf8()..];
            let Some(end) = after_start.find(quote) else {
                break;
            };
            let candidate = after_start[..end].trim();
            if !candidate.is_empty() {
                candidates.push(candidate.to_string());
            }
            remainder = &after_start[end + quote.len_utf8()..];
        }
    }

    // Coreutils quotes paths, but language runtimes often render
    // `...: /path: Read-only file system`. Keep a conservative token fallback
    // so those failures are attributable too.
    let prefix = line
        .split_once("Read-only file system")
        .or_else(|| line.split_once("EROFS"))
        .map_or(line, |(prefix, _)| prefix);
    if let Some(token) = prefix.split_whitespace().next_back() {
        let candidate = token
            .trim_matches(|character: char| {
                matches!(character, '\'' | '"' | '`' | ':' | '(' | ')' | '[' | ']')
            })
            .trim();
        if !candidate.is_empty() && !candidates.iter().any(|known| known == candidate) {
            candidates.push(candidate.to_string());
        }
    }
    candidates
}

/// Text a provider CLI emits when it cannot read its Keychain-backed login.
/// The CLI cannot tell "the item is gone" from "the item is unreadable", so
/// each vendor reports both as a login failure in its own wording.
fn keychain_auth_failure_marker(provider: &str) -> Option<&'static str> {
    match Provider::parse(provider).ok()? {
        Provider::Claude => Some("OAuth session expired"),
        Provider::Copilot => Some("No authentication information found"),
        // Matches both the documented quoted form (`run 'agent login' first`)
        // and the live cursor-agent 2026.09.10 wording (`run agent login first`).
        Provider::Cursor => Some("Authentication required. Please run"),
        _ => None,
    }
}

/// Turn Copilot's rejected `--model` error into crew-scoped remediation.
///
/// The CLI's stderr names the unavailable id but does not identify the Orbit
/// configuration that supplied it. The resolved crew is still present in the
/// activity input, so join those facts before the generic exit-code path drops
/// the actionable context.
pub(super) fn copilot_model_unavailable_diagnostic(
    provider: &str,
    crew: &str,
    output: &str,
) -> Option<String> {
    if Provider::parse(provider).ok()? != Provider::Copilot || crew.trim().is_empty() {
        return None;
    }

    let (_, after_prefix) = output.split_once("Model \"")?;
    let (model, _) = after_prefix.split_once("\" from --model flag is not available")?;
    if model.is_empty() {
        return None;
    }

    Some(format!(
        "Copilot rejected model `{model}` supplied by crew `{crew}`. Start `copilot` and enter \
         `/model` to list the ids available to this account, then update \
         `crews.{crew}.model` and retry."
    ))
}

/// Distinguish a sandbox Keychain denial from a genuinely expired provider
/// login.
///
/// A macOS sandbox that hides `$HOME/Library/Keychains` makes a valid login
/// look expired, and the CLI's own message sends the operator to re-login —
/// which cannot help. Orbit compiled the profile, so it is the only layer that
/// knows whether the credential was actually reachable; say so next to the
/// provider's message instead of leaving the operator to guess.
///
/// The verdict comes from `orbit_exec::macos_login_keychain_access`, which
/// reads the same profile the kernel enforced. Only the `Allowed` case may
/// recommend re-authentication: the other cases are Orbit's own denial, which
/// no amount of re-logging in outside the sandbox will clear. [ORB-10931]
/// [ORB-12261]
pub(super) fn macos_keychain_auth_diagnostic(
    provider: &str,
    sandbox: Option<&ResolvedSandbox>,
    output: &str,
) -> Option<String> {
    let home = std::env::var_os("HOME");
    macos_keychain_auth_diagnostic_with(provider, sandbox, output, home.as_deref())
}

/// Test-friendly variant: callers pass HOME explicitly instead of reading
/// process-global state, which the compiler's carve-out also depends on.
// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn macos_keychain_auth_diagnostic_with(
    provider: &str,
    sandbox: Option<&ResolvedSandbox>,
    output: &str,
    home: Option<&OsStr>,
) -> Option<String> {
    let sandbox = sandbox?;
    let marker = keychain_auth_failure_marker(provider)?;
    if sandbox.kind != ExecutorSandboxKind::MacosSandboxExec || !output.contains(marker) {
        return None;
    }
    match macos_login_keychain_access(provider, home, &sandbox.fs_profile) {
        // The provider does not keep credentials in the keychain, so this
        // failure has nothing to do with the sandbox's keychain deny.
        MacosLoginKeychainAccess::DeniedByDefaultPolicy => None,
        MacosLoginKeychainAccess::Allowed => Some(format!(
            "Orbit's macOS sandbox profile allows `$HOME/Library/Keychains` reads for provider \
             `{provider}`, so the stored credential was reachable and this is a real login \
             failure: re-authenticate the provider CLI outside Orbit, then retry."
        )),
        // The compiler emits the re-allow only when it can resolve HOME, so an
        // Orbit process started without a login environment still produces the
        // fake-expiry failure. That is a different fix from re-authenticating.
        MacosLoginKeychainAccess::HomeUnresolved => Some(format!(
            "HOME is unset for the Orbit process, so its macOS sandbox profile could not allow \
             `$HOME/Library/Keychains` reads for provider `{provider}`: the sandbox hid the \
             stored credential and the login is not necessarily expired. Start Orbit with a \
             login environment and retry before re-authenticating."
        )),
        MacosLoginKeychainAccess::DeniedByActivityRule { rule } => Some(format!(
            "The activity's fsProfile `{}` denies `$HOME/Library/Keychains` reads via rule \
             `{rule}`, which outranks the `{provider}` credential carve-out, so Orbit's macOS \
             sandbox hid the stored credential and the login is not necessarily expired. \
             Re-authenticating will not help: drop or narrow that denyRead rule, or run this \
             activity without the macOS sandbox.",
            sandbox.fs_profile.name
        )),
    }
}

// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn spawn_bare(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&Path>,
) -> Result<SpawnedChild, SpawnError> {
    let mut command = Command::new(program);
    command
        .args(args)
        // `env` is the complete child environment, composed from the
        // `[execution.env]` allowlist by the dispatcher. Nothing ambient is
        // added here: seeding a cleared environment from the parent is what
        // let benignly named credentials reach the provider. [ORB-10917]
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = cwd {
        command.current_dir(path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|err| SpawnError::from_spawn_io(program, &err))?;
    Ok(SpawnedChild {
        child,
        _profile_temp: None,
        _linux_mount_plan: None,
    })
}

fn spawn_macos_sandboxed(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&Path>,
    sandbox: &ResolvedSandbox,
    provider: &str,
) -> Result<SpawnedChild, SpawnError> {
    spawn_macos_sandboxed_with(
        program,
        args,
        env,
        cwd,
        sandbox,
        provider,
        sandbox_exec_available(),
    )
}

/// Test-friendly variant of [`spawn_macos_sandboxed`]: callers pass an
/// explicit availability flag instead of probing the trusted wrapper. Production
/// routes through the public wrapper which resolves the trusted absolute path; tests
/// can assert the fail-closed and fallback branches without mutating
/// process-global state.
// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn spawn_macos_sandboxed_with(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&Path>,
    sandbox: &ResolvedSandbox,
    provider: &str,
    sandbox_exec_present: bool,
) -> Result<SpawnedChild, SpawnError> {
    if !sandbox_exec_present {
        let unavailable = sandbox_exec_unavailable_message();
        if sandbox.allow_fallback {
            tracing::warn!(
                target: "orbit.engine.cli_runner",
                program = program,
                "{unavailable}; falling back to bare exec because executor declares allow_fallback"
            );
            return spawn_bare(program, args, env, cwd);
        }
        // A missing trusted sandbox-exec binary won't appear between retry
        // attempts — deterministic environment failure.
        return Err(SpawnError::permanent(format!(
            "{unavailable}; declare allow_fallback: true to permit bare exec"
        )));
    }

    // SBPL compilation happens at spawn time so the orbit-exec dependency
    // stays scoped to this crate. The host returns only a descriptor
    // (`fs_profile` + `kind` + `allow_fallback`) so orbit-core has no
    // direct edge to orbit-exec.
    //
    // A profile that fails to compile is deterministic config — permanent.
    // The sandboxed spawn itself goes through orbit-exec, which erases the
    // io::ErrorKind; classify it transient so retries are preserved.
    //
    // `provider` reaches the compiler because the credential denylist has a
    // provider-scoped exception: the confined CLI's own credential store.
    // See `orbit_exec::macos_login_keychain_access`. [ORB-10929] [ORB-12261]
    let profile_text = compile_macos_sandbox_profile(&sandbox.fs_profile, provider)
        .map_err(|err| SpawnError::permanent(err.to_string()))?;
    let child_env = prepare_macos_codex_ca_environment_with(
        provider,
        env,
        cwd,
        Path::new(DEFAULT_MACOS_CA_CERTIFICATE),
    )?;
    let (child, profile_temp) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: &profile_text,
        program,
        args,
        env: &child_env,
        cwd,
        stdin: Stdio::piped(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .map_err(|err| SpawnError::transient(err.to_string()))?;
    Ok(SpawnedChild {
        child,
        _profile_temp: Some(profile_temp),
        _linux_mount_plan: None,
    })
}

/// Select the CA bundle seen by Codex under Orbit's macOS sandbox.
///
/// Denying the system Keychain directories is intentional, but it prevents
/// rustls-native-certs from completing native root discovery. Codex supports
/// file-backed trust through `CODEX_CA_CERTIFICATE`, so Orbit supplies macOS's
/// public bundle when the operator has not selected either documented
/// override. Explicit values keep their normal precedence and are validated,
/// never replaced with the fallback after a typo or permissions failure.
pub(crate) fn prepare_macos_codex_ca_environment_with(
    provider: &str,
    env: &[(String, String)],
    cwd: Option<&Path>,
    default_ca_certificate: &Path,
) -> Result<Vec<(String, String)>, SpawnError> {
    if provider != "codex" {
        return Ok(env.to_vec());
    }

    let selected = [CODEX_CA_CERTIFICATE_ENV, SSL_CERT_FILE_ENV]
        .into_iter()
        .find_map(|name| {
            env.iter()
                .rev()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.as_str())
                .filter(|value| !value.is_empty())
                .map(|value| (name, value))
        });
    if let Some((name, value)) = selected {
        validate_ca_certificate_path(
            name,
            value,
            cwd,
            &format!(
                "set {name} to a readable PEM CA bundle or unset it so Orbit can use {DEFAULT_MACOS_CA_CERTIFICATE}"
            ),
        )?;
        return Ok(env.to_vec());
    }

    let value = default_ca_certificate.to_string_lossy().into_owned();
    validate_ca_certificate_path(
        CODEX_CA_CERTIFICATE_ENV,
        &value,
        cwd,
        "make the macOS public CA bundle readable or set CODEX_CA_CERTIFICATE or SSL_CERT_FILE to a readable PEM CA bundle",
    )?;

    let mut child_env = env.to_vec();
    child_env.push((CODEX_CA_CERTIFICATE_ENV.to_string(), value));
    Ok(child_env)
}

fn validate_ca_certificate_path(
    variable: &str,
    value: &str,
    cwd: Option<&Path>,
    recovery: &str,
) -> Result<(), SpawnError> {
    let configured = Path::new(value);
    let resolved = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        let base = match cwd {
            Some(cwd) => cwd.to_path_buf(),
            None => std::env::current_dir().map_err(|error| {
                SpawnError::permanent(format!(
                    "cannot resolve relative {variable} value `{value}` for sandboxed Codex: {error}"
                ))
            })?,
        };
        base.join(configured)
    };
    let metadata = std::fs::metadata(&resolved).map_err(|error| {
        SpawnError::permanent(format!(
            "{variable} selects CA bundle `{}` for sandboxed Codex, but Orbit cannot read it: {error}; {recovery}",
            resolved.display(),
        ))
    })?;
    if !metadata.is_file() {
        return Err(SpawnError::permanent(format!(
            "{variable} selects `{}` for sandboxed Codex, but it is not a CA bundle file; {recovery}",
            resolved.display()
        )));
    }
    std::fs::File::open(&resolved).map_err(|error| {
        SpawnError::permanent(format!(
            "{variable} selects CA bundle `{}` for sandboxed Codex, but Orbit cannot open it: {error}; {recovery}",
            resolved.display()
        ))
    })?;

    Ok(())
}
