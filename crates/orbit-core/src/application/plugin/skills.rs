//! Link a plugin's skills into provider discovery, and unlink them again
//! (design `docs/design/plugins/1_scope.md` §1, §3).
//!
//! A plugin's skills stay in its install directory: Orbit links them into the
//! same `skill_link_roots` (`~/.agents/skills`, `~/.claude/skills`) the
//! shipped skills use, so Claude and Codex discover them without a copy that
//! could drift from the installed version. Disable removes exactly the links
//! that point into that plugin's root, and `orbit plugin doctor` reports a
//! link whose target is gone.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::remove_path_if_exists;
use orbit_tools::plugin::LoadedPlugin;

use crate::bootstrap::init::{ensure_skill_links, skill_link_roots};

/// What linking did for one skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSkillLink {
    /// Skill id, the directory name under the plugin root.
    pub skill_id: String,
    /// The link Orbit maintains.
    pub link: PathBuf,
    /// The plugin directory the link points at.
    pub target: PathBuf,
}

/// The discovery roots this host links skills into.
///
/// Every function below takes the roots explicitly and this is the only place
/// that reads the home directory, so the linking rules can be exercised
/// against temporary roots without a test touching a real `~/.claude`.
pub(crate) fn link_roots() -> Vec<PathBuf> {
    crate::paths::home_dir()
        .map(|home| skill_link_roots(&home))
        .unwrap_or_default()
}

/// Link every skill `plugin` ships into each discovery root.
///
/// A discovery root that cannot be written is reported as a warning rather
/// than failing the enable: the plugin's tools and definitions are already
/// recorded, and a missing skill link is a `doctor` finding, not a reason to
/// leave the host half-enabled.
pub fn link_plugin_skills(plugin: &LoadedPlugin) -> (Vec<PluginSkillLink>, Vec<String>) {
    link_plugin_skills_into(&link_roots(), plugin)
}

/// [`link_plugin_skills`] against explicit discovery roots.
pub fn link_plugin_skills_into(
    roots: &[PathBuf],
    plugin: &LoadedPlugin,
) -> (Vec<PluginSkillLink>, Vec<String>) {
    let mut linked = Vec::new();
    let mut warnings = Vec::new();
    for skill_dir in &plugin.skills {
        let Some(skill_id) = skill_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(parent) = skill_dir.parent() else {
            continue;
        };
        for root in roots {
            match ensure_skill_links(parent, &[skill_id], root, false) {
                Ok(_) => linked.push(PluginSkillLink {
                    skill_id: skill_id.to_string(),
                    link: root.join(skill_id),
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

/// Remove the links that point into `install_path`, whatever they are named.
///
/// The install path rather than the manifest's skill list is the selector, so
/// a disable still cleans up after a manifest that changed between enable and
/// disable.
pub fn unlink_plugin_skills(install_path: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    unlink_plugin_skills_from(&link_roots(), install_path)
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
pub fn dangling_plugin_skill_links(plugin_root: &Path) -> Vec<(PathBuf, PathBuf)> {
    dangling_plugin_skill_links_in(&link_roots(), plugin_root)
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
