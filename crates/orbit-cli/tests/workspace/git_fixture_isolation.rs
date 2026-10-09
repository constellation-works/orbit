//! Workspace fixture Git setup and reads must stay off an inherited repository.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::test_env;
use tempfile::tempdir;

use crate::git_repo;

#[test]
fn workspace_git_fixtures_preserve_an_inherited_repository() {
    let temp = tempdir().expect("fixture tempdir");
    let decoy = temp.path().join("decoy");
    git_repo::init(&decoy);
    git(&decoy, &["config", "user.name", "Decoy Owner"]);
    git(&decoy, &["config", "user.email", "decoy@example.invalid"]);
    git(&decoy, &["config", "commit.gpgsign", "false"]);
    fs::write(decoy.join("sentinel"), "preserve the decoy\n").expect("write decoy file");
    git(&decoy, &["add", "sentinel"]);
    git(&decoy, &["commit", "--quiet", "-m", "decoy"]);

    let before = snapshot(&decoy);
    // Each exact child executes an existing CLI boundary fixture. Together
    // these cover every repaired setup helper, worktree creation, check-ignore,
    // and status reads without altering the parallel harness's environment.
    for test_name in [
        "config_set_root::config_set_under_root_refuses_without_workspace_layer_and_names_global",
        "replica_routines::replica_fires_only_its_worktree_gc_routine_and_owner_scheduling_is_unchanged",
        "routine_root::routine_list_honors_explicit_root_over_uninitialized_home_and_environment",
        "routine_state_seed::workspace_init_seeds_a_loadable_state_task_pilot_bound_to_this_host",
        "ship_sweep_root::ship_sweep_selects_explicit_environment_and_default_registries",
        "sweep_root::sweep_honors_explicit_root_over_uninitialized_home",
        "sweep_workspace::dry_run_sweep_evaluates_only_the_selected_workspace",
        "worktree_gc_routing::direct_gc_cli_previews_refuses_dirty_and_reclaims_only_the_selected_worktree",
        "workspace_selector::global_workspace_flag_selects_by_name_and_id_from_a_foreign_checkout",
        "workspace_sync::workspace_init_migrates_legacy_gitignore_and_reinit_is_byte_idempotent",
        "workspace_sync::relocated_root_checkout_ignores_its_delivery_worktrees",
    ] {
        let mut child = Command::new(std::env::current_exe().expect("workspace test binary"));
        test_env::clear_inherited_authority(|name| {
            child.env_remove(name);
        });
        child
            .args(["--exact", test_name, "--nocapture"])
            .env("GIT_DIR", decoy.join(".git"))
            .env("GIT_WORK_TREE", &decoy)
            .env("GIT_COMMON_DIR", decoy.join(".git"))
            .env("GIT_INDEX_FILE", decoy.join(".git/index"))
            .env("GIT_OBJECT_DIRECTORY", decoy.join(".git/objects"))
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                decoy.join(".git/objects"),
            )
            .env(test_env::SCRUBBED_MARKER_ENV, "1");
        let output = test_env::run_child_test(&mut child, test_name, temp.path());
        assert_eq!(
            snapshot(&decoy),
            before,
            "{test_name} changed the inherited repository's config, refs, index, or worktrees"
        );
        test_env::assert_child_test_passed(
            test_name,
            output.status,
            &output.stdout,
            &output.stderr,
        );
    }
}

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = git_repo::command()
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .output()
        .expect("run decoy Git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn snapshot(repo: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let git_dir = repo.join(".git");
    let mut snapshot = BTreeMap::from([
        (
            PathBuf::from("config"),
            fs::read(git_dir.join("config")).expect("decoy config"),
        ),
        (
            PathBuf::from("HEAD"),
            fs::read(git_dir.join("HEAD")).expect("decoy HEAD"),
        ),
        (
            PathBuf::from("index"),
            fs::read(git_dir.join("index")).expect("decoy index"),
        ),
        (
            PathBuf::from("worktree-list"),
            git(repo, &["worktree", "list", "--porcelain"]),
        ),
        (
            PathBuf::from("sentinel"),
            fs::read(repo.join("sentinel")).expect("decoy file"),
        ),
    ]);
    collect_files(&git_dir, &git_dir.join("refs"), &mut snapshot);
    collect_files(&git_dir, &git_dir.join("worktrees"), &mut snapshot);
    if git_dir.join("packed-refs").exists() {
        snapshot.insert(
            PathBuf::from("packed-refs"),
            fs::read(git_dir.join("packed-refs")).expect("decoy packed refs"),
        );
    }
    snapshot
}

fn collect_files(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    if !dir.exists() {
        return;
    }
    for entry in fs::read_dir(dir).expect("read decoy Git directory") {
        let entry = entry.expect("decoy Git entry");
        let path = entry.path();
        if entry.file_type().expect("decoy Git file type").is_dir() {
            collect_files(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root)
                    .expect("decoy relative path")
                    .to_path_buf(),
                fs::read(path).expect("decoy Git file"),
            );
        }
    }
}
