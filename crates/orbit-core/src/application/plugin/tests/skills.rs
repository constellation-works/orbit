//! Skill links: enable links a plugin's skills into provider discovery,
//! disable unlinks them, and `doctor` reports a link whose target is gone
//! (design §1, §3).
//!
//! The discovery roots are supplied explicitly here. Production resolves them
//! from the home directory, and a test that wrote there would edit the
//! developer's real `~/.claude/skills`.

use std::path::PathBuf;

use orbit_tools::plugin::load_plugin_dir;

use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::application::plugin::skills::{
    dangling_plugin_skill_links_in, link_plugin_skills_into, unlink_plugin_skills_from,
};

fn roots(fixture: &PluginFixture) -> Vec<PathBuf> {
    [".agents", ".claude"]
        .into_iter()
        .map(|dir| fixture.repo_root.join(dir).join("skills"))
        .collect()
}

#[test]
fn enable_links_each_skill_into_every_discovery_root_and_disable_unlinks_it() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let roots = roots(&fixture);

    let (linked, warnings) = link_plugin_skills_into(&roots, &plugin);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(linked.len(), roots.len());
    for root in &roots {
        let link = root.join("graph");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("a link")
                .file_type()
                .is_symlink()
        );
        assert!(link.join("SKILL.md").exists(), "the link resolves");
    }

    let removed = unlink_plugin_skills_from(&roots, &plugin.root).expect("unlink");
    assert_eq!(removed.len(), roots.len());
    for root in &roots {
        assert!(!root.join("graph").exists());
    }
}

#[test]
fn doctor_reports_a_link_whose_plugin_directory_is_gone() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let roots = roots(&fixture);
    link_plugin_skills_into(&roots, &plugin);

    assert!(dangling_plugin_skill_links_in(&roots, &plugin.root).is_empty());

    std::fs::remove_dir_all(plugin.root.join("skills")).expect("remove the skill tree");
    let dangling = dangling_plugin_skill_links_in(&roots, &plugin.root);
    assert_eq!(dangling.len(), roots.len(), "{dangling:?}");
}
