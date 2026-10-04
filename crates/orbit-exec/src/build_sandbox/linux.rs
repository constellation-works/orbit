//! `linux-bwrap-build-v1`: Bubblewrap with a constructed root (§3.2).
//!
//! The agent sandbox starts from `--ro-bind / /` and hides what it must; this
//! profile starts from Bubblewrap's empty root and adds what a build needs.
//! A path outside the readable set does not exist inside the sandbox.

use std::path::{Path, PathBuf};

use super::{BuildPhaseNetwork, BuildSandboxSpec};
use crate::linux_landlock::{RESOLVER_FILES, RUNTIME_DIRS, RUNTIME_FILES, TRUST_DIRS};

/// The Bubblewrap argv (without the wrapper itself) for one phase: the
/// namespaces, the constructed root, the build directory as the only
/// writable bind, and `argv` after `--`.
///
/// Reads the host filesystem to decide which runtime paths exist and which
/// are symbolic links (`/bin` on a merged-`/usr` system), so the rendered
/// root matches this host.
pub fn compile_linux_build_argv(
    spec: &BuildSandboxSpec<'_>,
    argv: &[String],
    cwd: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--unshare-all".into(),
        "--new-session".into(),
        "--die-with-parent".into(),
    ];
    if spec.network == BuildPhaseNetwork::Https {
        args.push("--share-net".into());
    }
    args.extend(["--proc", "/proc", "--dev", "/dev"].map(String::from));

    let mut bound: Vec<PathBuf> = Vec::new();
    // `/dev` is Bubblewrap's own minimal device tree; the runtime table's
    // device entries are already in it.
    let system_dirs = RUNTIME_DIRS
        .iter()
        .chain(TRUST_DIRS)
        .filter(|path| !path.starts_with("/dev"));
    for dir in system_dirs {
        let path = Path::new(dir);
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(path) {
                args.extend([
                    "--symlink".into(),
                    target.display().to_string(),
                    dir.to_string(),
                ]);
            }
            continue;
        }
        push_ro_bind(&mut args, path);
        bound.push(path.to_path_buf());
    }
    let system_files = RUNTIME_FILES
        .iter()
        .chain(RESOLVER_FILES)
        .filter(|path| !path.starts_with("/dev"));
    for file in system_files {
        let path = Path::new(file);
        // A resolver file is often a link into `/run`; the bind follows it so
        // the sandbox gets the content without the rest of `/run`.
        if path.is_file() && !is_under(path, &bound) {
            push_ro_bind(&mut args, path);
        }
    }
    for root in spec.readable {
        if !is_under(root, &bound) && root.exists() {
            push_ro_bind(&mut args, root);
        }
    }
    for (path, file) in super::credential_denies_under(spec) {
        if !path.exists() {
            continue;
        }
        if file {
            args.extend(["--ro-bind".into(), "/dev/null".into(), display(&path)]);
        } else {
            args.extend([
                "--tmpfs".into(),
                display(&path),
                "--remount-ro".into(),
                display(&path),
            ]);
        }
    }
    args.extend([
        "--bind".into(),
        display(spec.build_dir),
        display(spec.build_dir),
        // Everything but the build directory is read-only, including the
        // directories Bubblewrap created in its root to hold the binds.
        "--remount-ro".into(),
        "/".into(),
        "--chdir".into(),
        display(cwd),
        "--".into(),
    ]);
    args.extend(argv.iter().cloned());
    args
}

fn push_ro_bind(args: &mut Vec<String>, path: &Path) {
    args.extend(["--ro-bind".into(), display(path), display(path)]);
}

fn is_under(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(target_os = "linux")]
pub(super) fn probe(fetch: bool) -> Result<super::BuildSandboxProbe, String> {
    let bwrap = crate::probe_bwrap();
    if !bwrap.available {
        return Err(format!(
            "the {} build profile needs Bubblewrap: {}",
            orbit_types::plugin::PLUGIN_BUILD_PROFILE_LINUX,
            bwrap.detail
        ));
    }
    let landlock_abi = if fetch {
        let abi = crate::linux_landlock::abi_version();
        if abi < crate::NETWORK_LANDLOCK_ABI {
            return Err(format!(
                "spec.build.fetch needs Landlock ABI {} to confine TCP to port {}; this kernel \
                 provides {}",
                crate::NETWORK_LANDLOCK_ABI,
                super::PLUGIN_BUILD_FETCH_PORT,
                if abi < 0 {
                    "no Landlock".to_string()
                } else {
                    format!("ABI {abi}")
                }
            ));
        }
        Some(abi)
    } else {
        None
    };
    Ok(super::BuildSandboxProbe {
        profile: orbit_types::plugin::PLUGIN_BUILD_PROFILE_LINUX,
        landlock_abi,
    })
}

/// The Bubblewrap command for one phase, and the Landlock ruleset descriptor
/// a `fetch` phase's child applies before `exec` (kept alive until spawn).
#[cfg(target_os = "linux")]
pub(super) fn command(
    request: &super::BuildPhaseRequest<'_>,
) -> Result<(std::process::Command, Option<std::os::fd::OwnedFd>), orbit_common::OrbitError> {
    let bwrap = crate::bwrap_path().ok_or_else(|| {
        orbit_common::OrbitError::PolicyDenied(format!(
            "trusted Bubblewrap not available at {}",
            crate::bwrap_program_for_audit()
        ))
    })?;
    let mut command = std::process::Command::new(bwrap);
    command.args(compile_linux_build_argv(
        &request.sandbox,
        request.argv,
        request.cwd,
    ));
    let ruleset = match request.sandbox.network {
        BuildPhaseNetwork::None => None,
        BuildPhaseNetwork::Https => {
            Some(crate::linux_landlock::restrict_child_tcp_connect_to_port(
                &mut command,
                super::PLUGIN_BUILD_FETCH_PORT,
            )?)
        }
    };
    Ok((command, ruleset))
}
