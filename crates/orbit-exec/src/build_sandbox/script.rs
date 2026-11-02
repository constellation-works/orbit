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
//! them. Every namespace is unshared. Under `network: none` the network
//! namespace is unshared too, so the child has only its own loopback
//! interface, never the host's. A networked grant shares the host network:
//! `https` under a Landlock ruleset that admits outbound TCP to one port
//! only (the build profile's fetch mode), `any` with no rule. The
//! environment is cleared inside the sandbox and set from the request
//! alone.
//!
//! Supervision is the build phase's: one capped log for stdout and stderr,
//! a wall-clock bound and a scratch-size bound that kill the process group,
//! and `RLIMIT_CPU` and `RLIMIT_NOFILE` set on the wrapper before it execs.
//! Bubblewrap's PID namespace and `--die-with-parent` mean that killing it
//! kills every descendant, so nothing outlives the step.
//!
//! macOS runs the same request under `macos-sandbox-script-v1`
//! (`script_macos`), with the same probes, supervision and limits.

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

/// The TCP port a `network: https` grant may connect to.
pub const SCRIPT_HTTPS_PORT: u16 = 443;

/// The network one script run gets (§3 Network).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptSandboxNetwork {
    /// Its own network namespace: only its own loopback interface.
    None,
    /// The host network under a Landlock ruleset that admits TCP connect
    /// to this port only and no TCP bind. `network: https` is
    /// [`SCRIPT_HTTPS_PORT`]. Needs Landlock ABI
    /// [`crate::NETWORK_LANDLOCK_ABI`]; an older kernel refuses the run.
    TcpPort(u16),
    /// The host network with no rule.
    Any,
}

impl ScriptSandboxNetwork {
    /// The sandbox network a grant's `network` capability names.
    pub fn for_grant(network: orbit_types::workflow::activity_job::ScriptNetwork) -> Self {
        use orbit_types::workflow::activity_job::ScriptNetwork;
        match network {
            ScriptNetwork::None => Self::None,
            ScriptNetwork::Https => Self::TcpPort(SCRIPT_HTTPS_PORT),
            ScriptNetwork::Any => Self::Any,
        }
    }
}

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
    /// Declared file inputs, each bound read-only at `in/<name>`. The
    /// caller resolves and admits them first ([`script_input_conflict`]);
    /// the profile refuses one that still overlaps protected state.
    pub inputs: &'a [ScriptSandboxInput],
    /// The network the script reaches.
    pub network: ScriptSandboxNetwork,
}

/// One declared file input of a script run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptSandboxInput {
    /// The physical source path, a regular file or a directory.
    pub source: PathBuf,
    /// A plain name: the input appears at `in/<name>`.
    pub name: String,
}

/// The protected tree a script input source would expose, if any: a masked
/// tree it is, contains or lies within (a mask enclosing the cwd counts only
/// for a source outside the cwd, as it is not mounted at all), or a
/// credential path it is, contains or lies within. `source` and `cwd` are
/// physical paths. A bind at `in/<name>` is not covered by the masks and
/// denies, which apply at host paths, so such a source is refused instead.
pub fn script_input_conflict(
    source: &Path,
    cwd: &Path,
    masked: &[PathBuf],
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    let overlaps =
        |protected: &Path| source.starts_with(protected) || protected.starts_with(source);
    for mask in masked {
        let Ok(mask) = mask.canonicalize() else {
            continue;
        };
        if cwd.starts_with(&mask) && source.starts_with(cwd) {
            continue;
        }
        if overlaps(&mask) {
            return Some(mask);
        }
    }
    super::credential_denies(home)
        .into_iter()
        .map(|(path, _)| path.canonicalize().unwrap_or(path))
        .find(|path| overlaps(path))
}

