//! Plugin inspection uses the same workspace pins as sync and runtime loading.

use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::fs::io::create_dir_symlink;
use orbit_core::application::plugin::{
    PluginAddOptions, PluginEnableOptions, PluginUpgradeOptions, enable_plugin, install_plugin,
    plugin_doctor, sync_plugins, upgrade_plugin,
};
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::plugin::PluginStatus;

use super::dispatch_admission::isolated;

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.current_dir(repo).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn linked_worktree_doctor_validates_shared_pins_and_ignores_local_pins() {
    if !isolated(
        "plugin_inspection::linked_worktree_doctor_validates_shared_pins_and_ignores_local_pins",
    ) {
        return;
    }

    let original_cwd = std::env::current_dir().unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let fixture_root = fixture.path().canonicalize().unwrap();
    let main = fixture_root.join("main");
    let linked = fixture_root.join("linked");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init"]);
    git(
        &main,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    // Bootstrap this exact root before ordinary lookup can see an ancestor's
    // .orbit when TMPDIR is inside a managed checkout.
    init_workspace_at_root(&main.join(".orbit"), InitOptions::default())
        .expect("initialize the fixture's main workspace");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );

    let shared_root = main.join(".orbit");
    let local_root = linked.join(".orbit");
    let malformed_pins = "schemaVersion: 1\nplugins: [\n";
    std::fs::write(shared_root.join("plugins.yaml"), malformed_pins).unwrap();
    std::env::set_current_dir(&linked).unwrap();
    let runtime = OrbitRuntime::initialize().expect("initialize from a linked worktree");
    assert_eq!(runtime.shared_root(), shared_root);
    assert_eq!(runtime.paths().orbit_dir, shared_root);
    assert_eq!(runtime.paths().local_dir, local_root);

    let sync_error = sync_plugins(&runtime, true, &[]).expect_err("shared pins do not parse");
    assert!(matches!(sync_error, OrbitError::InvalidInput(_)));
    let rows = plugin_doctor(&runtime).expect("doctor returns a finding for invalid pins");
    assert_eq!(
        rows.len(),
        1,
        "invalid shared pins must produce one finding"
    );
    assert_eq!(rows[0].plugin, "pin file");
    assert_eq!(rows[0].status, PluginStatus::Inactive);
    assert!(!rows[0].intentional);
    assert_eq!(rows[0].message, sync_error.to_string());
    drop(runtime);

    // A stale worktree-local file is unrelated to the shared pins in use.
    std::fs::write(
        shared_root.join("plugins.yaml"),
        "schemaVersion: 1\nplugins: []\n",
    )
    .unwrap();
    std::fs::create_dir_all(&local_root).unwrap();
    std::fs::write(local_root.join("plugins.yaml"), malformed_pins).unwrap();
    let runtime = OrbitRuntime::initialize().expect("reload with valid shared pins");
    assert!(plugin_doctor(&runtime).unwrap().is_empty());
    assert!(sync_plugins(&runtime, true, &[]).unwrap().is_empty());
    drop(runtime);
    std::env::set_current_dir(original_cwd).unwrap();
}

struct SkillPluginFixture {
    root: tempfile::TempDir,
    runtime: OrbitRuntime,
}

impl SkillPluginFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let runtime = OrbitRuntime::from_roots(
            &root.path().join("global"),
            &root.path().join("repo/.orbit"),
        )
        .unwrap();
        Self { root, runtime }
    }

    fn source(&self, version: &str, skills: &[&str]) -> PathBuf {
        let source = self
            .root
            .path()
            .join("sources")
            .join(version)
            .join(orbit_types::plugin::PLUGIN_DIR_NAME);
        std::fs::create_dir_all(source.join("bin")).unwrap();
        std::fs::write(source.join("bin/backend.sh"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                source.join("bin/backend.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        for skill in skills {
            let directory = source.join("skills").join(skill);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("SKILL.md"), "# Fixture skill\n").unwrap();
        }
        let skill_paths: Vec<_> = skills
            .iter()
            .map(|skill| format!("skills/{skill}"))
            .collect();
        std::fs::write(
            source.join("plugin.yaml"),
            format!(
                "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: guide\n  version: {version}\n  \
                 description: Skill lifecycle fixture.\nspec:\n  backend:\n    type: exec\n    \
                 command: bin/backend.sh\n  skills: [{}]\n  tools:\n    - name: hello\n      \
                 description: Hello.\n      execution_kind: read_only\n",
                skill_paths.join(", ")
            ),
        )
        .unwrap();
        source
    }

    fn discovery_roots(&self) -> Vec<PathBuf> {
        [".agents", ".claude"]
            .into_iter()
            .map(|directory| self.root.path().join(directory).join("skills"))
            .collect()
    }
}

