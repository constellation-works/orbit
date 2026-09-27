//! [ORB-12109] `workspace teardown` must not leave the deregistered
//! workspace's global task-store partition behind, and `orbit doctor` must
//! flag it if it ever does.
//!
//! [ORB-12119] The partition to delete is the one the *task registry* binds to
//! the checkout, not the one named for its workspace-catalog id, and its
//! registry bindings must be retired with it.
//!
//! [ORB-12347] The target is an explicit selector, never cwd inference.
//!
//! [ORB-13474] Legacy repo-local skill-link cleanup removes only links into the
//! checkout's own `.orbit/skills/` and never follows a redirected discovery
//! directory out of the checkout.

use std::path::{Path, PathBuf};

use chrono::Utc;
use clap::{Parser, error::ErrorKind};
use orbit_cmd::DoctorCommands;
use orbit_cmd::task_store::{bound_partition_id, partition_is_bound, task_workspaces_dir};
use orbit_common::fs::io::create_dir_symlink;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};

use crate::command::{Cli, Execute};
use crate::tests::env_isolation::EnvGuard;

use super::super::teardown::{
    WorkspaceTeardownArgs, format_deleted_partition, format_teardown_plan,
};

fn write_task_bundle(global_root: &Path, workspace_id: &str, task_id: &str) {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    std::fs::create_dir_all(&bundle).expect("create task bundle dir");
    std::fs::write(bundle.join("task.yaml"), b"id: dummy\n").expect("write bundle file");
}

fn register(
    global_root: &Path,
    workspace_id: &str,
    name: &str,
    repo_root: &Path,
    orbit_dir: &Path,
) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load registry");
    let now = Utc::now();
    workspace_registry::register_workspace(
        &mut registry,
        Workspace {
            id: workspace_id.to_string(),
            name: name.to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        },
    )
    .expect("register workspace");
    workspace_registry::register_checkout(
        &mut registry,
        WorkspaceCheckout::owner(
            workspace_id.to_string(),
            repo_root.to_path_buf(),
            orbit_dir.to_path_buf(),
        ),
    )
    .expect("register checkout");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save registry");
}

fn teardown(workspace: &str, confirm: bool) -> WorkspaceTeardownArgs {
    WorkspaceTeardownArgs {
        workspace: workspace.to_string(),
        confirm,
    }
}

#[test]
fn teardown_without_a_workspace_selector_is_usage_not_cwd_inference() {
    for args in [
        &["orbit", "workspace", "teardown"] as &[&str],
        &["orbit", "workspace", "teardown", "--confirm"],
    ] {
        let error = match Cli::try_parse_from(args.iter().copied()) {
            Ok(_) => panic!("{args:?} must not parse without a workspace selector"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument, "{error}");
        let message = error.to_string();
        assert!(
            message.contains("Usage:"),
            "missing selector must print usage, got: {message}"
        );
        assert!(
            message.contains("<WORKSPACE>"),
            "usage must name the required selector, got: {message}"
        );
    }
}

#[test]
fn teardown_rejects_a_selector_that_is_not_registered() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&orbit_dir).expect("create workspace root");
    register(&global_root, "ws_known", "known", &repo_root, &orbit_dir);

    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");
    let error = teardown("not-registered", true)
        .execute(&runtime)
        .expect_err("unregistered selector must fail");
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "unregistered selector must be invalid input: {error:?}"
    );
    assert!(
        message.contains("unknown workspace selector 'not-registered'"),
        "{message}"
    );
    assert!(
        workspace_registry::find_workspace_by_id(
            &workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
                &global_root,
            ))
            .expect("load registry"),
            "ws_known",
        )
        .is_some(),
        "a rejected selector must not deregister another workspace"
    );
}