pub(super) fn valid_input_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
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
    let writable = admitted_profile_roots(spec)?;
    // `--unshare-all` includes the network namespace: with no
    // `--share-net`, the only interface is the namespace's own loopback. A
    // networked grant shares the host's; `TcpPort` is confined by the
    // Landlock ruleset [`run_script_sandboxed`] applies to the child.
    let mut args: Vec<String> = vec![
        "--unshare-all".into(),
        "--new-session".into(),
        "--die-with-parent".into(),
    ];
    if spec.network != ScriptSandboxNetwork::None {
        args.push("--share-net".into());
    }
    args.extend(["--proc", "/proc", "--dev", "/dev"].map(String::from));
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
    // Each input over its placeholder in the read-only `in/`: Bubblewrap
    // cannot create a mountpoint inside a read-only bind, so the caller's
    // `in/` holds an empty file or directory of the same name
    // ([`run_script_sandboxed`] creates it). `--ro-bind` makes the input
    // read-only whatever its host mode.
    for input in spec.inputs {
        args.extend([
            "--ro-bind".into(),
            display(&input.source),
            display(&mount.join("in").join(&input.name)),
        ]);
    }
    // Git metadata is readable so `git describe` and `git archive` work,
    // and never writable, even inside a writable worktree.
    for git_dir in cwd_git_dirs(spec.cwd) {
        push_ro_bind(&mut args, &git_dir);
    }
    // A mask is a tmpfs over the tree. It stays writable until the credential
    // denies have mounted their own mountpoints inside it (`~/.orbit/run` lies
    // under the global root), and goes read-only after them. A readable file
    // the grant names inside a mask (the `orbit` binary under `~/.orbit/bin`)
    // is bound again over the tmpfs; directories and everything else under a
    // mask stay hidden.
    let mut sealed = Vec::new();
    for masked in spec.masked {
        let Ok(masked) = masked.canonicalize() else {
            continue;
        };
        if spec.cwd.starts_with(&masked) || !masked.is_dir() {
            continue;
        }
        args.extend(["--tmpfs".into(), display(&masked)]);
        for root in spec.readable {
            if let Ok(root) = root.canonicalize()
                && root.starts_with(&masked)
                && root.is_file()
            {
                push_ro_bind(&mut args, &root);
            }
        }
        sealed.push(masked);
    }
    push_credential_denies(&mut args, spec.home);
    for masked in &sealed {
        args.extend(["--remount-ro".into(), display(masked)]);
    }
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

