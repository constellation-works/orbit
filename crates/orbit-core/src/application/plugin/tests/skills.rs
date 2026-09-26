//! Skill links: enable links a plugin's skills into provider discovery,
//! disable unlinks them, and `doctor` reports a link whose target is gone
//! (design §1, §3).
//!
//! The low-level discovery roots are supplied explicitly here. Lifecycle
//! coverage also proves that production derives them from the runtime's
//! global root, keeping a temporary runtime inside its fixture directory.

use std::path::PathBuf;

use orbit_common::fs::io::create_dir_symlink;
use orbit_tools::plugin::load_plugin_dir;

use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::application::plugin::skills::{
    dangling_plugin_skill_links_in, link_plugin_skills_into, unlink_plugin_skills_from,
};
use crate::application::plugin::{
    PluginAddOptions, PluginEnableOptions, disable_plugin, enable_plugin, install_plugin,
    plugin_doctor,
};

fn roots(fixture: &PluginFixture) -> Vec<PathBuf> {
    [".agents", ".claude"]
        .into_iter()
        .map(|dir| fixture.repo_root.join(dir).join("skills"))
        .collect()
}

#[test]
fn lifecycle_links_beside_the_runtime_global_root() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 plugin source"),
        &PluginAddOptions::default(),
    )
    .expect("install disabled plugin");
    let runtime = fixture.reopen();

    let result =
        enable_plugin(&runtime, "graph", &PluginEnableOptions::default()).expect("enable plugin");
    let discovery_base = fixture
        .global_root
        .parent()
        .expect("fixture global root has a parent");
    let expected_roots = [".agents", ".claude"].map(|dir| discovery_base.join(dir).join("skills"));
    assert_eq!(result.skills.len(), expected_roots.len());
    for root in &expected_roots {
        let link = root.join("graph-graph");
        assert!(
            result.skills.iter().any(|skill| skill.link == link),
            "enable must report the runtime-scoped link at {}: {:?}",
            link.display(),
            result.skills
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("runtime-scoped skill link")
                .file_type()
                .is_symlink()
        );
    }
    for provider in [".agents", ".claude"] {
        let home = std::env::var_os("HOME").expect("isolated HOME");
        assert!(
            !PathBuf::from(&home).join(provider).exists(),
            "enabling a plugin under a fixture root must not write under HOME"
        );
    }

    std::fs::remove_dir_all(PathBuf::from(&result.summary.install_path).join("skills"))
        .expect("remove installed skill tree");
    let doctor = plugin_doctor(&runtime).expect("inspect runtime-scoped discovery roots");
    for root in &expected_roots {
        let link = root.join("graph-graph");
        assert!(
            doctor.iter().any(|finding| finding
                .message
                .contains(link.to_str().expect("fixture discovery path is utf8"))),
            "doctor must report the runtime-scoped dangling link at {}: {:?}",
            link.display(),
            doctor
        );
    }
    let skill_findings = runtime
        .doctor_file_skills()
        .expect("inspect selected-root skill discovery");
    assert_eq!(
        skill_findings
            .iter()
            .filter(|finding| finding.skill_name == "graph-graph")
            .count(),
        2,
        "skill doctor must inspect both selected-root discovery directories"
    );

    disable_plugin(&runtime, "graph").expect("disable plugin");
    for root in &expected_roots {
        assert!(
            std::fs::symlink_metadata(root.join("graph-graph")).is_err(),
            "disable must remove the runtime-scoped link from {}",
            root.display()
        );
    }
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
fn linking_replaces_a_dangling_link_into_the_plugin_install_family() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let roots = roots(&fixture);
    let previous_skill = plugin
        .root
        .parent()
        .expect("plugin install family")
        .join("previous-version/skills/graph");

    for root in &roots {
        std::fs::create_dir_all(root).expect("create discovery root");
        create_dir_symlink(&previous_skill, &root.join("graph-graph"))
            .expect("link dangling previous plugin skill");
    }

    let (linked, warnings) = link_plugin_skills_into(&roots, &plugin);

    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(linked.len(), roots.len());
    for root in &roots {
        assert_eq!(
            root.join("graph-graph")
                .canonicalize()
                .expect("replacement link resolves"),
            plugin.skills[0]
                .canonicalize()
                .expect("installed plugin skill resolves"),
            "a dangling link owned by an earlier install must not block re-enable"
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
