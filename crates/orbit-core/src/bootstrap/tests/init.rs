//! What `orbit init` must guarantee: private directory trees, inert seeded
//! defaults that survive operator edits, and skill-link reconciliation that
//! reaps Orbit's own stale links without touching anyone else's.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::fs::io::create_dir_symlink;
use orbit_config::ConfigSeed;
use tempfile::tempdir;

use crate::application::skill::default_skill_ids;

use super::super::init::{
    InitOptions, global_skills_dir, init_workspace_at_root, orbit_layout_paths, unlink_skills,
};

#[cfg(unix)]
#[test]
fn global_and_workspace_init_create_private_directory_trees_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_PRIVATE_INIT_DIRECTORIES";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let status = std::process::Command::new("sh")
            .args(["-c", "umask 000; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("current test executable"))
            .arg("global_and_workspace_init_create_private_directory_trees_under_permissive_umask")
            .env(CHILD_MARKER, "1")
            .status()
            .expect("run test under permissive umask");
        assert!(status.success(), "permissive-umask child failed");
        return;
    }

    fn assert_private_directories<'a>(directories: impl IntoIterator<Item = &'a Path>) {
        for directory in directories {
            let mode = fs::metadata(directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode,
                0o700,
                "Orbit-owned directory {} has mode {mode:04o}",
                directory.display()
            );
        }
    }

    let temp = tempdir().expect("tempdir");
    let global_root = temp.path().join("global/.orbit");
    init_workspace_at_root(
        &global_root,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    )
    .expect("initialize global root");

    let workspace_root = temp.path().join("workspace/.orbit");
    init_workspace_at_root(
        &workspace_root,
        InitOptions {
            global_root_override: Some(global_root.clone()),
            ..Default::default()
        },
    )
    .expect("initialize workspace root");

    let global = orbit_layout_paths(&global_root);
    assert_private_directories(
        [
            &global_root,
            &global.resources_dir,
            &global.activities_dir,
            &global.jobs_dir,
            &global.executors_dir,
            &global.policies_dir,
            &global_skills_dir(&global_root),
        ]
        .into_iter()
        .map(PathBuf::as_path),
    );
    let workspace = orbit_layout_paths(&workspace_root);
    assert_private_directories(
        [
            &workspace_root,
            &workspace.resources_dir,
            &workspace.state_dir,
            &workspace.audit_dir,
            &workspace.job_runs_dir,
            &workspace.logs_dir,
            &workspace.scoreboard_dir,
            &workspace.worktrees_dir,
        ]
        .into_iter()
        .map(PathBuf::as_path),
    );
    assert!(
        !workspace.state_dir.join("diagnostics").exists(),
        "workspace init must not scaffold unused state/diagnostics"
    );
    assert!(
        !workspace_root.join("knowledge").exists(),
        "workspace init must not scaffold unused knowledge/"
    );
}

fn write_skill_dir(dir: &Path) {
    fs::create_dir_all(dir).expect("create skill dir");
    fs::write(dir.join("SKILL.md"), "# skill\n").expect("write SKILL.md");
}

#[test]
fn unlink_skills_does_not_follow_a_symlinked_discovery_directory() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("isolated");
    let global_root = root.join(".orbit");
    let skills_root = global_root.join("skills");
    let live_id = default_skill_ids()
        .into_iter()
        .next()
        .expect("at least one core skill id");
    let live_target = skills_root.join(live_id);
    write_skill_dir(&live_target);
    let live_body = fs::read_to_string(live_target.join("SKILL.md")).expect("read live skill");

    let claude_skills = root.join(".claude").join("skills");
    fs::create_dir_all(&claude_skills).expect("create real discovery dir");
    create_dir_symlink(&live_target, &claude_skills.join(live_id)).expect("link core skill");

    let foreign = temp.path().join("foreign-skills");
    fs::create_dir_all(&foreign).expect("create foreign discovery target");
    let foreign_custom = temp.path().join("foreign-custom");
    write_skill_dir(&foreign_custom);
    let foreign_body =
        fs::read_to_string(foreign_custom.join("SKILL.md")).expect("read foreign skill");
    create_dir_symlink(&foreign_custom, &foreign.join("my-skill")).expect("foreign custom link");
    create_dir_symlink(&temp.path().join("nowhere"), &foreign.join("gone"))
        .expect("foreign dangling link");
    create_dir_symlink(&live_target, &foreign.join(live_id)).expect("foreign core-shaped link");
    fs::write(foreign.join("notes.txt"), "keep-foreign").expect("foreign regular file");
    write_skill_dir(&foreign.join("operator-dir"));

    fs::create_dir_all(root.join(".agents")).expect("create agents parent");
    create_dir_symlink(&foreign, &root.join(".agents").join("skills"))
        .expect("redirect discovery dir");

    let result = unlink_skills(&global_root).expect("unlink skills");
    assert_eq!(result.removed_count, 1);
    let mut cleaned = result.cleaned_dirs.clone();
    cleaned.sort();
    let mut expected = vec![root.join(".claude"), claude_skills.clone()];
    expected.sort();
    assert_eq!(cleaned, expected);
    assert!(!claude_skills.exists());
    assert!(!root.join(".claude").exists());

    assert_eq!(
        fs::read_to_string(live_target.join("SKILL.md")).expect("core target survives"),
        live_body
    );
    assert_eq!(
        fs::read_link(foreign.join("my-skill")).expect("foreign custom link"),
        foreign_custom
    );
    assert_eq!(
        fs::read_to_string(foreign_custom.join("SKILL.md")).expect("foreign target survives"),
        foreign_body
    );
    assert_eq!(
        fs::read_link(foreign.join("gone")).expect("foreign dangling link"),
        temp.path().join("nowhere")
    );
    assert_eq!(
        fs::read_link(foreign.join(live_id)).expect("core-shaped link in foreign dir"),
        live_target
    );
    assert_eq!(
        fs::read_to_string(foreign.join("notes.txt")).expect("foreign file"),
        "keep-foreign"
    );
    assert!(foreign.join("operator-dir").join("SKILL.md").exists());
    assert_eq!(
        fs::read_link(root.join(".agents").join("skills")).expect("discovery symlink remains"),
        foreign
    );
    assert!(root.join(".agents").is_dir());
}