/// The checks both platforms' profiles make before compiling, and the
/// granted write roots resolved to their physical paths. A read or write
/// root that is, contains or lies within a masked tree enclosing the cwd
/// is refused (that tree is not mounted on Linux, and is not allowed on
/// macOS, so it cannot be masked), as is an input that is not a plain name
/// over an absolute source or that overlaps protected state.
pub(super) fn admitted_profile_roots(
    spec: &ScriptSandboxSpec<'_>,
) -> Result<Vec<PathBuf>, OrbitError> {
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
    for input in spec.inputs {
        if !valid_input_name(&input.name) || !input.source.is_absolute() {
            return Err(OrbitError::InvalidInput(format!(
                "script input `{}` needs a plain name and an absolute source",
                input.name
            )));
        }
        let source = input.source.canonicalize().map_err(|error| {
            OrbitError::InvalidInput(format!(
                "cannot resolve script input `{}` at {}: {error}",
                input.name,
                input.source.display()
            ))
        })?;
        let cwd = cwd_physical.as_deref().unwrap_or(spec.cwd);
        if let Some(protected) = script_input_conflict(&source, cwd, spec.masked, spec.home) {
            return Err(OrbitError::PolicyDenied(format!(
                "script input `{}` overlaps protected state at {}",
                input.name,
                protected.display()
            )));
        }
    }
    Ok(writable)
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
pub(super) fn cwd_git_dirs(cwd: &Path) -> Vec<PathBuf> {
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

/// An empty file or directory at `in/<name>` for each input, matching its
/// source, so Bubblewrap binds over an existing mountpoint.
#[cfg(target_os = "linux")]
fn create_input_mountpoints(spec: &ScriptSandboxSpec<'_>) -> Result<(), OrbitError> {
    for input in spec.inputs {
        if !valid_input_name(&input.name) {
            return Err(OrbitError::InvalidInput(format!(
                "script input name `{}` is not a plain name",
                input.name
            )));
        }
        let mountpoint = spec.scratch.join("in").join(&input.name);
        let unavailable = |error: std::io::Error| {
            OrbitError::InvalidInput(format!(
                "cannot prepare script input `{}` at {}: {error}",
                input.name,
                mountpoint.display()
            ))
        };
        if std::fs::symlink_metadata(&mountpoint).is_ok() {
            return Err(OrbitError::InvalidInput(format!(
                "script input `{}` already has an entry in {}",
                input.name,
                spec.scratch.join("in").display()
            )));
        }
        if input.source.is_dir() {
            orbit_common::fs::io::create_private_dir_all(&mountpoint).map_err(unavailable)?;
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&mountpoint)
                .map_err(unavailable)?;
        }
    }
    Ok(())
}

/// Whether this host can apply the script profile now, and which profile it
/// is. The error is the `script_sandbox_unavailable` reason: a platform
/// without the profile, a start nested inside another sandbox, or a host
/// without the sandbox's prerequisites. There is no weaker fallback.
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
    #[cfg(target_os = "macos")]
    {
        super::script_macos::probe()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(
            "script steps run only on Linux and macOS; this platform has no script profile"
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

/// Whether this host can give a script `network`. A port-confined network
/// needs Landlock ABI [`crate::NETWORK_LANDLOCK_ABI`]; only Linux shares a
/// network with a script at all. macOS refuses every network grant: without
/// a PID namespace, a descendant that calls `setsid` escapes the timeout
/// kill and would keep its network after the step ends. The error is the
/// reason, for `script_sandbox_unavailable`.
pub fn probe_script_sandbox_network(network: ScriptSandboxNetwork) -> Result<(), String> {
    if network == ScriptSandboxNetwork::None {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        if let ScriptSandboxNetwork::TcpPort(port) = network {
            let abi = crate::linux_landlock::abi_version();
            if abi < crate::NETWORK_LANDLOCK_ABI {
                return Err(format!(
                    "network `https` needs Landlock ABI {} to confine TCP to port {port}; this \
                     kernel provides {}",
                    crate::NETWORK_LANDLOCK_ABI,
                    if abi < 0 {
                        "no Landlock".to_string()
                    } else {
                        format!("ABI {abi}")
                    }
                ));
            }
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Err(super::script_macos::NETWORK_REFUSED.to_string())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("a script step gets network access only on Linux".to_string())
    }
}

/// The most file descriptors one script process may hold open
/// (`RLIMIT_NOFILE`, script steps §3 Resources).
pub const SCRIPT_OPEN_FILES_LIMIT: u64 = 1024;

/// The `RLIMIT_*` limits a script run gets: `RLIMIT_CPU` at its timeout in
/// CPU seconds per process (`SIGXCPU`, then `SIGKILL` after the termination
/// grace), and `RLIMIT_NOFILE` at [`SCRIPT_OPEN_FILES_LIMIT`].
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn script_process_limits(timeout: Duration) -> super::supervise::ProcessLimits {
    super::supervise::ProcessLimits {
        cpu_seconds: timeout
            .as_secs()
            .saturating_add(u64::from(timeout.subsec_nanos() > 0)),
        cpu_grace_seconds: SCRIPT_TERMINATION_GRACE.as_secs(),
        open_files: SCRIPT_OPEN_FILES_LIMIT,
    }
}

/// Where the script sees its scratch directory: [`SCRIPT_SANDBOX_MOUNT`]
/// under Bubblewrap, and the host path itself under Seatbelt, which
/// filters paths but cannot remount them. Argv and environment entries
/// that name the scratch directory (the entry snapshot, `HOME`, `TMPDIR`,
/// `ORBIT_SCRIPT_IN`, `ORBIT_SCRIPT_OUT`) are built from it.
pub fn script_scratch_path_in_sandbox(scratch: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        scratch.to_path_buf()
    } else {
        PathBuf::from(SCRIPT_SANDBOX_MOUNT)
    }
}

/// Run one script under the profile. The caller probes first
/// ([`probe_script_sandbox`], [`probe_script_sandbox_network`]); a profile
/// that cannot be applied here is an error, never a weaker run.
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
        create_input_mountpoints(spec)?;
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
        // The ruleset is applied in the child before `exec`, so it binds the
        // launcher, Bubblewrap and the script alike; the descriptor lives
        // until the run ends.
        let ruleset = match spec.network {
            ScriptSandboxNetwork::TcpPort(port) => Some(
                crate::linux_landlock::restrict_child_tcp_connect_to_port(&mut command, port)?,
            ),
            ScriptSandboxNetwork::None | ScriptSandboxNetwork::Any => None,
        };
        supervise_script(command, ruleset, &request, request.launcher_env)
    }
    #[cfg(target_os = "macos")]
    {
        // Seatbelt cannot nest: a start inside another sandbox is refused
        // here too, so it never reaches `sandbox-exec` and runs nothing.
        super::script_macos::probe().map_err(OrbitError::PolicyDenied)?;
        if !request.launcher.is_empty() {
            return Err(OrbitError::PolicyDenied(
                "a macOS script run takes no launcher; macOS has no per-script scope".to_string(),
            ));
        }
        let command = super::script_macos::command(spec, request.argv)?;
        // `sandbox-exec` passes its environment to the script unchanged, so
        // the wrapper's environment is the script's own.
        supervise_script(command, (), &request, request.env)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = request;
        Err(OrbitError::PolicyDenied(
            "script steps run only on Linux and macOS".to_string(),
        ))
    }
}

/// Supervise one started sandbox wrapper: the step's stdin, its capped
/// stdout and log, its timeout and scratch bound, and its `RLIMIT_*`
/// limits. `wrapper_env` is the environment the wrapper process starts
/// with.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn supervise_script<G>(
    command: std::process::Command,
    guard: G,
    request: &ScriptSandboxRequest<'_>,
    wrapper_env: &[(String, String)],
) -> Result<ScriptSandboxRun, OrbitError> {
    let phase = super::supervise::Phase {
        env: wrapper_env,
        cwd: request.sandbox.cwd,
        timeout: request.timeout,
        size_root: request.sandbox.scratch,
        size_cap: request.scratch_cap_bytes,
        stdin: Some(request.stdin.clone()),
        stdout_cap: Some(SCRIPT_STDOUT_CAP_BYTES),
        grace: SCRIPT_TERMINATION_GRACE,
        limits: Some(script_process_limits(request.timeout)),
    };
    let mut log = super::supervise::BuildLog::with_cap(SCRIPT_LOG_CAP_BYTES);
    let started = std::time::Instant::now();
    let run = super::supervise::run_phase(command, guard, &phase, &mut log)?;
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
