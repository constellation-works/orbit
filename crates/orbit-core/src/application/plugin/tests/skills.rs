//! Skill links: enable links a plugin's skills into provider discovery,
//! disable unlinks them, and `doctor` reports a link whose target is gone
//! (design §1, §3).
//!
//! The low-level discovery roots are supplied explicitly here. Lifecycle
//! coverage also proves that production derives them from the runtime's
//! global root, keeping a temporary runtime inside its fixture directory.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "lifecycle_links_beside_the_runtime_global_root",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "enable_links_each_skill_into_every_discovery_root_and_disable_unlinks_it",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "linking_refuses_to_replace_a_namespaced_link_owned_elsewhere",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "linking_replaces_a_dangling_link_into_the_plugin_install_family",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "doctor_reports_a_link_whose_plugin_directory_is_gone",
    ) {
        return;
    }
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

/// Discovery names that unlink must remove and doctor must treat as this
/// plugin's links. Spellings that only look local, or that cannot be resolved,
/// stay listed in the preserved set below.
const OWNED_LINKS: &[&str] = &[
    "owned-direct",
    "owned-dangling-family",
    "owned-relative-dangling",
    "owned-via-alias",
    "owned-version-dangling",
    "owned-version-relative",
];

const PRESERVED_LINKS: &[&str] = &[
    "escape-dotdot-existing",
    "escape-dotdot-dangling",
    "escape-symlink-existing",
    "escape-symlink-dangling",
    "escape-version-dotdot-existing",
    "escape-version-dotdot-dangling",
    "escape-version-symlink-existing",
    "escape-version-symlink-dangling",
    "ambiguous-missing-dotdot",
    "symlink-cycle",
    "unrelated",
];

