use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::{TaskRelation, TaskRelationType};

use super::*;
use crate::doctor::WorkspaceDoctorStatus;
use crate::doctor::automation::doctor_check_task_relations;

const FOREIGN_TARGET: &str = "ZZZ-00001";
const ADR_TARGET: &str = "ADR-0001";

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

/// ORB-14181: the live edge whose generated relation row was gone made the
/// index-only audit report `task-relations` healthy while every rebuild
/// failed on it. The canonical bundle is the authority.
#[test]
fn task_relations_reports_a_canonical_dangling_edge_the_index_lost() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &task_relations_reports_a_canonical_dangling_edge_the_index_lost,
    )) {
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

    let row = doctor_check_task_relations(&runtime);
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
        doctor_check_task_relations(&runtime).status,
        WorkspaceDoctorStatus::Ok
    );
}