#[test]
fn enabled_upgrade_reconciles_dropped_and_renamed_skills() {
    if !isolated("plugin_inspection::enabled_upgrade_reconciles_dropped_and_renamed_skills") {
        return;
    }
    let fixture = SkillPluginFixture::new();
    let source = fixture.source("1.0.0", &["keep", "drop", "rename"]);
    let installed = install_plugin(
        &fixture.runtime,
        source.to_str().unwrap(),
        &PluginAddOptions {
            enable: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut old_install = PathBuf::from(installed.install_path);
    let foreign = fixture
        .root
        .path()
        .join("global/plugins/other/1.0.0/skills/guide");
    std::fs::create_dir_all(&foreign).unwrap();
    let namespace = old_install.parent().unwrap();
    let escaped = namespace.join("../other/1.0.0/skills/guide");
    for root in fixture.discovery_roots() {
        assert!(root.join("guide-drop").exists());
        create_dir_symlink(&escaped, &root.join("foreign")).unwrap();
        // A nonstandard discovery name and a relative target must still be
        // cleaned by ownership, rather than by the current manifest's names.
        let relative = PathBuf::from("../../global/plugins/guide/1.0.0/skills/drop");
        create_dir_symlink(&relative, &root.join("custom-old")).unwrap();
        std::fs::write(root.join("notes"), "keep").unwrap();
    }
    for (version, skills) in [("2.0.0", vec!["keep", "renamed"]), ("3.0.0", vec![])] {
        let source = fixture.source(version, &skills);
        let upgraded = upgrade_plugin(
            &fixture.runtime,
            "guide",
            Some(source.to_str().unwrap()),
            &PluginUpgradeOptions::default(),
        )
        .unwrap();
        assert!(upgraded.summary.host_enabled);
        assert!(!old_install.exists(), "upgrade prunes the old version");
        let current = PathBuf::from(upgraded.summary.install_path);
        for root in fixture.discovery_roots() {
            for removed in ["guide-drop", "guide-rename", "custom-old"] {
                assert!(std::fs::symlink_metadata(root.join(removed)).is_err());
            }
            for name in ["keep", "renamed"] {
                let link = root.join(format!("guide-{name}"));
                if skills.contains(&name) {
                    assert_eq!(
                        link.canonicalize().unwrap(),
                        current.join("skills").join(name).canonicalize().unwrap()
                    );
                } else {
                    assert!(std::fs::symlink_metadata(link).is_err());
                }
            }
            assert_eq!(std::fs::read_link(root.join("foreign")).unwrap(), escaped);
            assert_eq!(std::fs::read_to_string(root.join("notes")).unwrap(), "keep");
            create_dir_symlink(&current, &root.join("current-alias")).unwrap();
        }
        // Re-enable preserves custom links owned by the current install.
        let enabled =
            enable_plugin(&fixture.runtime, "guide", &PluginEnableOptions::default()).unwrap();
        assert!(enabled.warnings.is_empty(), "{:?}", enabled.warnings);
        for root in fixture.discovery_roots() {
            assert_eq!(
                std::fs::read_link(root.join("current-alias")).unwrap(),
                current
            );
        }
        // The next upgrade must clean this custom link into the old version.
        old_install = current;
    }
}

#[test]
fn doctor_reports_dangling_skill_links_across_namespace_versions() {
    if !isolated("plugin_inspection::doctor_reports_dangling_skill_links_across_namespace_versions")
    {
        return;
    }
    let fixture = SkillPluginFixture::new();
    let source = fixture.source("2.0.0", &["keep"]);
    let installed = install_plugin(
        &fixture.runtime,
        source.to_str().unwrap(),
        &PluginAddOptions {
            enable: true,
            ..Default::default()
        },
    )
    .unwrap();
    let current = PathBuf::from(installed.install_path);
    let runtime = OrbitRuntime::from_roots(
        &fixture.runtime.global_root(),
        &fixture.runtime.shared_root(),
    )
    .unwrap();
    let baseline = plugin_doctor(&runtime).unwrap();
    let mut expected = Vec::new();
    for root in fixture.discovery_roots() {
        for version in ["1.0.0", "2.0.0", "9.0.0"] {
            let target = current
                .parent()
                .unwrap()
                .join(version)
                .join("skills/missing");
            let link = root.join(format!("missing-{version}"));
            create_dir_symlink(&target, &link).unwrap();
            expected.push(link);
        }
        let foreign = fixture
            .root
            .path()
            .join("global/plugins/other/1.0.0/skills/missing");
        create_dir_symlink(&foreign, &root.join("foreign-missing")).unwrap();
    }
    let rows = plugin_doctor(&runtime).unwrap();
    let findings: Vec<_> = rows.iter().filter(|row| !baseline.contains(row)).collect();
    assert_eq!(findings.len(), expected.len(), "{rows:?}");
    for link in expected {
        assert!(
            findings.iter().any(|row| {
                row.plugin == "guide"
                    && !row.intentional
                    && row.message.contains(link.to_str().unwrap())
            }),
            "doctor must report dangling link {}: {rows:?}",
            link.display()
        );
    }
}
