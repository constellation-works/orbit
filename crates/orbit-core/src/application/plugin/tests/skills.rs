//! Skill links: enable links a plugin's skills into provider discovery,
//! disable unlinks them, and `doctor` reports a link whose target is gone
//! (design §1, §3).
//!
//! The discovery roots are supplied explicitly here. Production resolves them
//! from the home directory, and a test that wrote there would edit the
//! developer's real `~/.claude/skills`.

use std::path::PathBuf;

use orbit_common::fs::io::create_dir_symlink;
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
    std::fs::rename(source.join("skills/graph"), source.join("skills/orbit"))
        .expect("rename fixture skill to collide with a shipped id");
    let manifest = std::fs::read_to_string(source.join("plugin.yaml")).expect("read manifest");
    std::fs::write(
        source.join("plugin.yaml"),
        manifest.replace("skills/graph", "skills/orbit"),
    )
    .expect("update fixture manifest");
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let roots = roots(&fixture);

    let shipped_skill = fixture.global_root.join("skills/orbit");
    std::fs::create_dir_all(&shipped_skill).expect("create shipped skill");
    std::fs::write(shipped_skill.join("SKILL.md"), "# Shipped Orbit skill\n")
        .expect("write shipped skill");
    for root in &roots {
        std::fs::create_dir_all(root).expect("create discovery root");
        create_dir_symlink(&shipped_skill, &root.join("orbit")).expect("link shipped skill");
    }

    let (linked, warnings) = link_plugin_skills_into(&roots, &plugin);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(linked.len(), roots.len());
    for root in &roots {
        assert_eq!(
            root.join("orbit")
                .canonicalize()
                .expect("resolve shipped link"),
            shipped_skill.canonicalize().expect("resolve shipped skill"),
            "the plugin must not replace the shipped skill link"
        );
        let link = root.join("graph-orbit");
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
        assert!(!root.join("graph-orbit").exists());
        assert_eq!(
            root.join("orbit")
                .canonicalize()
                .expect("resolve shipped link"),
            shipped_skill.canonicalize().expect("resolve shipped skill"),
            "disable must remove only links created for this plugin"
        );
    }
}

#[test]
fn linking_refuses_to_replace_a_namespaced_link_owned_elsewhere() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let roots = roots(&fixture);
    let external_skill = fixture.global_root.join("user-skills/graph");
    std::fs::create_dir_all(&external_skill).expect("create external skill");
    std::fs::write(
        external_skill.join("SKILL.md"),
        "# User-owned graph skill\n",
    )
    .expect("write external skill");
    for root in &roots {
        std::fs::create_dir_all(root).expect("create discovery root");
        create_dir_symlink(&external_skill, &root.join("graph-graph"))
            .expect("link user-owned skill");
    }

    let (linked, warnings) = link_plugin_skills_into(&roots, &plugin);

    assert!(
        linked.is_empty(),
        "no external link may be claimed: {linked:?}"
    );
    assert_eq!(warnings.len(), roots.len(), "{warnings:?}");
    for root in &roots {
        assert_eq!(
            root.join("graph-graph")
                .canonicalize()
                .expect("resolve external link"),
            external_skill
                .canonicalize()
                .expect("resolve external skill"),
            "a same-named user-owned link must not be replaced"
        );
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
