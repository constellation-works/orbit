//! Dependency resolution inside the shared material fingerprint [ORB-12544].
//!
//! Task ids are globally unique and a dependency may name a task owned by
//! another workspace on the same machine, so these fixtures bind two
//! workspaces into one coordination registry — the shape that made
//! `prepare_task_pilot` report an accepted prerequisite as not found.

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::TaskCreateParams;
use orbit_types::task::{
    Task, TaskPriority, TaskRelationType, TaskStatus, TaskType, task_dependencies_ready,
};
use orbit_types::workflow::automation::members::PreparationEligibility;
use tempfile::TempDir;

use super::super::preparation;
use crate::OrbitRuntime;

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

/// Two checkouts registered on one machine: `dependent` carries the git
/// history preparation fingerprints against, `owner` only has to own tasks.
struct TwoWorkspaces {
    _root: TempDir,
    dependent: OrbitRuntime,
    owner: OrbitRuntime,
    revision: String,
}

fn two_workspaces() -> TwoWorkspaces {
    let root = tempfile::tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");

    let repo_root = root.path().join("dependent");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    git(&repo_root, &["init", "--initial-branch=agent-main"]);
    git(&repo_root, &["config", "user.name", "Test"]);
    git(
        &repo_root,
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(repo_root.join("sample.txt"), "baseline").unwrap();
    git(&repo_root, &["add", "sample.txt"]);
    git(&repo_root, &["commit", "-m", "baseline"]);

    let owner_root = root.path().join("owner").join(".orbit");
    std::fs::create_dir_all(&owner_root).expect("create owner workspace root");

    let dependent =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build dependent runtime");
    let owner = OrbitRuntime::from_roots(&global_root, &owner_root).expect("build owner runtime");
    let revision = preparation::head_revision(&dependent, "agent-main").expect("head revision");

    assert_ne!(
        dependent.workspace_id().expect("dependent workspace id"),
        owner.workspace_id().expect("owner workspace id"),
        "the fixture must register two distinct workspaces"
    );

    TwoWorkspaces {
        _root: root,
        dependent,
        owner,
        revision,
    }
}

fn create_task(runtime: &OrbitRuntime, title: &str, dependencies: Vec<String>) -> Task {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".into(),
            parent_id: None,
            title: title.into(),
            description: "test".into(),
            acceptance_criteria: Vec::new(),
            dependencies,
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "test plan".into(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".into()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Proposed,
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

/// The reported defect: an accepted dependency on a task owned by another
/// workspace on this machine failed preparation with `task not found`.
/// Resolving it through its registered owner also keeps the fingerprint
/// bound to the prerequisite's meaning, so completing it invalidates the
/// preparation that was computed while it was still open.
#[test]
fn same_host_cross_workspace_dependency_prepares_and_invalidates_on_status_change() {
    let fixture = two_workspaces();
    let prerequisite = create_task(&fixture.owner, "prerequisite", Vec::new());
    let dependent = create_task(
        &fixture.dependent,
        "dependent",
        vec![prerequisite.id.clone()],
    );
    assert_eq!(
        dependent
            .relations
            .iter()
            .filter(|relation| relation.relation_type == TaskRelationType::BlockedBy)
            .map(|relation| relation.target.as_str())
            .collect::<Vec<_>>(),
        vec![prerequisite.id.as_str()],
        "the cross-workspace dependency is accepted at creation"
    );

    let before = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &PreparationEligibility::default(),
    )
    .expect("prepare a task whose prerequisite lives in another local workspace");

    // Admission reads the same prerequisite through the registry-wide status
    // projection, so preparation and readiness must agree at each step.
    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert_eq!(statuses.get(&prerequisite.id), Some(&TaskStatus::Proposed));
    assert!(
        !task_dependencies_ready(&dependent, &statuses),
        "an open prerequisite is unmet, wherever it is owned"
    );

    fixture
        .owner
        .apply_task_automation_update(
            &prerequisite.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("complete the prerequisite in its own workspace");

    let after = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &PreparationEligibility::default(),
    )
    .expect("re-prepare after the prerequisite changed");
    assert_ne!(
        before, after,
        "the prerequisite's status is material, so preparation computed before it \
         completed must not stay valid"
    );

    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert!(
        task_dependencies_ready(&dependent, &statuses),
        "readiness agrees once the prerequisite is done"
    );

    // Reading across the boundary is not authority over it: the dependent
    // workspace still lists only its own task.
    assert_eq!(
        fixture
            .dependent
            .list_tasks()
            .expect("list")
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>(),
        vec![dependent.id.clone()]
    );
    assert_eq!(
        fixture
            .owner
            .get_task(&prerequisite.id)
            .expect("owner still owns its task")
            .status,
        TaskStatus::Done
    );
}

/// A dependency this machine's registry can never resolve stays explicitly
/// unverified: preparation records the reference instead of inventing a
/// status for it, and never reports it as satisfied.
#[test]
fn another_host_dependency_is_recorded_as_unverifiable_not_satisfied() {
    let fixture = two_workspaces();
    let dependent = create_task(
        &fixture.dependent,
        "dependent",
        vec!["ZZZ-00001".to_string()],
    );

    let with_reference = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &PreparationEligibility::default(),
    )
    .expect("prepare a task referencing another host's task");

    let mut without_reference = dependent.clone();
    without_reference.relations.clear();
    let without_reference = preparation::fingerprint(
        &fixture.dependent,
        &without_reference,
        &fixture.revision,
        &PreparationEligibility::default(),
    )
    .expect("prepare the same task without the reference");
    assert_ne!(
        with_reference, without_reference,
        "an unverifiable prerequisite is still part of the task's material"
    );

    let mut satisfied = dependent.clone();
    satisfied.relations.clear();
    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert_eq!(
        statuses.get("ZZZ-00001"),
        None,
        "no status is ever invented for another host's task"
    );
    assert_eq!(
        preparation::fingerprint(
            &fixture.dependent,
            &satisfied,
            &fixture.revision,
            &PreparationEligibility::default(),
        )
        .expect("fingerprint"),
        without_reference
    );
}

/// A prerequisite under a prefix this machine does own, with nothing bound to
/// it, is gone rather than elsewhere: preparation fails closed and names it.
#[test]
fn a_deleted_prerequisite_fails_preparation_closed_with_an_actionable_reason() {
    let fixture = two_workspaces();
    let prerequisite = create_task(&fixture.owner, "prerequisite", Vec::new());
    let dependent = create_task(
        &fixture.dependent,
        "dependent",
        vec![prerequisite.id.clone()],
    );
    preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &PreparationEligibility::default(),
    )
    .expect("prepare while the prerequisite exists");

    fixture
        .owner
        .delete_task(&prerequisite.id)
        .expect("delete the prerequisite in its own workspace");

    let error = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &PreparationEligibility::default(),
    )
    .expect_err("a vanished prerequisite must not prepare");
    let message = error.to_string();
    assert!(
        message.contains(&prerequisite.id) && message.contains(&dependent.id),
        "the refusal must name both tasks: {message}"
    );
    assert!(
        message.contains("no workspace registered on this machine owns"),
        "the refusal must state why it is unresolvable: {message}"
    );

    let statuses = fixture
        .dependent
        .task_status_index()
        .expect("status projection");
    assert!(
        !task_dependencies_ready(&dependent, &statuses),
        "admission agrees the dependency is unmet rather than satisfied"
    );
}

/// The budget guard still applies to references this machine cannot resolve,
/// so an unverifiable dependency cannot buy unbounded scanning.
#[test]
fn preparation_keeps_its_dependency_budget() {
    let fixture = two_workspaces();
    let mut dependent = create_task(&fixture.dependent, "dependent", Vec::new());
    dependent.relations = (1..=51)
        .map(|number| orbit_types::task::TaskRelation {
            relation_type: TaskRelationType::BlockedBy,
            target: format!("ZZZ-{number:05}"),
        })
        .collect();

    let error = preparation::fingerprint(
        &fixture.dependent,
        &dependent,
        &fixture.revision,
        &PreparationEligibility::default(),
    )
    .expect_err("51 dependencies exceed the scan budget");
    assert!(
        error.to_string().contains("dependency_scan_budget"),
        "unexpected error: {error}"
    );
    let _ = Utc::now();
}
