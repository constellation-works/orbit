//! Teardown preserves durable selectors and partition ownership across failures.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_registry::workspace_registry;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

fn isolated_child(test: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_TEARDOWN_RETRY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    let home = tempdir().expect("isolated child home");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("isolated teardown retry fixture");
    test_env::assert_child_test_passed(test, output.status, &output.stdout, &output.stderr);
    true
}

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    operator: PathBuf,
    repo: PathBuf,
    orbit_dir: PathBuf,
    global_root: PathBuf,
    registry_path: PathBuf,
    partition: PathBuf,
    owned_link: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let operator = temp.path().join("operator");
        fs::create_dir_all(&home).expect("home");
        crate::git_repo::init(&repo);
        crate::git_repo::seal_lookup_boundary(&operator);
        cli(
            &repo,
            &home,
            &[
                "init",
                "--non-interactive",
                "--machine-name",
                "teardown-retry-host",
                "--task-prefix",
                "TR",
            ],
        )
        .success();
        cli(&repo, &home, &["workspace", "init", "--name", "retry"]).success();
        let shown: Value = serde_json::from_slice(
            &cli(&repo, &home, &["workspace", "show", "--format", "json"])
                .success()
                .get_output()
                .stdout,
        )
        .expect("workspace JSON");
        let repo = repo.canonicalize().expect("physical repo");
        let orbit_dir = repo.join(".orbit");
        assert_eq!(shown["registered"], true);
        assert_eq!(
            shown["checkout"]["repo_root"],
            repo.to_string_lossy().as_ref()
        );
        assert_eq!(
            shown["checkout"]["orbit_dir"],
            orbit_dir.to_string_lossy().as_ref()
        );
        cli(
            &repo,
            &home,
            &[
                "task",
                "add",
                "--title",
                "leftover",
                "--complexity",
                "low",
                "--json",
            ],
        )
        .success();

        let global_root = home.join(".orbit");
        let partition = orbit_cmd::task_store_partition_path(&global_root, "ws_retry");
        assert!(partition.is_dir(), "fixture must create a task partition");
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path).expect("catalog");
        // Model a legacy partition whose id differs from its catalog workspace.
        // A retry must find it through the task binding, rather than the catalog id.
        registry
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.name == "retry")
            .expect("workspace")
            .id = "ws_catalog".to_string();
        registry
            .checkouts
            .iter_mut()
            .find(|checkout| checkout.workspace_id == "ws_retry")
            .expect("checkout")
            .workspace_id = "ws_catalog".to_string();
        workspace_registry::save_registry_to(&registry, &registry_path).expect("cross-id catalog");

        let skill = orbit_dir.join("skills/orbit");
        fs::create_dir_all(&skill).expect("legacy skill target");
        let owned_link = repo.join(".agents/skills/orbit");
        fs::create_dir_all(owned_link.parent().expect("skills directory")).expect("skills dir");
        std::os::unix::fs::symlink(&skill, &owned_link).expect("legacy skill link");
        Self {
            _temp: temp,
            home,
            operator,
            repo,
            orbit_dir,
            global_root,
            registry_path,
            partition,
            owned_link,
        }
    }

    fn selector(&self, kind: &str) -> String {
        match kind {
            "name" => "retry".to_string(),
            "id" => "ws_catalog".to_string(),
            "path" => self.repo.to_string_lossy().into_owned(),
            _ => panic!("unknown selector kind"),
        }
    }

    fn teardown(&self, selector: &str) -> assert_cmd::assert::Assert {
        cli(
            &self.operator,
            &self.home,
            &["workspace", "teardown", selector, "--confirm"],
        )
    }

    fn assert_registered(&self) {
        let registry =
            workspace_registry::load_registry_from(&self.registry_path).expect("catalog");
        for selector in ["retry", "ws_catalog"] {
            assert_eq!(
                workspace_registry::find_workspace(&registry, selector)
                    .expect("resolve selector")
                    .expect("saved workspace must remain retryable")
                    .id,
                "ws_catalog"
            );
        }
        assert_eq!(
            workspace_registry::find_checkout_by_path(&registry, &self.repo)
                .expect("saved checkout must remain retryable")
                .workspace_id,
            "ws_catalog"
        );
    }

    fn assert_removed(&self) {
        assert!(!self.orbit_dir.exists(), "retry must delete the data root");
        assert!(
            !self.partition.exists(),
            "retry must delete the legacy partition"
        );
        assert!(
            fs::symlink_metadata(&self.owned_link).is_err(),
            "retry must remove the owned skill link"
        );
        let registry =
            workspace_registry::load_registry_from(&self.registry_path).expect("catalog");
        assert!(workspace_registry::find_workspace_by_id(&registry, "ws_catalog").is_none());
        assert!(workspace_registry::find_checkout_by_path(&registry, &self.repo).is_none());
        assert_eq!(
            orbit_cmd::bound_partition_id(&self.global_root, &self.orbit_dir)
                .expect("task binding"),
            None
        );
    }
}

fn cli(cwd: &Path, home: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_OPERATOR", "1")
        .args(args);
    command.assert()
}

struct ReadOnlyDir {
    path: PathBuf,
    original: fs::Permissions,
}

impl ReadOnlyDir {
    fn new(path: PathBuf) -> Self {
        let original = fs::metadata(&path)
            .expect("directory metadata")
            .permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).expect("make read-only");
        Self { path, original }
    }
}

impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, self.original.clone()).expect("restore permissions");
    }
}

