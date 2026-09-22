//! Link a plugin's skills into provider discovery, and unlink them again
//! (design `docs/design/plugins/1_scope.md` §1, §3).
//!
//! A plugin's skills stay in its install directory: Orbit links them into the
//! same `skill_link_roots` (`~/.agents/skills`, `~/.claude/skills` for the
//! default `~/.orbit` root) the shipped skills use, under the namespaced id
//! `<plugin>-<skill>`. An alternate global root keeps the discovery roots
//! beside itself rather than mutating the invoking user's home. The namespace
//! keeps plugin links disjoint from shipped skills, and linking refuses to
//! replace a same-named link owned outside the plugin's install family.
//! Disable removes exactly the links that point into that plugin's root, and
//! `orbit plugin doctor` reports a link whose target is gone.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{create_dir_symlink, remove_path_if_exists};
use orbit_tools::plugin::LoadedPlugin;

use crate::bootstrap::init::skill_link_roots;

/// What linking did for one skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSkillLink {
    /// Namespaced discovery id (`<plugin>-<skill-directory>`).
    pub skill_id: String,
    /// The link Orbit maintains.
    pub link: PathBuf,
    /// The plugin directory the link points at.
    pub target: PathBuf,
}

/// The provider discovery roots belonging to one runtime's global root.
///
/// The default global root is `~/.orbit`, so its siblings are the ordinary
/// `~/.agents/skills` and `~/.claude/skills` directories. A `--root` or
/// `ORBIT_ROOT` override instead keeps the links beside that selected root,
/// which also confines in-process fixtures to their temporary directory.
pub(crate) fn link_roots(global_root: &Path) -> Vec<PathBuf> {
    global_root
        .parent()
        .map(skill_link_roots)
        .unwrap_or_default()
}

/// Link every skill `plugin` ships into each discovery root.
///
/// A discovery root that cannot be written is reported as a warning rather
/// than failing the enable: the plugin's tools and definitions are already
/// recorded, and a missing skill link is a `doctor` finding, not a reason to
/// leave the host half-enabled.
pub(crate) fn link_plugin_skills(
    global_root: &Path,
    plugin: &LoadedPlugin,
) -> (Vec<PluginSkillLink>, Vec<String>) {
    link_plugin_skills_into(&link_roots(global_root), plugin)
}

/// [`link_plugin_skills`] against explicit discovery roots.
pub fn link_plugin_skills_into(
    roots: &[PathBuf],
    plugin: &LoadedPlugin,
) -> (Vec<PluginSkillLink>, Vec<String>) {
    let mut linked = Vec::new();
    let mut warnings = Vec::new();
    for skill_dir in &plugin.skills {
        let Some(skill_id) = plugin_skill_link_id(plugin.namespace(), skill_dir) else {
            continue;
        };
        for root in roots {
            match ensure_plugin_skill_link(root, &skill_id, skill_dir, &plugin.root) {
                Ok(_) => linked.push(PluginSkillLink {
                    skill_id: skill_id.clone(),
                    link: root.join(&skill_id),
                    target: skill_dir.clone(),
                }),
                Err(error) => warnings.push(format!(
                    "could not link skill '{skill_id}' from plugin '{}' into {}: {error}",
                    plugin.namespace(),
                    root.display()
                )),
            }
        }
    }
    (linked, warnings)
}

/// The provider-discovery id for a plugin skill.
///
/// The plugin namespace is already a validated manifest identifier, and the
/// source id is the final component of a contained `spec.skills[]` path.
pub(crate) fn plugin_skill_link_id(namespace: &str, skill_dir: &Path) -> Option<String> {
    let source_id = skill_dir.file_name()?.to_str()?;
    Some(format!("{namespace}-{source_id}"))
}