#[test]
fn teardown_rejects_a_selector_that_does_not_match_the_cwd_checkout() {
    let temp = tempfile::tempdir().expect("tempdir");
    // `orbit init` registers the checkout as `current_dir()` reports it, which
    // is the resolved path. Register through the same canonical root here so
    // the cwd guard compares like with like: on macOS `$TMPDIR` lives under
    // the `/var` -> `/private/var` symlink, and registering the unresolved
    // path would let the mismatched selector slip past the refusal.
    let temp_root = std::fs::canonicalize(temp.path()).expect("canonical tempdir");
    let global_root = temp_root.join("global");
    let here = temp_root.join("here");
    let here_orbit = here.join(".orbit");
    let other = temp_root.join("other");
    let other_orbit = other.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&here_orbit).expect("create here workspace root");
    std::fs::create_dir_all(&other_orbit).expect("create other workspace root");
    register(&global_root, "ws_here", "here", &here, &here_orbit);
    register(&global_root, "ws_other", "other", &other, &other_orbit);
    write_task_bundle(&global_root, "ws_here", "ORB-1");
    write_task_bundle(&global_root, "ws_other", "ORB-2");

    let _env = EnvGuard::acquire().cwd(&here);
    let runtime = OrbitRuntime::from_roots(&global_root, &here_orbit).expect("build runtime");
    let error = teardown("other", true)
        .execute(&runtime)
        .expect_err("selector for a different checkout must fail");
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "cwd mismatch must be invalid input: {error:?}"
    );
    assert!(
        message.contains("does not match the checkout containing the current directory ('here')"),
        "{message}"
    );
    assert!(
        here_orbit.is_dir() && other_orbit.is_dir(),
        "a mismatched selector must not delete either checkout"
    );
    assert!(
        task_workspaces_dir(&global_root).join("ws_here").exists()
            && task_workspaces_dir(&global_root).join("ws_other").exists(),
        "a mismatched selector must not delete either task store"
    );
}

#[test]
fn teardown_without_confirm_prints_the_resolved_plan_and_does_not_delete() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("constellation");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&orbit_dir).expect("create workspace root");
    register(
        &global_root,
        "ws_orbit",
        "constellation",
        &repo_root,
        &orbit_dir,
    );
    write_task_bundle(&global_root, "ws_orbit", "ORB-1");

    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");
    let bound = bound_partition_id(&global_root, &orbit_dir)
        .expect("read checkout binding")
        .expect("checkout is bound");
    write_task_bundle(&global_root, &bound, "ORB-2");
    let partition = task_workspaces_dir(&global_root).join(&bound);

    let error = teardown("constellation", false)
        .execute(&runtime)
        .expect_err("unconfirmed teardown must refuse");
    let message = error.to_string();
    assert!(
        message.contains("workspace: constellation (ws_orbit)"),
        "plan must name catalog workspace and id: {message}"
    );
    assert!(
        message.contains(&format!("checkout: {}", repo_root.display())),
        "plan must name checkout root: {message}"
    );
    assert!(
        message.contains(&format!("task-store partition: {}", partition.display())),
        "plan must name the task-store partition path: {message}"
    );
    assert!(
        partition.exists() && orbit_dir.is_dir(),
        "unconfirmed teardown must not delete"
    );
}

#[test]
fn partition_summary_names_the_workspace_it_belonged_to() {
    let path = Path::new("/tmp/tasks/workspaces/ws_orbit");
    assert_eq!(
        format_deleted_partition(path, "constellation"),
        "deleted task store partition ws_orbit (workspace 'constellation')"
    );
}

