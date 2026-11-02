//! `macos-sandbox-script-v1`: the macOS profile a granted script step runs
//! under (`docs/design/script-steps/1_scope.md` §3).
//!
//! It starts from the build profile's deny-default SBPL preamble and allows
//! the same paths the Linux profile mounts: the host runtime table and the
//! grant's recorded interpreters and read roots, read-only; the cwd tree (a
//! run worktree read-write, or the workspace root read-only) with its Git
//! metadata read-only; the grant's write roots read-write; and the step's
//! scratch directory, whose `snapshot/` and `in/` are read-only. Seatbelt filters
//! paths but cannot remount them, so the script sees the scratch directory
//! at its host path ([`super::script_scratch_path_in_sandbox`]), and each
//! declared input is copied into `in/<as>` before spawn instead of bound.
//!
//! SBPL's last match wins, so the order is the policy: allows first, then
//! the masked trees (the workspace's `.orbit/`, the global root) denied,
//! then the scratch directory allowed again (it lives under the workspace
//! state), then the read-only carve-outs, and the credential denies last,
//! so they win over every grant. The network is always denied: a network
//! grant is refused before anything runs ([`NETWORK_REFUSED`]).
//!
//! The gaps against Linux are stated in the design: no memory or task
//! limits (only `RLIMIT_CPU` and `RLIMIT_NOFILE`), a timed-out descendant
//! that calls `setsid` escapes the process-group kill (it stays under this
//! profile), file metadata stays visible as in the build profile, and
//! privacy permissions (TCC) apply on top of the profile.

#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

use orbit_common::OrbitError;

use super::macos::{
    DEV_WRITE_ALLOW, deny_default_preamble, physical, push_credential_denies, runtime_read_filters,
    subpath,
};
use super::script::{ScriptSandboxNetwork, ScriptSandboxSpec, admitted_profile_roots};
use crate::macos_sandbox::sbpl_filter::sbpl_escape;

/// Why a macOS script step gets no network grant.
pub(super) const NETWORK_REFUSED: &str = "a script step gets no network on macOS: without a PID \
     namespace, a descendant that calls setsid escapes the timeout kill and would keep its \
     network after the step ends";

/// The gaps of [`orbit_types::workflow::activity_job::SCRIPT_PROFILE_MACOS`]
/// against the Linux profile, as `orbit doctor` reports them.
pub const SCRIPT_MACOS_PROFILE_GAPS: &str = "no memory or task limits (only RLIMIT_CPU and \
     RLIMIT_NOFILE), a timed-out descendant that calls setsid can outlive the step under the \
     profile, no network grants, file metadata outside the granted paths stays visible, and \
     macOS privacy permissions (TCC) apply on top of the profile";

/// The SBPL profile for one script. Reads the host filesystem to resolve
/// the grant's roots and the cwd tree's Git directories, and refuses what
/// the Linux profile refuses (a root or input overlapping protected state)
/// plus any network.
pub fn compile_macos_script_profile(spec: &ScriptSandboxSpec<'_>) -> Result<String, OrbitError> {
    if spec.network != ScriptSandboxNetwork::None {
        return Err(OrbitError::PolicyDenied(format!(
            "{NETWORK_REFUSED}; nothing was run"
        )));
    }
    let writable_roots = admitted_profile_roots(spec)?;
    let scratch = |name: &str| subpath(&spec.scratch.join(name));
    let git_dirs = super::script::cwd_git_dirs(spec.cwd);

    let mut profile = deny_default_preamble();
    let mut readable = runtime_read_filters();
    readable.extend(spec.readable.iter().map(|path| subpath(path)));
    // Like Linux's --bind mounts, write roots allow reads and execution.
    // The later mask and credential rules still protect their children.
    readable.extend(writable_roots.iter().map(|path| subpath(path)));
    readable.push(subpath(spec.cwd));
    readable.extend(git_dirs.iter().map(|path| subpath(path)));
    readable.push(subpath(spec.scratch));
    let readable = readable.join(" ");
    profile.push_str(&format!("(allow file-read* {readable})\n"));
    profile.push_str(&format!("(allow process-exec {readable})\n"));

    let mut writable: Vec<String> = Vec::new();
    if spec.cwd_writable {
        writable.push(subpath(spec.cwd));
    }
    writable.extend(writable_roots.iter().map(|path| subpath(path)));
    let scratch_writable = ["home", "tmp", "out"].map(scratch).join(" ");
    writable.push(scratch_writable.clone());
    profile.push_str(&format!("(allow file-write* {})\n", writable.join(" ")));
    profile.push_str(DEV_WRITE_ALLOW);

    // The masked trees: a mask that encloses the cwd is not allowed in the
    // first place (and a root that would expose it was refused above), so
    // only a mask inside an allowed tree is denied. A readable file the
    // grant names inside a mask (the `orbit` binary under `~/.orbit/bin`) is
    // allowed again; directories and everything else under a mask stay
    // denied.
    let mut denied: Vec<PathBuf> = Vec::new();
    for masked in spec.masked {
        let Ok(masked) = masked.canonicalize() else {
            continue;
        };
        if spec.cwd.starts_with(&masked) || !masked.is_dir() || denied.contains(&masked) {
            continue;
        }
        denied.push(masked.clone());
        let filter = subpath(&masked);
        profile.push_str(&format!(
            "(deny file-read-data file-read-xattr file-write* process-exec {filter})\n"
        ));
        for root in spec.readable {
            if let Ok(root) = root.canonicalize()
                && root.starts_with(&masked)
                && root.is_file()
            {
                let literal = format!("(literal \"{}\")", sbpl_escape(&physical(&root)));
                profile.push_str(&format!("(allow file-read* process-exec {literal})\n"));
            }
        }
    }
    // The scratch directory lives under the workspace state, inside a mask.
    let scratch_root = subpath(spec.scratch);
    profile.push_str(&format!(
        "(allow file-read* process-exec {scratch_root})\n(allow file-write* {scratch_writable})\n"
    ));
    // Read-only carve-outs inside writable trees: the snapshot the grant was
    // checked against, the copied inputs, and the cwd tree's Git metadata.
    let mut read_only = vec![scratch("snapshot"), scratch("in")];
    if spec.cwd_writable {
        read_only.push(subpath(&spec.cwd.join(".git")));
    }
    for git_dir in git_dirs.iter().map(|path| subpath(path)) {
        if !read_only.contains(&git_dir) {
            read_only.push(git_dir);
        }
    }
    profile.push_str(&format!("(deny file-write* {})\n", read_only.join(" ")));

    push_credential_denies(&mut profile, spec.home);
    profile.push_str("(deny network*)\n");
    Ok(profile)
}

