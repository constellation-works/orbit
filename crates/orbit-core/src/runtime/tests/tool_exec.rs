//! Sibling tests for `tool_exec.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::OrbitRuntime;
use crate::adapter::tool_host::build_orbit_tool_host;
use crate::runtime::tool_exec::{
    ContextResolutionProbes, populate_filesystem_policy_context, resolve_task_id_from_context,
};
use orbit_store::contracts::TaskCreateParams;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use orbit_types::tool::ToolSessionContext;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn run_tool_context_allowlist_honors_task_wildcard() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: "Wildcard task".to_string(),
            description: "Exercise wildcard runtime allowlist".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task");

    let output = runtime
        .run_tool_with_context_and_role(
            "orbit.task.show",
            json!({ "id": task.id.clone() }),
            Role::Admin,
            ToolContext {
                allowed_tools: vec!["orbit.task.*".to_string()],
                orbit_host: Some(crate::adapter::tool_host::build_orbit_tool_host(
                    &runtime,
                    Some(task.id.clone()),
                    None,
                    orbit_types::tool::ToolSessionContext::default(),
                )),
                ..Default::default()
            },
        )
        .expect("wildcard activity context should permit orbit.task.show");

    assert_eq!(output["id"], task.id);
}

#[test]
fn consecutive_registered_calls_from_repo_root_skip_task_scan_and_git_probes() {
    let fixture = GitRuntimeFixture::new();
    let first = create_task(&fixture.runtime, &fixture.repo_root, "first");
    let _second = create_task(&fixture.runtime, &fixture.repo_root, "second");
    let repo_root = canonical(&fixture.repo_root);

    assert!(
        resolve_task_id_from_context(&fixture.runtime, &cwd_context(&repo_root, None, None))
            .expect("resolve task id")
            .is_none(),
        "cwd-inside-repo must not select an arbitrary first task"
    );

    let probes = ContextResolutionProbes::capture();
    for _ in 0..2 {
        // fs.read is retired; the registered execution path is the same seam.
        let output = fixture
            .runtime
            .run_tool_with_context_and_role(
                "orbit.task.show",
                json!({ "id": first.id.clone() }),
                Role::Admin,
                cwd_context(&repo_root, None, None),
            )
            .expect("registered show");
        assert_eq!(output["id"], first.id);
    }

    assert_eq!(
        probes.git_checkout_probes(),
        0,
        "stable repository-root cwd must not spawn git checkout probes"
    );
    assert_eq!(
        probes.git_common_dir_probes(),
        0,
        "stable repository-root cwd must not spawn git common-dir probes"
    );
}

#[test]
fn dry_run_and_registered_resolution_preserve_explicit_host_context() {
    let fixture = GitRuntimeFixture::new();
    let first = create_task(&fixture.runtime, &fixture.repo_root, "first");
    let shown = create_task(&fixture.runtime, &fixture.repo_root, "shown");
    let repo_root = canonical(&fixture.repo_root);
    let host = build_orbit_tool_host(
        &fixture.runtime,
        Some(first.id.clone()),
        Some("jrun-keep".to_string()),
        ToolSessionContext::default(),
    );

    let dry_run = fixture
        .runtime
        .run_tool_dry_run("orbit.task.show", &json!({ "id": shown.id.clone() }))
        .expect("dry-run");
    assert!(dry_run.missing_params.is_empty());
    assert!(
        resolve_task_id_from_context(&fixture.runtime, &cwd_context(&repo_root, None, None))
            .expect("resolve")
            .is_none()
    );

    let output = fixture
        .runtime
        .run_tool_with_context_and_role(
            "orbit.task.show",
            json!({ "id": shown.id.clone() }),
            Role::Admin,
            cwd_context(&repo_root, Some(host.clone()), None),
        )
        .expect("show with explicit host");
    assert_eq!(output["id"], shown.id);
    assert_eq!(
        host.task_scope().task_id.as_deref(),
        Some(first.id.as_str())
    );
    assert_eq!(host.task_scope().run_id.as_deref(), Some("jrun-keep"));
}

#[test]
fn task_show_does_not_enumerate_the_table_for_scope_inference() {
    let fixture = GitRuntimeFixture::new();
    let mut ids = Vec::new();
    for index in 0..12 {
        ids.push(
            create_task(
                &fixture.runtime,
                &fixture.repo_root,
                &format!("task-{index}"),
            )
            .id,
        );
    }
    let requested = ids[7].clone();
    let repo_root = canonical(&fixture.repo_root);

    // Before: list_tasks() of all 12 + get_task(first) for unused root
    // inference, then get_task(requested) for the show payload.
    // After: inference reads 0 rows; show reads the requested bundle only.
    assert!(
        resolve_task_id_from_context(&fixture.runtime, &cwd_context(&repo_root, None, None))
            .expect("resolve")
            .is_none()
    );

    let output = fixture
        .runtime
        .run_tool_with_context_and_role(
            "orbit.task.show",
            json!({ "id": requested.clone(), "fields": ["id", "title"] }),
            Role::Admin,
            cwd_context(&repo_root, None, None),
        )
        .expect("show requested task");
    assert_eq!(output["id"], requested);
    assert_eq!(output["title"], "task-7");
}