#[test]
fn disabling_preserves_links_that_resolve_outside_the_namespace() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "disabling_preserves_links_that_resolve_outside_the_namespace",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let layout = NamespaceLayout::prepare(&fixture, &plugin.root);
    let skill_dir = plugin.skills.first().expect("plugin skill");
    let discovery = layout.discovery.clone();
    let user_skill = write_user_skill(&fixture);

    let owned_relative = layout.relative_alias("missing-version/skills/guide");
    let owned_version_relative =
        layout.relative_alias_inside_plugin(&plugin.root, "missing-from-alias");
    let links = [
        ("owned-direct", skill_dir.clone()),
        (
            "owned-dangling-family",
            layout.family.join("missing-version/skills/guide"),
        ),
        ("owned-relative-dangling", owned_relative),
        ("owned-via-alias", layout.alias_to_skill(skill_dir)),
        (
            "owned-version-dangling",
            plugin.root.join("skills/missing-guide"),
        ),
        ("owned-version-relative", owned_version_relative),
        (
            "escape-dotdot-existing",
            layout.family.join("../other/1.0.0/skills/guide"),
        ),
        (
            "escape-dotdot-dangling",
            layout.family.join("../other/9.9.9/skills/guide"),
        ),
        (
            "escape-symlink-existing",
            layout.family.join("via-other/skills/guide"),
        ),
        (
            "escape-symlink-dangling",
            layout.family.join("via-other/skills/missing-guide"),
        ),
        (
            "escape-version-dotdot-existing",
            plugin.root.join("../../other/1.0.0/skills/guide"),
        ),
        (
            "escape-version-dotdot-dangling",
            plugin.root.join("../../other/9.9.9/skills/guide"),
        ),
        (
            "escape-version-symlink-existing",
            plugin.root.join("escape/skills/guide"),
        ),
        (
            "escape-version-symlink-dangling",
            plugin.root.join("escape/skills/missing-guide"),
        ),
        (
            "ambiguous-missing-dotdot",
            layout
                .family
                .join("missing-hop/../other/1.0.0/skills/guide"),
        ),
        ("symlink-cycle", layout.family.join("cycle/skills/guide")),
        ("unrelated", user_skill.clone()),
    ];
    for (name, spelling) in links {
        install_link(&spelling, &discovery.join(name));
    }
    write_kept_file(&discovery);
    let other_discovery = layout.other_discovery.clone();
    create_dir_symlink(&user_skill, &other_discovery.join("unrelated-claude"))
        .expect("link unrelated skill in the other discovery root");
    write_kept_file(&other_discovery);

    assert!(
        layout
            .family
            .join("../other/1.0.0/skills/guide")
            .starts_with(&layout.family),
        "dotdot escape must lexically begin inside the namespace"
    );
    assert!(
        !layout
            .family
            .join("../other/1.0.0/skills/guide")
            .canonicalize()
            .expect("other skill exists")
            .starts_with(&layout.family),
        "dotdot escape must resolve outside the namespace"
    );
    assert!(
        layout
            .family
            .join("via-other/skills/guide")
            .starts_with(&layout.family)
    );
    assert!(
        !layout
            .family
            .join("via-other/skills/guide")
            .canonicalize()
            .expect("symlinked skill exists")
            .starts_with(&layout.family)
    );
    assert!(
        plugin
            .root
            .join("escape/skills/missing-guide")
            .starts_with(&plugin.root)
    );
    assert!(
        !discovery
            .join(layout.relative_alias("missing-version/skills/guide"))
            .starts_with(&layout.family),
        "relative alias spelling must not be a lexical child of the namespace"
    );
    assert!(
        std::fs::read_link(discovery.join("owned-relative-dangling"))
            .expect("read relative spelling")
            .is_relative()
    );
    assert!(
        std::fs::read_link(discovery.join("owned-version-relative"))
            .expect("read version-relative spelling")
            .components()
            .any(|component| component == Component::ParentDir)
    );

    let roots = layout.roots();
    let family_dangling = name_set(&dangling_plugin_skill_links_in(&roots, &layout.family));
    assert_eq!(
        family_dangling,
        name_set_from(&[
            "owned-dangling-family",
            "owned-relative-dangling",
            "owned-version-dangling",
            "owned-version-relative",
        ]),
        "doctor must report only dangling links that resolve inside the namespace: {family_dangling:?}"
    );
    let version_dangling = name_set(&dangling_plugin_skill_links_in(&roots, &plugin.root));
    assert_eq!(
        version_dangling,
        name_set_from(&["owned-version-dangling", "owned-version-relative"]),
        "doctor must use the same resolution for the version directory: {version_dangling:?}"
    );

    let preserved: Vec<(PathBuf, PathBuf)> = PRESERVED_LINKS
        .iter()
        .map(|name| {
            let link = discovery.join(name);
            (
                link.clone(),
                std::fs::read_link(&link).expect("read preserved link"),
            )
        })
        .chain(std::iter::once((
            other_discovery.join("unrelated-claude"),
            std::fs::read_link(other_discovery.join("unrelated-claude")).expect("read other root"),
        )))
        .collect();

    let removed = unlink_plugin_skills_from(&roots, &layout.family).expect("unlink namespace");
    assert_eq!(
        name_set_of_paths(&removed),
        name_set_from(OWNED_LINKS),
        "unlink must remove only links that physically resolve inside the namespace: {removed:?}"
    );
    for name in OWNED_LINKS {
        assert!(
            std::fs::symlink_metadata(discovery.join(name)).is_err(),
            "owned link {name} must be gone"
        );
    }
    for (link, spelling) in preserved {
        assert_eq!(
            std::fs::read_link(&link).expect("preserved link still exists"),
            spelling,
            "disabling must leave {} unchanged",
            link.display()
        );
    }
    assert_eq!(
        std::fs::read_to_string(discovery.join("notes/README")).expect("read kept file"),
        "keep\n"
    );
    assert_eq!(
        std::fs::read_to_string(other_discovery.join("notes/README"))
            .expect("read other kept file"),
        "keep\n"
    );
}

