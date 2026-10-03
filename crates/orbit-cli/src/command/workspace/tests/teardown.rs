//! [ORB-13474] Legacy repo-local skill-link cleanup in `workspace teardown`
//! removes only links into the checkout's own `.orbit/skills/` and never
//! follows a redirected discovery directory out of the checkout.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::fs::io::create_dir_symlink;
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};

use super::super::teardown::WorkspaceTeardownArgs;

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
        .execute_from(&fixture.runtime, &fixture.root)
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
