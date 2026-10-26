//! Invocation-local skill discovery. Shared provider state stays at its
//! original paths; only the selected skill directories are materialized.

use std::path::{Path, PathBuf};

use orbit_types::workflow::{ExperimentActivityOverlay, experiment_skill_provider_supported};

use super::super::super::dispatcher::DispatchError;

/// Keep the returned directory alive until the provider and its children exit.
/// A control dispatch does no filesystem work and changes no environment.
pub(super) fn prepare_experiment_skill_root(
    provider: &str,
    overlays: &[ExperimentActivityOverlay],
    scratch: &Path,
    child_env: &mut Vec<(String, String)>,
) -> Result<Option<tempfile::TempDir>, DispatchError> {
    let bundles = overlays
        .iter()
        .flat_map(|overlay| &overlay.skills)
        .collect::<Vec<_>>();
    if bundles.is_empty() {
        return Ok(None);
    }
    let refuse = |detail: String| {
        DispatchError::CliInvocationPermanent(format!("skill overlay refused: {detail}"))
    };
    if !experiment_skill_provider_supported(provider) {
        return Err(refuse(format!(
            "provider `{provider}` is unsupported; supported: claude on Unix"
        )));
    }
    let value = |key: &str| {
        child_env
            .iter()
            .rev()
            .find(|(name, value)| name == key && !value.is_empty())
            .map(|(_, value)| PathBuf::from(value))
    };
    let configured = value("CLAUDE_CONFIG_DIR");
    let home = value("HOME");
    let source = configured
        .clone()
        .or_else(|| home.as_ref().map(|home| home.join(".claude")))
        .ok_or_else(|| refuse("neither CLAUDE_CONFIG_DIR nor HOME is set".into()))?;
    if !source.is_absolute() {
        return Err(refuse("CLAUDE_CONFIG_DIR must be absolute".into()));
    }
    let mut names = std::collections::BTreeSet::new();
    for bundle in &bundles {
        bundle.verify().map_err(&refuse)?;
        if !names.insert(bundle.name.as_str()) {
            return Err(refuse(format!("duplicate skill `{}`", bundle.name)));
        }
    }
    let private = tempfile::Builder::new()
        .prefix("experiment-skills-")
        .tempdir_in(scratch)
        .map_err(|error| refuse(error.to_string()))?;
    let skills_root = private.path().join("skills");
    orbit_common::fs::io::create_private_dir(&skills_root)
        .map_err(|error| refuse(error.to_string()))?;
    mirror_claude_entries(&source, private.path(), &["skills"])
        .map_err(|error| refuse(error.to_string()))?;
    // With the default config directory Claude keeps this sidecar in HOME;
    // an explicit config directory keeps it inside that directory instead.
    if configured.is_none()
        && let Some(sidecar) = home
            .map(|home| home.join(".claude.json"))
            .filter(|path| path.exists())
        && !private.path().join(".claude.json").exists()
    {
        link_claude_state(&sidecar, &private.path().join(".claude.json"))
            .map_err(|error| refuse(error.to_string()))?;
    }
    mirror_claude_entries(
        &source.join("skills"),
        &skills_root,
        &names.into_iter().collect::<Vec<_>>(),
    )
    .map_err(|error| refuse(error.to_string()))?;
    for bundle in bundles {
        let root = skills_root.join(&bundle.name);
        orbit_common::fs::io::create_private_dir(&root)
            .map_err(|error| refuse(error.to_string()))?;
        for (path, file) in &bundle.files {
            let target = root.join(path);
            if let Some(parent) = target.parent() {
                orbit_common::fs::io::create_private_dir_all(parent)
                    .map_err(|error| refuse(error.to_string()))?;
            }
            orbit_common::fs::io::atomic_write_bytes(&target, &file.bytes)
                .map_err(|error| refuse(error.to_string()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &target,
                    std::fs::Permissions::from_mode(if file.executable { 0o700 } else { 0o600 }),
                )
                .map_err(|error| refuse(error.to_string()))?;
            }
        }
    }
    child_env.retain(|(key, _)| key != "CLAUDE_CONFIG_DIR");
    child_env.push((
        "CLAUDE_CONFIG_DIR".into(),
        private.path().display().to_string(),
    ));
    Ok(Some(private))
}

fn mirror_claude_entries(source: &Path, target: &Path, excluded: &[&str]) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if !excluded.iter().any(|name| entry.file_name() == *name) {
            link_claude_state(&entry.path(), &target.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn link_claude_state(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(not(unix))]
fn link_claude_state(_source: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "skill overlays require Unix",
    ))
}