#[test]
fn deletion_failures_keep_every_selector_and_legacy_partition_retryable() {
    if isolated_child(
        "teardown_retry::deletion_failures_keep_every_selector_and_legacy_partition_retryable",
    ) {
        return;
    }
    for stage in ["partition", "skills", "orbit"] {
        for selector_kind in ["name", "id", "path"] {
            let fixture = Fixture::new();
            let selector = fixture.selector(selector_kind);
            let saved = fs::read(&fixture.registry_path).expect("saved catalog");
            let blocked = match stage {
                "partition" => fixture.partition.join("blocked"),
                "skills" => fixture
                    .owned_link
                    .parent()
                    .expect("skills parent")
                    .to_path_buf(),
                "orbit" => fixture.orbit_dir.join("blocked"),
                _ => unreachable!(),
            };
            fs::create_dir_all(&blocked).expect("blocked directory");
            fs::write(blocked.join("leftover"), b"must survive failed deletion").expect("leftover");
            let read_only = ReadOnlyDir::new(blocked.clone());
            let failed = fixture.teardown(&selector).failure();
            let stderr = String::from_utf8_lossy(&failed.get_output().stderr);
            assert!(
                stderr.contains("Permission denied"),
                "{stage}/{selector_kind}: {stderr}"
            );
            assert_eq!(
                fs::read(&fixture.registry_path).expect("catalog after failure"),
                saved,
                "failed deletion must not persist the in-memory deregistration"
            );
            fixture.assert_registered();
            assert!(
                blocked.join("leftover").exists(),
                "failed deletion must leave its data"
            );
            if stage == "partition" {
                assert_eq!(
                    orbit_cmd::bound_partition_id(&fixture.global_root, &fixture.orbit_dir)
                        .expect("binding after failed partition deletion"),
                    Some("ws_retry".to_string()),
                    "a failed deletion must keep the legacy partition discoverable"
                );
            } else {
                assert!(
                    !fixture.partition.exists(),
                    "earlier deletion must have succeeded"
                );
            }
            drop(read_only);
            fixture.teardown(&selector).success();
            fixture.assert_removed();
        }
    }
}

#[test]
fn retry_finishes_after_deletion_before_bindings_are_saved() {
    if isolated_child("teardown_retry::retry_finishes_after_deletion_before_bindings_are_saved") {
        return;
    }
    for phase in ["partition_deleted", "local_data_deleted"] {
        for selector_kind in ["name", "id", "path"] {
            let fixture = Fixture::new();
            let selector = fixture.selector(selector_kind);
            // Model a crash after partition deletion but before task unbinding,
            // or after all local deletion but before workspace deregistration.
            if phase == "partition_deleted" {
                fs::remove_dir_all(&fixture.partition).expect("delete partition before unbinding");
            } else {
                orbit_cmd::remove_checkout_task_stores(
                    &fixture.global_root,
                    &fixture.orbit_dir,
                    Some("ws_catalog"),
                )
                .expect("delete and unbind partitions");
                fs::remove_file(&fixture.owned_link).expect("delete skill link");
                fs::remove_dir_all(&fixture.orbit_dir)
                    .expect("delete local data before catalog save");
            }
            fixture.assert_registered();
            fixture.teardown(&selector).success();
            fixture.assert_removed();
        }
    }
}

#[test]
fn teardown_does_not_follow_a_symlinked_discovery_dir_out_of_the_checkout() {
    if isolated_child(
        "teardown_retry::teardown_does_not_follow_a_symlinked_discovery_dir_out_of_the_checkout",
    ) {
        return;
    }
    let fixture = Fixture::new();
    // The registration fixture planted an owned link in a real discovery dir.
    fs::remove_file(&fixture.owned_link).expect("remove fixture link before redirecting");
    fs::remove_dir(fixture.owned_link.parent().expect("skills parent"))
        .expect("empty skills directory");
    fs::remove_dir(fixture.repo.join(".agents")).expect("empty discovery directory");
    let outside = fixture._temp.path().join("outside");
    let outside_agents = outside.join("agents");
    let outside_claude_skills = outside.join("claude-skills");
    let unrelated = outside.join("catalog/my-skill");
    fs::create_dir_all(&unrelated).expect("outside catalog skill");
    let owned_target = fixture.orbit_dir.join("skills/orbit");
    for skills in [outside_agents.join("skills"), outside_claude_skills.clone()] {
        fs::create_dir_all(&skills).expect("outside discovery directory");
        std::os::unix::fs::symlink(&owned_target, skills.join("orbit"))
            .expect("owned-looking outside link");
        std::os::unix::fs::symlink(&unrelated, skills.join("my-skill"))
            .expect("unrelated outside link");
    }
    let repo_agents = fixture.repo.join(".agents");
    let repo_claude_skills = fixture.repo.join(".claude/skills");
    fs::create_dir_all(repo_claude_skills.parent().unwrap()).expect("claude directory");
    std::os::unix::fs::symlink(&outside_agents, &repo_agents).expect("redirect agents");
    std::os::unix::fs::symlink(&outside_claude_skills, &repo_claude_skills)
        .expect("redirect claude skills");
    fixture.teardown("retry").success();
    assert!(
        !fixture.orbit_dir.exists(),
        "teardown must still delete .orbit/"
    );
    for skills in [outside_agents.join("skills"), outside_claude_skills.clone()] {
        for (name, target) in [("orbit", &owned_target), ("my-skill", &unrelated)] {
            let link = skills.join(name);
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "teardown must not delete through a redirected discovery dir"
            );
            assert_eq!(fs::read_link(link).unwrap(), *target);
        }
    }
    assert_eq!(
        fs::read_link(repo_agents).unwrap(),
        outside_agents,
        "a redirected discovery dir must survive"
    );
    assert_eq!(
        fs::read_link(repo_claude_skills).unwrap(),
        outside_claude_skills,
        "a redirected discovery dir must survive"
    );
}
