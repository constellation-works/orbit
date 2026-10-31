//! `linux-bwrap-script-v1`: the profile a granted script step runs under
//! (`docs/design/script-steps/1_scope.md` §3).
//!
//! It is the build profile's constructed root with a different set of
//! binds: the host runtime table and the grant's recorded interpreters and
//! read roots, read-only; the cwd tree (a run worktree read-write, or the
//! workspace root read-only) with its `.git` read-only and its `.orbit/`
//! masked; the grant's write roots; and the step's scratch directory at
//! [`SCRIPT_SANDBOX_MOUNT`], whose `snapshot/` (the manifest and entry
//! bytes the grant was checked against) and `in/` are read-only. The
//! credential denies are applied after every bind, so they win over all of
//! them. Every namespace is unshared, the network included: under
//! `network: none` the child has only its own loopback interface, never
//! the host's. The environment is cleared inside the sandbox and set from
//! the request alone.
//!
//! Supervision is the build phase's: one capped log for stdout and stderr,
//! a wall-clock bound and a scratch-size bound that kill the process group.
//! Bubblewrap's PID namespace and `--die-with-parent` mean that killing it
//! kills every descendant, so nothing outlives the step.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::OrbitError;

use super::PLUGIN_BUILD_LOG_CAP_BYTES;
use super::linux::{display, push_credential_denies, push_ro_bind, push_runtime_table};

/// Where the step's scratch directory appears inside the sandbox.
pub const SCRIPT_SANDBOX_MOUNT: &str = "/orbit-script";

/// The most stdout a script's answer may be (§4).
pub const SCRIPT_STDOUT_CAP_BYTES: usize =
    orbit_types::workflow::activity_job::SCRIPT_STDOUT_CAP_BYTES;

/// The capped log of one attempt: the build log's cap (§4).
pub const SCRIPT_LOG_CAP_BYTES: usize = PLUGIN_BUILD_LOG_CAP_BYTES;

/// How long a timed-out script gets between `SIGTERM` and `SIGKILL` (§3).
pub const SCRIPT_TERMINATION_GRACE: Duration = Duration::from_secs(10);

/// The scratch subdirectories, created by the caller before the run.
pub const SCRIPT_SCRATCH_DIRS: [&str; 5] = ["snapshot", "home", "tmp", "in", "out"];

/// What the script profile lets one script see.
#[derive(Debug, Clone, Copy)]
pub struct ScriptSandboxSpec<'a> {
    /// The cwd tree: absolute and physical.
    pub cwd: &'a Path,
    /// A run worktree is writable; the workspace root is not.
    pub cwd_writable: bool,
    /// The step's scratch directory on the host, holding
    /// [`SCRIPT_SCRATCH_DIRS`]; mounted at [`SCRIPT_SANDBOX_MOUNT`].
    pub scratch: &'a Path,
    /// Recorded interpreter paths and granted read roots: read-only. A root
    /// that overlaps a masked tree enclosing the cwd is refused, as a write
    /// root is.
    pub readable: &'a [PathBuf],
    /// Granted write roots: read-write.
    pub writable: &'a [PathBuf],
    /// Trees that must not exist for the script (the workspace's `.orbit/`,
    /// the global root). A mask that would hide the cwd itself is skipped:
    /// such a tree is not mounted in the first place.
    pub masked: &'a [PathBuf],
    /// The operator's `HOME`, whose credential trees stay denied.
    pub home: Option<&'a OsStr>,
}

/// One script to run under the profile.
pub struct ScriptSandboxRequest<'a> {
    pub sandbox: ScriptSandboxSpec<'a>,
    /// The argv inside the sandbox. `argv[0]` is an absolute path.
    pub argv: &'a [String],
    /// The script's complete environment, set inside the sandbox.
    pub env: &'a [(String, String)],
    /// A launcher the wrapper runs under (a per-script systemd scope), or
    /// empty. It must exec the wrapper in place.
    pub launcher: &'a [String],
    /// The environment of the launcher and wrapper processes; it never
    /// reaches the script.
    pub launcher_env: &'a [(String, String)],
    /// The validated step input, written to stdin and closed.
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub scratch_cap_bytes: u64,
}

/// How a script run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRunEnd {
    Exited(i32),
    Signaled(i32),
    /// The timeout expired; the sandbox was killed.
    TimedOut,
    /// The scratch directory outgrew its cap; the sandbox was killed.
    ScratchCapExceeded,
}

/// What one script run produced.
#[derive(Debug, Clone)]
pub struct ScriptSandboxRun {
    pub end: ScriptRunEnd,
    /// Stdout up to [`SCRIPT_STDOUT_CAP_BYTES`].
    pub stdout: Vec<u8>,
    pub stdout_over_cap: bool,
    /// Stdout and stderr interleaved, capped at [`SCRIPT_LOG_CAP_BYTES`].
    /// Not redacted: the caller redacts before storing it.
    pub log: Vec<u8>,
    pub duration: Duration,
}