#[test]
fn replacement_and_doctor_agree_on_physical_link_ownership() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "replacement_and_doctor_agree_on_physical_link_ownership",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    let plugin = load_plugin_dir(&source).expect("load plugin");
    let layout = NamespaceLayout::prepare(&fixture, &plugin.root);
    let discovery = layout.discovery.clone();
    let user_skill = write_user_skill(&fixture);
    create_dir_symlink(&user_skill, &discovery.join("custom")).expect("link unrelated skill");
    write_kept_file(&discovery);
    let roots = vec![discovery.clone()];
    let skill_dir = plugin.skills.first().expect("plugin skill");

    let relative = layout.relative_alias_inside_plugin(&plugin.root, "missing-from-alias");
    let joined = discovery.join(&relative);
    assert!(
        relative.is_relative() && !joined.starts_with(&plugin.root),
        "owned relative spelling must not lexically begin at the plugin root"
    );
    create_dir_symlink(&relative, &discovery.join("graph-graph"))
        .expect("link owned dangling skill");
    assert_eq!(
        name_set(&dangling_plugin_skill_links_in(&roots, &plugin.root)),
        name_set_from(&["graph-graph"])
    );
    assert_eq!(
        name_set(&dangling_plugin_skill_links_in(&roots, &layout.family)),
        name_set_from(&["graph-graph"])
    );

    let custom_spelling = std::fs::read_link(discovery.join("custom")).expect("read custom");
    let (linked, warnings) = link_plugin_skills_into(&roots, &plugin);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(linked.len(), 1);
    assert_eq!(
        discovery
            .join("graph-graph")
            .canonicalize()
            .expect("replaced link resolves"),
        skill_dir.canonicalize().expect("plugin skill resolves")
    );
    assert_eq!(
        std::fs::read_link(discovery.join("custom")).expect("custom remains"),
        custom_spelling
    );
    assert_eq!(
        std::fs::read_to_string(discovery.join("notes/README")).expect("notes remain"),
        "keep\n"
    );

    let escapes = [
        plugin.root.join("../../other/9.9.9/skills/guide"),
        plugin.root.join("../../other/1.0.0/skills/guide"),
        plugin.root.join("escape/skills/missing-guide"),
        plugin.root.join("escape/skills/guide"),
    ];
    for escape in escapes {
        assert!(
            escape.starts_with(&plugin.root),
            "{} must lexically begin inside the plugin root",
            escape.display()
        );
        if escape.exists() {
            assert!(
                !escape
                    .canonicalize()
                    .expect("existing escape resolves")
                    .starts_with(&layout.family),
                "{} must resolve outside the install family",
                escape.display()
            );
        }
        std::fs::remove_file(discovery.join("graph-graph")).expect("reset discovery link");
        create_dir_symlink(&escape, &discovery.join("graph-graph")).expect("install escape link");
        let spelling = std::fs::read_link(discovery.join("graph-graph")).expect("read escape");
        assert!(
            !name_set(&dangling_plugin_skill_links_in(&roots, &plugin.root))
                .contains("graph-graph"),
            "doctor must not claim {}",
            escape.display()
        );
        assert!(
            !name_set(&dangling_plugin_skill_links_in(&roots, &layout.family))
                .contains("graph-graph"),
            "family doctor must not claim {}",
            escape.display()
        );
        let (linked, warnings) = link_plugin_skills_into(&roots, &plugin);
        assert!(
            linked.is_empty(),
            "replacement must refuse {}: {linked:?}",
            escape.display()
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            std::fs::read_link(discovery.join("graph-graph")).expect("escape spelling survives"),
            spelling
        );
        assert_eq!(
            std::fs::read_link(discovery.join("custom")).expect("custom survives replacement"),
            custom_spelling
        );
        assert_eq!(
            std::fs::read_to_string(discovery.join("notes/README")).expect("notes survive"),
            "keep\n"
        );
    }
}