fn ensure_plugin_skill_link(
    root: &Path,
    skill_id: &str,
    target: &Path,
    plugin_root: &Path,
) -> Result<(), OrbitError> {
    if let Ok(metadata) = fs::symlink_metadata(root)
        && !metadata.file_type().is_dir()
    {
        return Err(OrbitError::InvalidInput(format!(
            "expected '{}' to be a directory for skill links; found non-directory path",
            root.display()
        )));
    }
    fs::create_dir_all(root).map_err(|error| OrbitError::Io(error.to_string()))?;

    let link = root.join(skill_id);
    let Ok(metadata) = fs::symlink_metadata(&link) else {
        create_dir_symlink(target, &link).map_err(|error| OrbitError::Io(error.to_string()))?;
        return Ok(());
    };
    if !metadata.file_type().is_symlink() {
        return Err(OrbitError::InvalidInput(format!(
            "refusing to replace non-symlink discovery path '{}'",
            link.display()
        )));
    }

    let existing = resolve_link_target(&link)?;
    let expected = target
        .canonicalize()
        .map_err(|error| OrbitError::Io(error.to_string()))?;
    if existing
        .canonicalize()
        .is_ok_and(|resolved| resolved == expected)
    {
        return Ok(());
    }

    // A version upgrade may leave this namespace's discovery link pointing
    // at the previous installed version. That target is safe to replace; a
    // link into shipped skills, another plugin, or user content is not.
    let install_family = plugin_root.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "plugin root '{}' has no install-family directory",
            plugin_root.display()
        ))
    })?;
    let existing_is_plugin_owned = existing
        .canonicalize()
        .is_ok_and(|resolved| resolved.starts_with(install_family))
        || (!existing.exists() && existing.starts_with(install_family));
    if !existing_is_plugin_owned {
        return Err(OrbitError::InvalidInput(format!(
            "refusing to replace discovery link '{}' because it points outside plugin '{}'",
            link.display(),
            plugin_root.display()
        )));
    }

    fs::remove_file(&link).map_err(|error| OrbitError::Io(error.to_string()))?;
    create_dir_symlink(target, &link).map_err(|error| OrbitError::Io(error.to_string()))?;
    Ok(())
}

fn resolve_link_target(link: &Path) -> Result<PathBuf, OrbitError> {
    let target = fs::read_link(link).map_err(|error| OrbitError::Io(error.to_string()))?;
    if target.is_absolute() {
        Ok(target)
    } else {
        Ok(link.parent().unwrap_or(Path::new(".")).join(target))
    }
}

/// Remove the links that point into `install_path`, whatever they are named.
///
/// The install path rather than the manifest's skill list is the selector, so
/// a disable still cleans up after a manifest that changed between enable and
/// disable.
pub(crate) fn unlink_plugin_skills(
    global_root: &Path,
    install_path: &Path,
) -> Result<Vec<PathBuf>, OrbitError> {
    unlink_plugin_skills_from(&link_roots(global_root), install_path)
}

/// [`unlink_plugin_skills`] against explicit discovery roots.
pub fn unlink_plugin_skills_from(
    roots: &[PathBuf],
    install_path: &Path,
) -> Result<Vec<PathBuf>, OrbitError> {
    let mut removed = Vec::new();
    for root in roots {
        for (link, target) in links_under(root) {
            if !target.starts_with(install_path) {
                continue;
            }
            remove_path_if_exists(&link)?;
            removed.push(link);
        }
    }
    removed.sort();
    Ok(removed)
}

/// Links in the discovery roots whose target no longer exists, paired with the
/// target they name. Only links pointing into `plugin_root` are reported: a
/// dangling link to anything else belongs to the skill catalog's own doctor.
pub(crate) fn dangling_plugin_skill_links(
    global_root: &Path,
    plugin_root: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    dangling_plugin_skill_links_in(&link_roots(global_root), plugin_root)
}

/// [`dangling_plugin_skill_links`] against explicit discovery roots.
pub fn dangling_plugin_skill_links_in(
    roots: &[PathBuf],
    plugin_root: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    let mut dangling = Vec::new();
    for root in roots {
        for (link, target) in links_under(root) {
            if target.starts_with(plugin_root) && !target.exists() {
                dangling.push((link, target));
            }
        }
    }
    dangling.sort();
    dangling
}

/// Every symlink directly under `root`, with the path it names. The target is
/// resolved without requiring it to exist, so a dangling link is visible.
fn links_under(root: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let Ok(target) = std::fs::read_link(&path) else {
            continue;
        };
        let target = if target.is_absolute() {
            target
        } else {
            root.join(target)
        };
        links.push((path, target));
    }
    links
}
