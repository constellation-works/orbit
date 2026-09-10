// Migrated from backend/factory.rs per ORB-00231
use orbit_types::task::{
    TaskPriority, TaskRelation, TaskRelationType, TaskStatus, TaskType, task_dependencies_ready,
};
use tempfile::TempDir;

use super::super::*;
use crate::contracts::TaskCreateParams;
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, RegisterWorkspaceParams, TaskRegistryStore, task_registry_path,
};

fn task_params(title: &str, status: TaskStatus) -> TaskCreateParams {
    TaskCreateParams {
        actor: "codex".to_string(),
        parent_id: None,
        title: title.to_string(),
        description: "coordination task".to_string(),
        acceptance_criteria: vec!["round trips".to_string()],
        dependencies: Vec::new(),
        relations: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: "1. Execute".to_string(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        workspace_path: None,
        repo_root: None,
        created_by: Some("codex".to_string()),
        planned_by: Some("codex".to_string()),
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Feature,
        external_refs: Vec::new(),
        source_task_id: None,
        crew: None,
        orchestrator: None,
        comments: Vec::new(),
    }
}

#[test]
fn workspace_task_backends_exposes_create_get_and_list_trait_surface() {
    let temp = TempDir::new().expect("tempdir");
    let registry =
        TaskRegistryStore::open(&task_registry_path(temp.path())).expect("open registry");
    let repo_dir = temp.path().join("repo");
    let orbit_dir = repo_dir.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some("orbit-test-123456".to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: repo_dir.clone(),
            workspace_path: repo_dir.clone(),
            orbit_dir: orbit_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("bind workspace");
    let backends = workspace_task_backends(
        registry,
        binding.workspace_id,
        Some(repo_dir.to_string_lossy().into_owned()),
        Some(repo_dir.to_string_lossy().into_owned()),
    );

    let created = backends
        .task
        .create_task(TaskCreateParams {
            actor: "codex:gpt-5.5".to_string(),
            parent_id: None,
            title: "Trait-created v2 task".to_string(),
            description: "A task created through the trait surface.".to_string(),
            acceptance_criteria: vec!["Round trip through trait backend".to_string()],
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: vec!["task-artifacts".to_string()],
            required_tools: Vec::new(),
            plan: "1. Exercise backend".to_string(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            workspace_path: None,
            repo_root: None,
            created_by: Some("codex:gpt-5.5".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Feature,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task");

    assert_eq!(created.id, "ORB-00000");
    assert_eq!(
        backends
            .task
            .get_task("ORB-00000")
            .expect("get task")
            .expect("task exists")
            .title,
        "Trait-created v2 task"
    );
    assert_eq!(backends.task.list_tasks().expect("list tasks").len(), 1);
}

#[test]
fn coordination_backends_create_and_schedule_across_checkoutless_workspaces() {
    let temp = TempDir::new().expect("tempdir");
    let registry =
        TaskRegistryStore::open(&task_registry_path(temp.path())).expect("open registry");
    for (workspace_id, slug) in [
        ("logical-alpha-aaaaaa", "Logical Alpha"),
        ("logical-beta-bbbbbb", "Logical Beta"),
    ] {
        registry
            .register_workspace(RegisterWorkspaceParams {
                workspace_id: workspace_id.to_string(),
                slug: slug.to_string(),
                repo_fingerprint: None,
            })
            .expect("register logical workspace");
        assert!(
            registry
                .find_workspace_checkout(workspace_id)
                .expect("find checkout")
                .is_none()
        );
    }

    let beta = coordination_task_backends(registry.clone(), "logical-beta-bbbbbb".into());
    let target = beta
        .task
        .create_task(task_params("Completed remote target", TaskStatus::Done))
        .expect("create target without checkout");
    let alpha = coordination_task_backends(registry.clone(), "logical-alpha-aaaaaa".into());
    let mut source_params = task_params("Ready cross-workspace source", TaskStatus::Backlog);
    source_params.dependencies = vec![target.id.clone()];
    source_params.relations.push(TaskRelation {
        relation_type: TaskRelationType::RelatedTo,
        target: target.id.clone(),
    });
    let source = alpha
        .task
        .create_task(source_params)
        .expect("create cross-workspace source without checkout");

    let statuses = alpha.task.task_status_index().expect("global statuses");
    assert!(task_dependencies_ready(&source, &statuses));
    assert_eq!(alpha.task.list_tasks().expect("alpha list").len(), 1);
    assert_eq!(beta.task.list_tasks().expect("beta list").len(), 1);
    assert_eq!(
        alpha
            .task
            .get_task(&source.id)
            .expect("query source")
            .expect("source exists")
            .dependencies(),
        vec![target.id]
    );

    let allocator_before = registry.allocator_next_number().expect("allocator before");
    let mut missing = task_params("Missing dependency", TaskStatus::Backlog);
    missing.dependencies = vec!["ORB-09999".into()];
    let error = alpha
        .task
        .create_task(missing)
        .expect_err("missing global target must fail");
    assert!(error.to_string().contains("ORB-09999"));
    assert!(error.to_string().contains("logical-alpha-aaaaaa"));
    assert_eq!(
        registry.allocator_next_number().expect("allocator after"),
        allocator_before
    );
    assert_eq!(alpha.task.list_tasks().expect("alpha list after").len(), 1);

    let mut foreign = task_params("Foreign dependency", TaskStatus::Backlog);
    foreign.dependencies = vec!["DK-00042".into()];
    foreign.relations.push(TaskRelation {
        relation_type: TaskRelationType::RelatedTo,
        target: "DK-00042".into(),
    });
    let foreign = alpha
        .task
        .create_task(foreign)
        .expect("foreign-prefix references are stored unverified");
    let statuses = alpha.task.task_status_index().expect("global statuses");
    assert!(
        task_dependencies_ready(&foreign, &statuses),
        "foreign dependencies cannot gate on state this machine cannot see"
    );
}