struct NamespaceLayout {
    family: PathBuf,
    sources: PathBuf,
    discovery: PathBuf,
    other_discovery: PathBuf,
}

fn install_link(target: &Path, link: &Path) {
    create_dir_symlink(target, link)
        .unwrap_or_else(|error| panic!("link {} -> {}: {error}", link.display(), target.display()));
}

impl NamespaceLayout {
    fn prepare(fixture: &PluginFixture, plugin_root: &Path) -> Self {
        let family = plugin_root
            .parent()
            .expect("plugin install family")
            .to_path_buf();
        let sources = family
            .parent()
            .expect("canonical sources dir")
            .to_path_buf();
        let other_skill = sources.join("other/1.0.0/skills/guide");
        std::fs::create_dir_all(&other_skill).expect("create the other plugin skill");
        std::fs::write(other_skill.join("SKILL.md"), "# Other\n").expect("write other skill");
        create_dir_symlink(&family, &sources.join("alias-graph")).expect("alias the graph family");
        create_dir_symlink(&sources.join("other/1.0.0"), &family.join("via-other"))
            .expect("symlink an ancestor out of the family");
        create_dir_symlink(&family.join("cycle"), &family.join("cycle")).expect("symlink cycle");
        create_dir_symlink(&sources.join("other/1.0.0"), &plugin_root.join("escape"))
            .expect("symlink an ancestor out of the plugin root");

        let mut discovery_roots = roots(fixture);
        let other_discovery = discovery_roots.pop().expect("second discovery root");
        let discovery = discovery_roots.pop().expect("discovery root");
        for root in [&discovery, &other_discovery] {
            std::fs::create_dir_all(root).expect("create discovery root");
        }
        Self {
            family,
            sources,
            discovery,
            other_discovery,
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.discovery.clone(), self.other_discovery.clone()]
    }

    fn relative_alias(&self, tail: &str) -> PathBuf {
        relative_to(
            &self.discovery,
            &self
                .sources
                .join("other")
                .join("..")
                .join("alias-graph")
                .join(tail),
        )
    }

    fn relative_alias_inside_plugin(&self, plugin_root: &Path, skill_name: &str) -> PathBuf {
        let plugin_dir = plugin_root.file_name().expect("plugin directory name");
        self.relative_alias(&format!(
            "{}/skills/{skill_name}",
            plugin_dir.to_string_lossy()
        ))
    }

    fn alias_to_skill(&self, skill_dir: &Path) -> PathBuf {
        let relative = skill_dir
            .strip_prefix(&self.family)
            .expect("skill stays inside the install family");
        self.sources.join("alias-graph").join(relative)
    }
}

fn write_user_skill(fixture: &PluginFixture) -> PathBuf {
    let user_skill = fixture.repo_root.join("user-skills/custom");
    std::fs::create_dir_all(&user_skill).expect("create user skill");
    std::fs::write(user_skill.join("SKILL.md"), "# Custom\n").expect("write user skill");
    user_skill
}

fn write_kept_file(root: &Path) {
    std::fs::create_dir_all(root.join("notes")).expect("create notes dir");
    std::fs::write(root.join("notes/README"), "keep\n").expect("write kept file");
}

fn relative_to(from_dir: &Path, target: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = target.components().collect();
    let shared = from
        .iter()
        .zip(to.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in shared..from.len() {
        relative.push("..");
    }
    for component in &to[shared..] {
        relative.push(component.as_os_str());
    }
    relative
}

fn name_set(links: &[(PathBuf, PathBuf)]) -> BTreeSet<String> {
    links.iter().map(|(link, _)| link_name(link)).collect()
}

fn name_set_of_paths(links: &[PathBuf]) -> BTreeSet<String> {
    links.iter().map(|link| link_name(link)).collect()
}

fn name_set_from(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn link_name(path: &Path) -> String {
    path.file_name()
        .expect("discovery entry has a name")
        .to_string_lossy()
        .into_owned()
}
