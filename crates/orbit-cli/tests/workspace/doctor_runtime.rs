//! Workspace doctor checks and repairs through `orbit_cmd`'s exported
//! [`DoctorCommands`] and [`doctor_report_probes`], against a runtime opened
//! on temporary roots.

use std::fs;

use orbit_cmd::{
    DoctorCommands, WorkspaceDoctorResult, WorkspaceDoctorStatus, doctor_report_probes,
};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::{TaskRelation, TaskRelationType};

const FOREIGN_TARGET: &str = "ZZZ-00001";
const ADR_TARGET: &str = "ADR-0001";

fn workspace_runtime(temp: &tempfile::TempDir) -> OrbitRuntime {
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime")
}

fn relation(relation_type: TaskRelationType, target: &str) -> TaskRelation {
    TaskRelation {
        relation_type,
        target: target.to_string(),
    }
}

fn allowed_relations() -> Vec<TaskRelation> {
    vec![
        relation(TaskRelationType::RelatedTo, FOREIGN_TARGET),
        relation(TaskRelationType::Produces, ADR_TARGET),
    ]
}

fn add_task(runtime: &OrbitRuntime, title: &str, relations: Vec<TaskRelation>) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Relation audit fixture.".to_string(),
            acceptance_criteria: vec!["Doctor reports unresolved edges.".to_string()],
            plan: "Audit canonical relations.".to_string(),
            relations,
            ..Default::default()
        })
        .expect("add task")
        .id
}

fn audit(runtime: &OrbitRuntime, workspace_id: &str) -> Vec<(String, String, String, bool)> {
    runtime
        .audit_dangling_relations(Some(workspace_id))
        .expect("audit relations")
        .into_iter()
        .map(|edge| {
            (
                edge.source_task_id,
                edge.relation_type,
                edge.target_task_id,
                edge.indexed,
            )
        })
        .collect()
}

fn task_relations_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let mut rows = doctor_report_probes()
        .find(|probe| probe.name == "task-relations")
        .expect("task-relations probe")
        .run(runtime, false);
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows.remove(0)
}

/// ORB-14181: the live edge whose generated relation row was gone made the
/// index-only audit report `task-relations` healthy while every rebuild
/// failed on it. The canonical bundle is the authority.
#[test]
fn task_relations_reports_a_canonical_dangling_edge_the_index_lost() {
    if crate::isolated_test::run_in_child(
        "doctor_runtime::task_relations_reports_a_canonical_dangling_edge_the_index_lost",
    ) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let target = add_task(&runtime, "Retired provenance", Vec::new());
    let mut relations = allowed_relations();
    relations.push(relation(TaskRelationType::SpawnedFrom, &target));
    let source = add_task(&runtime, "Spawned", relations);
    runtime.delete_task(&target).expect("delete target");
    let workspace_id = runtime.workspace_id().expect("workspace id");

    // Deleting the target drops the generated rows that named it, so the
    // edge survives only in the source's canonical bundle.
    assert_eq!(
        audit(&runtime, &workspace_id),
        vec![(
            source.clone(),
            "spawned_from".to_string(),
            target.clone(),
            false
        )],
        "an edge missing from the generated index is still reported, and marked"
    );

    let row = task_relations_row(&runtime);
    assert_eq!(
        row.status,
        WorkspaceDoctorStatus::Warning,
        "{}",
        row.message
    );
    for named in [&source, &target, "spawned_from"] {
        assert!(row.message.contains(named), "{named}: {}", row.message);
    }
    for allowed in [FOREIGN_TARGET, ADR_TARGET] {
        assert!(!row.message.contains(allowed), "{allowed}: {}", row.message);
    }
    assert!(row.remediation.is_some());

    runtime
        .update_task_as_human(
            &source,
            TaskUpdateParams {
                relations: Some(allowed_relations()),
                ..Default::default()
            },
            "operator".to_string(),
        )
        .expect("drop the dangling edge");
    assert!(audit(&runtime, &workspace_id).is_empty());
    assert_eq!(
        task_relations_row(&runtime).status,
        WorkspaceDoctorStatus::Ok
    );
}

#[cfg(unix)]
#[test]
fn retired_graph_cleanup_unlinks_boundaries_without_following_them() {
    if crate::isolated_test::run_in_child(
        "doctor_runtime::retired_graph_cleanup_unlinks_boundaries_without_following_them",
    ) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let shared_root = temp.path().join("main").join(".orbit");
    let local_root = temp.path().join("worktree").join(".orbit");
    for root in [&global_root, &shared_root, &local_root] {
        fs::create_dir_all(root).expect("create runtime root");
    }
    let runtime = OrbitRuntime::from_resolved_roots(&global_root, &shared_root, &local_root)
        .expect("build split-root runtime");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    let outside_marker = outside.join("keep.db");
    fs::write(&outside_marker, b"keep").expect("write outside marker");
    let local_graph = runtime.local_root().join("graph");
    let shared_graph = runtime.shared_root().join("knowledge/graph");
    fs::create_dir_all(shared_graph.parent().expect("knowledge parent"))
        .expect("create knowledge parent");
    std::os::unix::fs::symlink(&outside, &local_graph).expect("link local graph");
    std::os::unix::fs::symlink(&outside, &shared_graph).expect("link shared graph");

    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("remove graph links"),
        2
    );
    assert!(
        outside_marker.exists(),
        "cleanup must not follow graph symlinks"
    );
    assert!(fs::symlink_metadata(local_graph).is_err());
    assert!(fs::symlink_metadata(shared_graph).is_err());
}

#[cfg(unix)]
#[test]
fn cleanup_preserves_a_lock_held_by_a_live_process() {
    use std::os::unix::io::AsRawFd;

    if crate::isolated_test::run_in_child(
        "doctor_runtime::cleanup_preserves_a_lock_held_by_a_live_process",
    ) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let lock_path = runtime.paths().state_dir.join(".held-op.lock");
    fs::create_dir_all(lock_path.parent().expect("lock dir")).expect("create lock dir");
    // The recorded holder has exited, so only the held lock keeps the file.
    let mut holder = std::process::Command::new("true")
        .spawn()
        .expect("spawn child");
    let pid = holder.id();
    holder.wait().expect("reap child");
    fs::write(
        &lock_path,
        serde_json::json!({
            "pid": pid,
            "acquired_at": chrono::Utc::now().to_rfc3339(),
            "label": "stale metadata",
        })
        .to_string(),
    )
    .expect("write lock file");

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock file");
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "hold lock: {}", std::io::Error::last_os_error());

    assert_eq!(
        runtime
            .remove_stale_lock_files()
            .expect("clean stale locks"),
        0
    );
    assert!(lock_path.exists(), "a held lock file must remain");
    drop(file);
}
