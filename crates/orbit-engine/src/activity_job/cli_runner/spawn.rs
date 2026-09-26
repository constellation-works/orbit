use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use orbit_exec::{
    BwrapProbeOutcome, LinuxBwrapMountAuthority, LinuxBwrapPlan, LinuxBwrapPostRunGuard,
    LinuxBwrapSpawnRequest, MacosSandboxSpawnRequest, UnsatisfiedWriteGrant,
    compile_linux_bwrap_argv_with_authority, compile_macos_sandbox_profile,
    prepare_linux_bwrap_write_grants, probe_bwrap, sandbox_exec_available,
    sandbox_exec_unavailable_message, spawn_under_linux_bwrap, spawn_under_macos_sandbox,
};
use orbit_types::workflow::ExecutorSandboxKind;
use tempfile::NamedTempFile;

use super::super::dispatcher::ResolvedSandbox;

pub(super) const CODEX_CA_CERTIFICATE_ENV: &str = "CODEX_CA_CERTIFICATE";
pub(super) const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";
const DEFAULT_MACOS_CA_CERTIFICATE: &str = "/etc/ssl/cert.pem";

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

#[derive(Debug)]
pub(crate) struct SpawnedChild {
    pub(crate) child: Child,
    /// Sandbox profile tempfile, if any. Held until the supervisor returns
    /// so the kernel can keep reading the SBPL profile while the child runs.
    /// Shared: identical compiled profiles reuse one process-wide tempfile
    /// (see `orbit_exec::spawn_under_macos_sandbox`), so this may outlive
    /// this particular spawn.
    pub(crate) _profile_temp: Option<Arc<NamedTempFile>>,
    /// Linux mount-source descriptors retained until the provider exits.
    ///
    /// Closing a duplicate SQLite database descriptor can release this
    /// process's POSIX locks even while the host lease connection remains
    /// open, so the complete plan shares the sandboxed child's lifetime.
    pub(crate) _linux_mount_plan: Option<LinuxBwrapPlan>,
}

impl SpawnedChild {
    /// The post-run write-policy snapshot the Linux plan took while compiling
    /// its deny mounts — after grant preparation, before the child ran. Only
    /// a managed Bubblewrap launch carries one.
    pub(crate) fn take_linux_post_run_guard(&mut self) -> Option<LinuxBwrapPostRunGuard> {
        self._linux_mount_plan
            .as_mut()
            .and_then(LinuxBwrapPlan::take_post_run_guard)
    }
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
    #[cfg(target_os = "linux")]
    let child_result = orbit_common::test_process::retry_executable_busy(|| command.spawn());
    #[cfg(not(target_os = "linux"))]
    let child_result = command.spawn();
    let child = child_result.map_err(|err| SpawnError::from_spawn_io(program, &err))?;
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
        inherited_fds: &[],
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
