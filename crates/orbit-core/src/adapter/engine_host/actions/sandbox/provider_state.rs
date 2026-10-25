use std::path::{Path, PathBuf};

use orbit_common::fs::physical::canonicalize_existing_prefix_checked;
use orbit_engine::DispatchError;
use orbit_types::policy::ResolvedFsProfile;

use super::resolve::append_unique_modify_root;
use super::runtime_paths::linux_path_inspection_error;

pub(super) fn append_linux_provider_state_roots(
    provider: &str,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), DispatchError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut directories = Vec::new();
    if let Some(path) = std::env::var_os("CODEX_HOME").map(PathBuf::from) {
        directories.push(path);
    } else if let Some(home) = &home {
        directories.push(home.join(".codex"));
    }
    if let Some(path) = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) {
        directories.push(path);
    } else if let Some(home) = &home {
        directories.push(home.join(".claude"));
    }
    if let Some(home) = &home {
        directories.push(home.join(".gemini"));
        directories.push(home.join(".grok"));
    }
    // [ORB-10946] Copilot's roots are appended only when Copilot is the
    // provider being dispatched. This mirrors the macOS gate, and it matters
    // more here than on macOS: every entry in this list is *created* by the
    // validated provider-root path below, so an unconditional entry would
    // mkdir a `~/.copilot` on hosts that have never installed the CLI.
    directories.extend(linux_copilot_state_roots(provider, home.as_deref()));
    directories.extend(linux_muse_state_roots(provider, home.as_deref()));
    for directory in directories {
        let canonical = ensure_linux_provider_directory(&directory, home.as_deref())?;
        append_unique_modify_root(resolved, canonical.display().to_string());
    }
    Ok(())
}

/// Reject a provider state root that would grant more than a provider directory.
///
/// `candidate` is the path being judged; `configured` is what the operator
/// supplied. They differ once symlinks have been resolved, and naming both keeps
/// a rejection traceable back to the setting that caused it.
fn reject_overbroad_linux_provider_state_root(
    candidate: &Path,
    configured: &Path,
    home: Option<&Path>,
) -> Result<(), DispatchError> {
    let describe = || {
        if candidate == configured {
            format!("`{}`", configured.display())
        } else {
            format!(
                "`{}` (resolved to `{}`)",
                configured.display(),
                candidate.display()
            )
        }
    };

    if candidate.parent().is_none() {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux provider state root {} must not be the filesystem root",
            describe()
        )));
    }

    if let Some(home) = home {
        let canonical_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        if canonical_home.starts_with(candidate) {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "Linux provider state root {} is broader than the user's home directory",
                describe()
            )));
        }
    }

    Ok(())
}

/// Resolve a provider state root before creating it on behalf of a child.
///
/// Provider-specific environment variables are operator-configurable, but
/// they must not turn sandbox preparation into an arbitrary path creator.
/// Reject relative paths, path traversal, and root/home-wide targets.
///
/// Symlinks are resolved rather than rejected. Hosts routinely reach `$HOME`
/// through a symlinked ancestor (OSTree systems ship `/home -> /var/home`), and
/// dotfile managers routinely make a provider directory itself a symlink; both
/// are ordinary configurations, not attacks. [ORB-11984]
///
/// Following symlinks means the configured path no longer bounds the grant, so
/// containment is enforced twice: once on what the operator supplied, and again
/// on the resolved destination. The returned path has an existing canonical
/// ancestor and only the validated missing suffix, so the caller can safely
/// materialize it.
pub(super) fn validated_linux_provider_state_root(
    path: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, DispatchError> {
    if !path.is_absolute() {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux provider state root `{}` must be absolute",
            path.display()
        )));
    }

    for component in path.components() {
        if !matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        ) {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "Linux provider state root `{}` must not contain traversal components",
                path.display()
            )));
        }
    }
    reject_overbroad_linux_provider_state_root(path, path, home)?;

    // Resolve the deepest part of the path that already exists, so a provider
    // directory that is itself a symlink to an existing directory validates
    // against its real destination.
    let Some(validated) = canonicalize_existing_prefix_checked(path)
        .map_err(|error| linux_path_inspection_error(path, error))?
    else {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux provider state root `{}` has no existing ancestor",
            path.display()
        )));
    };

    reject_overbroad_linux_provider_state_root(&validated, path, home)?;

    Ok(validated)
}

