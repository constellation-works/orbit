use std::path::{Path, PathBuf};

use orbit_engine::DispatchError;
use orbit_types::policy::ResolvedFsProfile;

use super::resolve::append_unique_modify_root;
use super::runtime_paths::resolved_existing_ancestor;

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
    directories.extend(linux_cursor_state_roots_with(provider, home.as_deref()));
    directories.extend(linux_pi_state_roots(provider, home.as_deref()));
    directories.extend(linux_opencode_state_roots(provider, home.as_deref()));
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
    let Some(validated) = resolved_existing_ancestor(path)? else {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux provider state root `{}` has no existing ancestor",
            path.display()
        )));
    };

    reject_overbroad_linux_provider_state_root(&validated, path, home)?;

    Ok(validated)
}

pub(super) fn ensure_linux_provider_directory(
    path: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, DispatchError> {
    let validated = validated_linux_provider_state_root(path, home)?;
    std::fs::create_dir_all(&validated).map_err(|error| {
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

/// Writable state root for an active Cursor executor on Linux. The CLI stores
/// logged-in authentication, settings, permissions, and sessions under
/// `$HOME/.cursor`; no other provider receives this grant. [ORB-10945]
pub(super) fn linux_cursor_state_roots_with(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Cursor)
    {
        return Vec::new();
    }
    home.map(|home| vec![home.join(".cursor")])
        .unwrap_or_default()
}

/// Process-env wrapper around [`linux_pi_state_roots_with`].
fn linux_pi_state_roots(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    linux_pi_state_roots_with(
        provider,
        home,
        std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .as_deref(),
    )
}

/// Writable state root for an active Pi executor on Linux. The CLI stores
/// `/login` credentials, settings, saved project trust decisions, installed
/// packages, and sessions under `$PI_CODING_AGENT_DIR` when set, otherwise
/// `$HOME/.pi`. No other provider receives this grant — every entry in the
/// caller's list is *created* by `ensure_linux_provider_directory`, so an
/// unconditional entry would mkdir a `~/.pi` on hosts that never installed Pi.
/// [ORB-11296]
pub(super) fn linux_pi_state_roots_with(
    provider: &str,
    home: Option<&Path>,
    pi_coding_agent_dir: Option<&Path>,
) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Pi)
    {
        return Vec::new();
    }
    match pi_coding_agent_dir {
        Some(path) => vec![path.to_path_buf()],
        None => home.map(|home| vec![home.join(".pi")]).unwrap_or_default(),
    }
}

/// Process-env wrapper around [`linux_opencode_state_roots_with`].
fn linux_opencode_state_roots(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let env_path = |name: &str| std::env::var_os(name).map(PathBuf::from);
    linux_opencode_state_roots_with(
        provider,
        home,
        OpencodeStateEnv {
            xdg_data_home: env_path("XDG_DATA_HOME"),
            xdg_config_home: env_path("XDG_CONFIG_HOME"),
            xdg_state_home: env_path("XDG_STATE_HOME"),
            xdg_cache_home: env_path("XDG_CACHE_HOME"),
            opencode_config_dir: env_path("OPENCODE_CONFIG_DIR"),
        },
    )
}

/// XDG roots that locate OpenCode's writable state on Linux.
#[derive(Default, Clone)]
pub(super) struct OpencodeStateEnv {
    pub(super) xdg_data_home: Option<PathBuf>,
    pub(super) xdg_config_home: Option<PathBuf>,
    pub(super) xdg_state_home: Option<PathBuf>,
    pub(super) xdg_cache_home: Option<PathBuf>,
    pub(super) opencode_config_dir: Option<PathBuf>,
}

/// Writable state roots for an active OpenCode executor on Linux.
///
/// OpenCode resolves every root through `xdg-basedir` and creates its data,
/// config, and state directories at startup, before it reads Orbit's envelope.
/// The data root holds `auth.json` from `opencode auth login`, the session and
/// message stores, and logs. No other provider receives this grant — every
/// entry in the caller's list is *created* by `ensure_linux_provider_directory`,
/// so an unconditional entry would mkdir an `~/.local/share/opencode` on hosts
/// that never installed OpenCode. [ORB-11295]
pub(super) fn linux_opencode_state_roots_with(
    provider: &str,
    home: Option<&Path>,
    env: OpencodeStateEnv,
) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Opencode)
    {
        return Vec::new();
    }
    let scoped = |xdg_base: Option<PathBuf>, home_relative_default: &[&str]| -> Option<PathBuf> {
        let base = xdg_base.or_else(|| {
            home.map(|home| {
                home_relative_default
                    .iter()
                    .fold(home.to_path_buf(), |path, segment| path.join(segment))
            })
        })?;
        Some(base.join("opencode"))
    };
    [
        scoped(env.xdg_data_home, &[".local", "share"]),
        env.opencode_config_dir
            .or_else(|| scoped(env.xdg_config_home, &[".config"])),
        scoped(env.xdg_state_home, &[".local", "state"]),
        scoped(env.xdg_cache_home, &[".cache"]),
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
pub(super) fn linux_copilot_state_roots_with(
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