/// The Bubblewrap argv (without the wrapper itself) for one script.
///
/// Reads the host filesystem to decide which runtime paths exist, and to
/// find the cwd tree's Git directories.
pub fn compile_linux_script_argv(
    spec: &ScriptSandboxSpec<'_>,
    env: &[(String, String)],
    argv: &[String],
) -> Result<Vec<String>, OrbitError> {
    // An enclosing mask cannot be mounted without hiding the cwd. Reject a
    // grant that would expose that otherwise-unmounted tree, including aliases.
    // Read roots are bound wholesale too, so they get the same refusal; only a
    // root inside the cwd tree, which is mounted anyway, stays allowed.
    let cwd_physical = spec.cwd.canonicalize().ok();
    for root in spec.readable {
        let Ok(root) = root.canonicalize() else {
            // A path that does not resolve is not bound either.
            continue;
        };
        if cwd_physical
            .as_deref()
            .is_some_and(|cwd| root.starts_with(cwd))
        {
            continue;
        }
        if overlaps_enclosing_mask(spec, &root) {
            return Err(OrbitError::PolicyDenied(format!(
                "script read root {} overlaps protected state",
                root.display()
            )));
        }
    }
    let mut writable = Vec::with_capacity(spec.writable.len());
    for root in spec.writable {
        let root = root.canonicalize().map_err(|error| {
            OrbitError::InvalidInput(format!(
                "cannot resolve script write root {}: {error}",
                root.display()
            ))
        })?;
        if overlaps_enclosing_mask(spec, &root) {
            return Err(OrbitError::PolicyDenied(format!(
                "script write root {} overlaps protected state",
                root.display()
            )));
        }
        writable.push(root);
    }
    // `--unshare-all` includes the network namespace: with no
    // `--share-net`, the only interface is the namespace's own loopback.
    let mut args: Vec<String> = vec![
        "--unshare-all".into(),
        "--new-session".into(),
        "--die-with-parent".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
    ];
    push_runtime_table(&mut args, spec.readable);
    let cwd_bind = if spec.cwd_writable {
        "--bind"
    } else {
        "--ro-bind"
    };
    args.extend([cwd_bind.into(), display(spec.cwd), display(spec.cwd)]);
    for root in &writable {
        args.extend(["--bind".into(), display(root), display(root)]);
    }
    let mount = Path::new(SCRIPT_SANDBOX_MOUNT);
    args.extend([
        "--bind".into(),
        display(spec.scratch),
        SCRIPT_SANDBOX_MOUNT.into(),
    ]);
    for read_only in ["snapshot", "in"] {
        args.extend([
            "--ro-bind".into(),
            display(&spec.scratch.join(read_only)),
            display(&mount.join(read_only)),
        ]);
    }
    // Git metadata is readable so `git describe` and `git archive` work,
    // and never writable, even inside a writable worktree.
    for git_dir in cwd_git_dirs(spec.cwd) {
        push_ro_bind(&mut args, &git_dir);
    }
    for masked in spec.masked {
        let Ok(masked) = masked.canonicalize() else {
            continue;
        };
        if spec.cwd.starts_with(&masked) || !masked.is_dir() {
            continue;
        }
        args.extend([
            "--tmpfs".into(),
            display(&masked),
            "--remount-ro".into(),
            display(&masked),
        ]);
    }
    push_credential_denies(&mut args, spec.home);
    args.extend([
        "--remount-ro".into(),
        "/".into(),
        "--chdir".into(),
        display(spec.cwd),
        "--clearenv".into(),
    ]);
    for (key, value) in env {
        args.extend(["--setenv".into(), key.clone(), value.clone()]);
    }
    args.push("--".into());
    args.extend(argv.iter().cloned());
    Ok(args)
}

/// Whether a canonical grant root is, contains or lies within a masked tree
/// that encloses the cwd. Such a tree is not mounted, so it cannot be masked.
fn overlaps_enclosing_mask(spec: &ScriptSandboxSpec<'_>, root: &Path) -> bool {
    spec.masked.iter().any(|masked| {
        masked.canonicalize().is_ok_and(|masked| {
            spec.cwd.starts_with(&masked) && (root.starts_with(&masked) || masked.starts_with(root))
        })
    })
}

/// The Git directories a cwd tree reads: its `.git` directory, or for a
/// linked worktree the `gitdir:` its `.git` file names and that gitdir's
/// common directory. Anything that does not resolve is left out; a script
/// that needs Git then fails on its own terms.
fn cwd_git_dirs(cwd: &Path) -> Vec<PathBuf> {
    let dot_git = cwd.join(".git");
    let Ok(metadata) = std::fs::metadata(&dot_git) else {
        return Vec::new();
    };
    if metadata.is_dir() {
        return vec![dot_git];
    }
    let Ok(pointer) = std::fs::read_to_string(&dot_git) else {
        return Vec::new();
    };
    let Some(git_dir) = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .map(|path| cwd.join(path))
        .and_then(|path| path.canonicalize().ok())
    else {
        return Vec::new();
    };
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .and_then(|relative| git_dir.join(relative.trim()).canonicalize().ok());
    let mut dirs = vec![dot_git];
    // The common directory is skipped only when a collected directory already
    // contains it. It goes before the per-worktree gitdir so the gitdir mounts
    // over it.
    if let Some(common) = common
        && !dirs.iter().any(|dir| common.starts_with(dir))
    {
        dirs.push(common);
    }
    dirs.push(git_dir);
    dirs
}