fn ensure_linux_provider_directory(
    path: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, DispatchError> {
    let validated = validated_linux_provider_state_root(path, home)?;
    orbit_common::fs::io::create_private_dir_all(&validated).map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "create Linux provider state root `{}`: {error}",
            validated.display()
        ))
    })?;
    validated.canonicalize().map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "canonicalize Linux provider state root `{}`: {error}",
            validated.display()
        ))
    })
}

/// `<xdg base>/<app>`: the explicit XDG variable when non-empty, otherwise its
/// specified default relative to `$HOME`. XDG treats empty values as unset.
fn xdg_scoped_root(
    home: Option<&Path>,
    xdg_base: Option<PathBuf>,
    home_relative_default: &[&str],
    app: &str,
) -> Option<PathBuf> {
    let base = xdg_base
        .filter(|base| !base.as_os_str().is_empty())
        .or_else(|| {
            home.map(|home| {
                home_relative_default
                    .iter()
                    .fold(home.to_path_buf(), |path, segment| path.join(segment))
            })
        })?;
    Some(base.join(app))
}

/// Writable state roots for an active Muse Code executor on Linux.
///
/// Muse keeps `settings.json` and the `muse login` credential `auth.json` in
/// `$XDG_CONFIG_HOME/muse` (default `$HOME/.config/muse`), and local tracing,
/// plugins, and skills in `$XDG_DATA_HOME/muse` (default
/// `$HOME/.local/share/muse`). Its login is that file, not a keyring, so no
/// other carve-out is needed. No other provider receives this grant — every
/// entry in the caller's list is *created* by `ensure_linux_provider_directory`,
/// so an unconditional entry would mkdir a `~/.config/muse` on hosts that
/// never installed Muse. [ORB-16577]
fn linux_muse_state_roots(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Muse)
    {
        return Vec::new();
    }
    // XDG treats an empty variable as unset.
    let env_path = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    [
        xdg_scoped_root(home, env_path("XDG_CONFIG_HOME"), &[".config"], "muse"),
        xdg_scoped_root(
            home,
            env_path("XDG_DATA_HOME"),
            &[".local", "share"],
            "muse",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Process-env wrapper around [`linux_copilot_state_roots_with`].
fn linux_copilot_state_roots(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    linux_copilot_state_roots_with(
        provider,
        home,
        std::env::var_os("COPILOT_HOME")
            .map(PathBuf::from)
            .as_deref(),
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .as_deref(),
    )
}

/// Writable roots an active Copilot executor needs on Linux: its
/// configuration/state directory (`$COPILOT_HOME`, else `$HOME/.copilot`) and
/// the launcher's bundled-package extraction cache (`$XDG_CACHE_HOME/copilot`,
/// else `$HOME/.cache/copilot`). Empty for every other provider. [ORB-10946]
///
/// The override values are parameters rather than direct env reads so the
/// gate can be asserted without mutating process state from a test.
// pub(super) widened for the sibling tests/ layout.
fn linux_copilot_state_roots_with(
    provider: &str,
    home: Option<&Path>,
    copilot_home: Option<&Path>,
    xdg_cache_home: Option<&Path>,
) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Copilot)
    {
        return Vec::new();
    }
    let mut roots = Vec::with_capacity(2);
    match copilot_home {
        Some(path) => roots.push(path.to_path_buf()),
        None => {
            if let Some(home) = home {
                roots.push(home.join(".copilot"));
            }
        }
    }
    match xdg_cache_home {
        Some(path) => roots.push(path.join("copilot")),
        None => {
            if let Some(home) = home {
                roots.push(home.join(".cache").join("copilot"));
            }
        }
    }
    roots
}