#[test]
fn teardown_plan_names_workspace_checkout_and_partition() {
    let now = Utc::now();
    let workspace = Workspace {
        id: "ws_orbit".to_string(),
        name: "constellation".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let checkout = WorkspaceCheckout::owner(
        "ws_orbit".to_string(),
        "/repos/constellation".into(),
        "/repos/constellation/.orbit".into(),
    );
    let plan = format_teardown_plan(
        &workspace,
        &checkout,
        &[Path::new("/tmp/tasks/workspaces/ws_orbit").to_path_buf()],
    );
    assert!(
        plan.contains("workspace: constellation (ws_orbit)"),
        "{plan}"
    );
    assert!(plan.contains("checkout: /repos/constellation"), "{plan}");
    assert!(
        plan.contains("task-store partition: /tmp/tasks/workspaces/ws_orbit"),
        "{plan}"
    );
}

#[test]
fn teardown_deletes_the_bound_task_store_partition_and_retires_its_bindings() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&orbit_dir).expect("create workspace root");

    // A second, untouched registered workspace with its own task bundle,
    // proving teardown and the doctor check both stay scoped to the
    // workspace actually torn down.
    let survivor_root = temp.path().join("survivor");
    let survivor_orbit_dir = survivor_root.join(".orbit");
    std::fs::create_dir_all(&survivor_orbit_dir).expect("create survivor workspace root");
    register(
        &global_root,
        "ws_survivor",
        "ws_survivor",
        &survivor_root,
        &survivor_orbit_dir,
    );
    write_task_bundle(&global_root, "ws_survivor", "ORB-1");

    register(
        &global_root,
        "ws_teardown",
        "ws_teardown",
        &repo_root,
        &orbit_dir,
    );
    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");

    // Opening the runtime bound this checkout in the task registry. That
    // binding — not the catalog's `ws_teardown` — names the partition its task
    // state lives in.
    let bound = bound_partition_id(&global_root, &orbit_dir)
        .expect("read checkout binding")
        .expect("checkout is bound");
    assert_ne!(
        bound, "ws_teardown",
        "fixture must exercise the two distinct id spaces"
    );
    write_task_bundle(&global_root, &bound, "ORB-2");
    write_task_bundle(&global_root, &bound, "ORB-3");
    // A partition named for the catalog id, as an older binary would have left it.
    write_task_bundle(&global_root, "ws_teardown", "ORB-4");

    let bound_partition = task_workspaces_dir(&global_root).join(&bound);
    assert!(
        bound_partition.is_dir(),
        "fixture task store must exist before teardown"
    );

    teardown("ws_teardown", true)
        .execute(&runtime)
        .expect("teardown");

    assert!(
        !bound_partition.exists(),
        "teardown must delete the partition this checkout's task state is bound to"
    );
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_teardown")
            .exists(),
        "teardown must also delete a partition left under its catalog id"
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_survivor")
            .exists(),
        "teardown must not touch another workspace's task store"
    );

    // No binding may survive pointing at a deleted bundle directory.
    assert!(
        !partition_is_bound(&global_root, &bound).expect("read workspace bindings"),
        "teardown must retire the task-registry binding for the deleted partition"
    );
    assert!(
        bound_partition_id(&global_root, &orbit_dir)
            .expect("read checkout binding")
            .is_none(),
        "teardown must retire the checkout binding for the deleted orbit dir"
    );

    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &global_root,
    ))
    .expect("load registry after teardown");
    assert!(
        workspace_registry::find_workspace_by_id(&registry, "ws_teardown").is_none(),
        "teardown must still deregister the workspace"
    );

    // Doctor, run right after teardown, must report no orphaned task-store
    // partitions — the torn-down partitions are gone, and the survivor's is
    // still registered.
    let results = runtime.doctor_workspace().expect("doctor after teardown");
    let row = results
        .iter()
        .find(|row| row.check_name == "orphan-task-stores")
        .expect("orphan-task-stores row present");
    assert_eq!(row.status, orbit_cmd::WorkspaceDoctorStatus::Ok, "{row:?}");
}