/// Whether this host can apply the script profile now. The error is the
/// `script_sandbox_unavailable` reason: a platform without the profile, a
/// start nested inside another sandbox, or Bubblewrap without the
/// namespaces it needs. There is no weaker fallback.
pub fn probe_script_sandbox() -> Result<&'static str, String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(reason) = nested_sandbox_reason() {
            return Err(reason);
        }
        let bwrap = crate::probe_bwrap();
        if !bwrap.available {
            return Err(format!(
                "the {} profile needs Bubblewrap with unprivileged user namespaces: {}",
                orbit_types::workflow::activity_job::SCRIPT_PROFILE_LINUX,
                bwrap.detail
            ));
        }
        Ok(orbit_types::workflow::activity_job::SCRIPT_PROFILE_LINUX)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(
            "script steps run only on Linux in this build; this platform has no script profile"
                .to_string(),
        )
    }
}

/// Why this process is already inside a sandbox, if it is. Bubblewrap
/// with a PID namespace (Orbit's agent profile, the build profile, this
/// profile) runs as PID 1 of the namespace, and Flatpak marks its root.
/// The script never runs under the outer profile instead.
#[cfg(target_os = "linux")]
fn nested_sandbox_reason() -> Option<String> {
    let init = std::fs::read_to_string("/proc/1/comm").unwrap_or_default();
    if init.trim() == "bwrap" {
        return Some(
            "this process already runs inside a Bubblewrap sandbox (PID 1 of its PID namespace \
             is bwrap); a script step does not start nested inside another sandbox"
                .to_string(),
        );
    }
    if Path::new("/.flatpak-info").exists() {
        return Some(
            "this process runs inside a Flatpak sandbox; a script step does not start nested \
             inside another sandbox"
                .to_string(),
        );
    }
    None
}

/// Run one script under the profile. The caller probes first
/// ([`probe_script_sandbox`]); a profile that cannot be applied here is an
/// error, never a weaker run.
pub fn run_script_sandboxed(
    request: ScriptSandboxRequest<'_>,
) -> Result<ScriptSandboxRun, OrbitError> {
    let spec = &request.sandbox;
    if !spec.cwd.is_absolute() || !spec.scratch.is_absolute() {
        return Err(OrbitError::InvalidInput(
            "a script run needs an absolute cwd and scratch directory".to_string(),
        ));
    }
    let Some(program) = request.argv.first() else {
        return Err(OrbitError::InvalidInput(
            "a script run needs a program".to_string(),
        ));
    };
    if !Path::new(program).is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "script program `{program}` must be an absolute path"
        )));
    }
    for directory in SCRIPT_SCRATCH_DIRS {
        if !spec.scratch.join(directory).is_dir() {
            return Err(OrbitError::InvalidInput(format!(
                "script scratch directory {} has no `{directory}/`",
                spec.scratch.display()
            )));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let bwrap = crate::bwrap_path().ok_or_else(|| {
            OrbitError::PolicyDenied(format!(
                "trusted Bubblewrap not available at {}",
                crate::bwrap_program_for_audit()
            ))
        })?;
        let wrapper_args = compile_linux_script_argv(spec, request.env, request.argv)?;
        let mut command = match request.launcher.split_first() {
            Some((launcher, launcher_args)) => {
                let mut command = std::process::Command::new(launcher);
                command.args(launcher_args).arg(&bwrap);
                command
            }
            None => std::process::Command::new(&bwrap),
        };
        command.args(wrapper_args);
        let phase = super::supervise::Phase {
            env: request.launcher_env,
            cwd: spec.cwd,
            timeout: request.timeout,
            size_root: spec.scratch,
            size_cap: request.scratch_cap_bytes,
            stdin: Some(request.stdin),
            stdout_cap: Some(SCRIPT_STDOUT_CAP_BYTES),
            grace: SCRIPT_TERMINATION_GRACE,
        };
        let mut log = super::supervise::BuildLog::with_cap(SCRIPT_LOG_CAP_BYTES);
        let started = std::time::Instant::now();
        let run = super::supervise::run_phase(command, (), &phase, &mut log)?;
        Ok(ScriptSandboxRun {
            end: match run.end {
                super::BuildPhaseEnd::Exited(code) => ScriptRunEnd::Exited(code),
                super::BuildPhaseEnd::Signaled(signal) => ScriptRunEnd::Signaled(signal),
                super::BuildPhaseEnd::TimedOut => ScriptRunEnd::TimedOut,
                super::BuildPhaseEnd::BuildDirCapExceeded => ScriptRunEnd::ScratchCapExceeded,
            },
            stdout: run.stdout,
            stdout_over_cap: run.stdout_over_cap,
            log: log.render(),
            duration: started.elapsed(),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = request;
        Err(OrbitError::PolicyDenied(
            "script steps run only on Linux in this build".to_string(),
        ))
    }
}