#[test]
fn root_resolution_keeps_linked_worktrees_and_rejects_unrelated_checkouts() {
    let fixture = GitRuntimeFixture::new();
    let repo_root = canonical(&fixture.repo_root);
    let linked = canonical(&fixture.linked);
    let unrelated = canonical(&fixture.unrelated);

    let probes = ContextResolutionProbes::capture();
    let mut linked_context = cwd_context(&linked, None, None);
    populate_filesystem_policy_context(&fixture.runtime, &mut linked_context)
        .expect("populate linked worktree");
    assert_eq!(
        linked_context.workspace_root.as_deref(),
        Some(linked.as_path()),
        "linked worktree cwd must remain that checkout"
    );
    assert!(
        probes.git_checkout_probes() >= 1,
        "linked worktree is outside the runtime checkout and still probes git"
    );
    assert!(probes.git_common_dir_probes() >= 1);

    let probes = ContextResolutionProbes::capture();
    let mut unrelated_context = cwd_context(&unrelated, None, None);
    populate_filesystem_policy_context(&fixture.runtime, &mut unrelated_context)
        .expect("populate unrelated checkout");
    assert_eq!(
        unrelated_context.workspace_root.as_deref(),
        Some(repo_root.as_path()),
        "unrelated checkout must not become the filesystem policy root"
    );
    assert!(
        probes.git_checkout_probes() >= 1,
        "unrelated cwd is outside the runtime checkout"
    );

    let probes = ContextResolutionProbes::capture();
    let mut supplied = cwd_context(&linked, None, Some(repo_root.clone()));
    populate_filesystem_policy_context(&fixture.runtime, &mut supplied)
        .expect("preserve supplied root");
    assert_eq!(
        supplied.workspace_root.as_deref(),
        Some(repo_root.as_path())
    );
    assert_eq!(
        probes.git_checkout_probes() + probes.git_common_dir_probes(),
        0,
        "an already-supplied workspace_root must not re-probe git"
    );
}

struct GitRuntimeFixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo_root: PathBuf,
    linked: PathBuf,
    unrelated: PathBuf,
}

impl GitRuntimeFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let linked = root.path().join("linked");
        let unrelated = root.path().join("unrelated");
        std::fs::create_dir_all(&global_root).expect("global");
        std::fs::create_dir_all(repo_root.join(".orbit")).expect("workspace");
        std::fs::create_dir_all(&unrelated).expect("unrelated");

        git(&repo_root, &["init", "-b", "agent-main"]);
        git(&repo_root, &["config", "user.name", "Orbit Test"]);
        git(
            &repo_root,
            &["config", "user.email", "orbit-test@example.com"],
        );
        git(&repo_root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo_root.join("README.md"), "init\n").expect("write");
        git(&repo_root, &["add", "README.md"]);
        git(&repo_root, &["commit", "-m", "init"]);
        git(
            &repo_root,
            &[
                "worktree",
                "add",
                linked.to_str().expect("utf8 linked path"),
                "HEAD",
            ],
        );

        git(&unrelated, &["init", "-b", "other"]);
        git(&unrelated, &["config", "user.name", "Orbit Test"]);
        git(
            &unrelated,
            &["config", "user.email", "orbit-test@example.com"],
        );
        git(&unrelated, &["config", "commit.gpgsign", "false"]);
        std::fs::write(unrelated.join("other.txt"), "other\n").expect("write unrelated");
        git(&unrelated, &["add", "other.txt"]);
        git(&unrelated, &["commit", "-m", "unrelated"]);

        let runtime = OrbitRuntime::from_roots(&global_root, &repo_root.join(".orbit"))
            .expect("build runtime");
        Self {
            _root: root,
            runtime,
            repo_root,
            linked,
            unrelated,
        }
    }
}

fn create_task(runtime: &OrbitRuntime, _workspace_path: &Path, title: &str) -> Task {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: title.to_string(),
            description: "context-resolution fixture".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task")
}

fn cwd_context(
    cwd: &Path,
    orbit_host: Option<std::sync::Arc<dyn orbit_tools::OrbitToolHost>>,
    workspace_root: Option<PathBuf>,
) -> ToolContext {
    ToolContext {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        orbit_host,
        workspace_root,
        ..Default::default()
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn git(current_dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