#[test]
fn teardown_without_confirm_leaves_the_task_store_and_registration_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&orbit_dir).expect("create workspace root");
    register(
        &global_root,
        "ws_unconfirmed",
        "ws_unconfirmed",
        &repo_root,
        &orbit_dir,
    );
    write_task_bundle(&global_root, "ws_unconfirmed", "ORB-1");

    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");
    let result = teardown("ws_unconfirmed", false).execute(&runtime);
    assert!(matches!(result, Err(OrbitError::InvalidInput(_))));

    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_unconfirmed")
            .exists(),
        "an unconfirmed teardown must not touch the task store"
    );
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &global_root,
    ))
    .expect("load registry after unconfirmed teardown");
    assert!(
        workspace_registry::find_workspace_by_id(&registry, "ws_unconfirmed").is_some(),
        "an unconfirmed teardown must not deregister the workspace"
    );
}

/// A registered checkout whose `.orbit/skills/` holds the skill workspace init linked.
struct SkillLinkFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo_root: PathBuf,
    orbit_dir: PathBuf,
    runtime: OrbitRuntime,
}

impl SkillLinkFixture {
    fn new(workspace_id: &str) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        let global_root = root.join("global");
        let repo_root = root.join("repo");
        let orbit_dir = repo_root.join(".orbit");
        std::fs::create_dir_all(&global_root).expect("create global root");
        std::fs::create_dir_all(orbit_dir.join("skills").join("orbit")).expect("create skill");
        register(
            &global_root,
            workspace_id,
            workspace_id,
            &repo_root,
            &orbit_dir,
        );
        let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");
        Self {
            _temp: temp,
            root,
            repo_root,
            orbit_dir,
            runtime,
        }
    }

    fn owned_target(&self, name: &str) -> PathBuf {
        self.orbit_dir.join("skills").join(name)
    }
}

fn link(target: &Path, link: &Path) {
    std::fs::create_dir_all(link.parent().expect("link parent")).expect("create link parent");
    create_dir_symlink(target, link).expect("create skill link");
}

fn assert_link_to(link: &Path, target: &Path, why: &str) {
    let meta = std::fs::symlink_metadata(link)
        .unwrap_or_else(|error| panic!("{why}: {} is gone: {error}", link.display()));
    assert!(meta.file_type().is_symlink(), "{why}: {}", link.display());
    assert_eq!(
        std::fs::read_link(link).expect("read link"),
        target,
        "{why}: {}",
        link.display()
    );
}

fn assert_gone(path: &Path, why: &str) {
    assert!(
        std::fs::symlink_metadata(path).is_err(),
        "{why}: {} still exists",
        path.display()
    );
}

#[test]
fn teardown_removes_only_orbit_owned_skill_links_from_both_discovery_dirs() {
    let fixture = SkillLinkFixture::new("ws_links");
    let catalog = fixture.root.join("catalog").join("my-skill");
    let plugin_skill = fixture
        .root
        .join("plugins")
        .join("acme")
        .join("1.0.0")
        .join("skills")
        .join("review");
    std::fs::create_dir_all(&catalog).expect("create unrelated catalog skill");
    std::fs::create_dir_all(&plugin_skill).expect("create plugin skill");
    let missing = fixture.root.join("catalog").join("gone");

    for dir_name in [".agents", ".claude"] {
        let skills = fixture.repo_root.join(dir_name).join("skills");
        link(&fixture.owned_target("orbit"), &skills.join("orbit"));
        // A retired skill whose catalog entry is already gone is still Orbit's.
        link(&fixture.owned_target("retired"), &skills.join("retired"));
        link(&catalog, &skills.join("my-skill"));
        link(&missing, &skills.join("gone"));
        link(&plugin_skill, &skills.join("acme-review"));
        // Named for something else, so not the link workspace init wrote.
        link(&fixture.owned_target("orbit"), &skills.join("alias"));
        std::fs::write(skills.join("notes.md"), b"user notes\n").expect("write regular file");
        std::fs::create_dir_all(skills.join("local-skill")).expect("create skill directory");
    }

    teardown("ws_links", true)
        .execute(&fixture.runtime)
        .expect("confirmed teardown");

    assert!(
        !fixture.orbit_dir.exists(),
        "teardown must still delete .orbit/"
    );
    for dir_name in [".agents", ".claude"] {
        let skills = fixture.repo_root.join(dir_name).join("skills");
        assert_gone(&skills.join("orbit"), "an Orbit-owned link must be removed");
        assert_gone(
            &skills.join("retired"),
            "a dangling Orbit-owned link must be removed",
        );
        assert_link_to(
            &skills.join("my-skill"),
            &catalog,
            "an unrelated link must survive",
        );
        assert_link_to(
            &skills.join("gone"),
            &missing,
            "a dangling unrelated link must survive",
        );
        assert_link_to(
            &skills.join("acme-review"),
            &plugin_skill,
            "a plugin-owned link must survive",
        );
        assert_link_to(
            &skills.join("alias"),
            &fixture.owned_target("orbit"),
            "a link whose name does not match its Orbit target is not provably Orbit's",
        );
        assert!(
            skills.join("notes.md").is_file() && skills.join("local-skill").is_dir(),
            "regular files and directories in {dir_name}/skills must survive"
        );
    }
    assert!(
        catalog.is_dir() && plugin_skill.is_dir(),
        "link targets must survive"
    );
}

