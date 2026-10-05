//! Disabling a plugin unlinks only the skill links its own install family
//! owns; a link resolving outside the namespace is never removed.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use orbit_common::fs::io::create_dir_symlink;
use orbit_tools::plugin::load_plugin_dir;

use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::application::plugin::skills::{
    dangling_plugin_skill_links_in, unlink_plugin_skills_from,
};

fn roots(fixture: &PluginFixture) -> Vec<PathBuf> {
    [".agents", ".claude"]
        .into_iter()
        .map(|dir| fixture.repo_root.join(dir).join("skills"))
        .collect()
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
        // `relative_to` counts path components, so the discovery roots must be
        // spelled as the plugin root is: symlink-free. Under a symlinked temp
        // directory (macOS `/var`) the spelled root has one component fewer
        // than the physical one and every relative alias would climb one level
        // short.
        let discovery = discovery.canonicalize().expect("resolve discovery root");
        let other_discovery = other_discovery
            .canonicalize()
            .expect("resolve other discovery root");
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
