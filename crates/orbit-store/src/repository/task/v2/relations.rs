use super::*;

pub(super) fn relations_from_create_params(
    params: &TaskCreateParams,
) -> Result<Vec<TaskRelation>, OrbitError> {
    let mut relations = Vec::new();
    if let Some(parent_id) = &params.parent_id {
        relations.push(TaskRelation {
            relation_type: TaskRelationType::ChildOf,
            target: parent_id.clone(),
            regression: None,
        });
    }
    for dependency in &params.dependencies {
        relations.push(TaskRelation {
            relation_type: TaskRelationType::BlockedBy,
            target: dependency.clone(),
            regression: None,
        });
    }
    if let Some(source_task_id) = &params.source_task_id {
        relations.push(TaskRelation {
            relation_type: TaskRelationType::RegressionFrom,
            target: source_task_id.clone(),
            regression: None,
        });
    }
    relations.extend(params.relations.clone());
    Ok(relations)
}

pub(super) fn replace_relations(
    relations: &mut Vec<TaskRelation>,
    relation_type: TaskRelationType,
    replacements: impl IntoIterator<Item = TaskRelation>,
) {
    relations.retain(|relation| relation.relation_type != relation_type);
    relations.extend(replacements);
}