#[test]
fn teardown_removes_discovery_dirs_it_empties_but_keeps_unowned_empty_ones() {
    let fixture = SkillLinkFixture::new("ws_empty");
    let agents = fixture.repo_root.join(".agents");
    let claude = fixture.repo_root.join(".claude");
    link(
        &fixture.owned_target("orbit"),
        &agents.join("skills").join("orbit"),
    );
    std::fs::create_dir_all(claude.join("skills")).expect("create empty claude skills dir");

    teardown("ws_empty", true)
        .execute(&fixture.runtime)
        .expect("confirmed teardown");

    assert_gone(
        &agents,
        "a discovery dir emptied of Orbit links must be removed with its empty parent",
    );
    assert!(
        claude.join("skills").is_dir(),
        "an empty discovery dir teardown did not empty is not Orbit's to remove"
    );
}

#[test]
fn teardown_does_not_follow_a_symlinked_discovery_dir_out_of_the_checkout() {
    let fixture = SkillLinkFixture::new("ws_redirect");
    let outside = fixture.root.join("outside");
    let outside_agents = outside.join("agents");
    let outside_claude_skills = outside.join("claude-skills");
    let unrelated = outside.join("catalog").join("my-skill");
    std::fs::create_dir_all(&unrelated).expect("create outside catalog skill");

    // Entries outside the checkout, including ones shaped like Orbit-owned links.
    for skills in [outside_agents.join("skills"), outside_claude_skills.clone()] {
        link(&fixture.owned_target("orbit"), &skills.join("orbit"));
        link(&unrelated, &skills.join("my-skill"));
    }
    // `.agents` itself and `.claude/skills` each redirect outside the checkout.
    let repo_agents = fixture.repo_root.join(".agents");
    let repo_claude_skills = fixture.repo_root.join(".claude").join("skills");
    link(&outside_agents, &repo_agents);
    link(&outside_claude_skills, &repo_claude_skills);

    teardown("ws_redirect", true)
        .execute(&fixture.runtime)
        .expect("confirmed teardown");

    assert!(
        !fixture.orbit_dir.exists(),
        "teardown must still delete .orbit/"
    );
    for skills in [outside_agents.join("skills"), outside_claude_skills.clone()] {
        assert_link_to(
            &skills.join("orbit"),
            &fixture.owned_target("orbit"),
            "teardown must not delete through a redirected discovery dir",
        );
        assert_link_to(
            &skills.join("my-skill"),
            &unrelated,
            "teardown must not delete through a redirected discovery dir",
        );
    }
    assert_link_to(
        &repo_agents,
        &outside_agents,
        "a redirected discovery dir must survive",
    );
    assert_link_to(
        &repo_claude_skills,
        &outside_claude_skills,
        "a redirected discovery dir must survive",
    );
}