/// Whether this host can apply the macOS script profile now. A process that
/// already runs under Seatbelt cannot apply another profile (`sandbox-exec`
/// exits 71, `sandbox_apply: Operation not permitted`), so a start nested in
/// a worker sandbox is refused here instead of running under the outer one.
#[cfg(target_os = "macos")]
pub(super) fn probe() -> Result<&'static str, String> {
    use crate::SandboxExecApplyProbe;
    use orbit_types::workflow::activity_job::SCRIPT_PROFILE_MACOS;

    match crate::probe_sandbox_exec_apply() {
        SandboxExecApplyProbe::Applied => Ok(SCRIPT_PROFILE_MACOS),
        SandboxExecApplyProbe::ApplyRefused(reason) => Err(format!(
            "this process already runs inside a Seatbelt sandbox, which cannot apply the \
             {SCRIPT_PROFILE_MACOS} profile ({reason}); a script step does not start nested \
             inside another sandbox"
        )),
        SandboxExecApplyProbe::BinaryAbsent(reason) => {
            Err(format!("the {SCRIPT_PROFILE_MACOS} profile needs {reason}"))
        }
        SandboxExecApplyProbe::Failed(reason) => Err(format!(
            "the {SCRIPT_PROFILE_MACOS} profile could not be applied: {reason}"
        )),
        SandboxExecApplyProbe::NotMacos => Err("this host is not macOS".to_string()),
    }
}

/// The `sandbox-exec` command for one script: its inputs copied into
/// `in/<as>`, then the profile passed inline (a profile file in a shared
/// temp root could be rewritten by another confined process before
/// `sandbox-exec` reads it).
#[cfg(target_os = "macos")]
pub(super) fn command(
    spec: &ScriptSandboxSpec<'_>,
    argv: &[String],
) -> Result<std::process::Command, OrbitError> {
    let profile = compile_macos_script_profile(spec)?;
    copy_script_inputs(spec)?;
    let sandbox_exec = crate::sandbox_exec_path()
        .ok_or_else(|| OrbitError::PolicyDenied(crate::sandbox_exec_unavailable_message()))?;
    let mut command = std::process::Command::new(sandbox_exec);
    command
        .arg("-p")
        .arg(profile)
        .args(argv)
        .current_dir(spec.cwd);
    Ok(command)
}

/// Copy each declared input into `in/<as>`. Seatbelt cannot bind a path
/// elsewhere, so the script reads a copy; the profile keeps `in/` read-only.
/// Symlinks are copied as symlinks: their targets resolve under the profile
/// like any other path. The copy counts against the scratch cap.
#[cfg(target_os = "macos")]
fn copy_script_inputs(spec: &ScriptSandboxSpec<'_>) -> Result<(), OrbitError> {
    let in_dir = spec.scratch.join("in");
    for input in spec.inputs {
        if !super::script::valid_input_name(&input.name) {
            return Err(OrbitError::InvalidInput(format!(
                "script input name `{}` is not a plain name",
                input.name
            )));
        }
        let destination = in_dir.join(&input.name);
        if std::fs::symlink_metadata(&destination).is_ok() {
            return Err(OrbitError::InvalidInput(format!(
                "script input `{}` already has an entry in {}",
                input.name,
                in_dir.display()
            )));
        }
        copy_input_tree(&input.source, &destination).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "cannot copy script input `{}` from {}: {error}",
                input.name,
                input.source.display()
            ))
        })?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_input_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    let kind = metadata.file_type();
    if kind.is_dir() {
        orbit_common::fs::io::create_private_dir_all(destination)?;
        let mut entries: Vec<PathBuf> = std::fs::read_dir(source)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<_>>()?;
        entries.sort();
        for entry in entries {
            let Some(name) = entry.file_name() else {
                continue;
            };
            copy_input_tree(&entry, &destination.join(name))?;
        }
        Ok(())
    } else if kind.is_file() {
        std::fs::copy(source, destination).map(|_| ())
    } else if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(source)?, destination)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a file, directory or symlink", source.display()),
        ))
    }
}
