use super::*;

pub(super) fn relations_from_create_params(
    params: &TaskCreateParams,
) -> Result<Vec<TaskRelation>, OrbitError> {
    // A `dependencies` entry is stored as a `BlockedBy` edge, exactly like a
    // `blocked_by` entry in `relations`. Name both inputs in the refusal so a
    // caller supplying the same prerequisite twice knows to keep only one.
    for dependency in &params.dependencies {
        if params.relations.iter().any(|relation| {
            relation.relation_type == TaskRelationType::BlockedBy && relation.target == *dependency
        }) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task relation BlockedBy -> '{dependency}': 'dependencies' and a 'blocked_by' entry in 'relations' represent the same prerequisite edge; supply '{dependency}' once (remove it from one of the two fields)"
            )));
        }
    }
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
        let regression_targets: Vec<&str> = params
            .relations
            .iter()
            .filter(|relation| relation.relation_type == TaskRelationType::RegressionFrom)
            .map(|relation| relation.target.as_str())
            .collect();
        if regression_targets.is_empty() {
            relations.push(TaskRelation {
                relation_type: TaskRelationType::RegressionFrom,
                target: source_task_id.clone(),
                regression: None,
            });
        } else if let Some(differing) = regression_targets
            .iter()
            .find(|target| **target != source_task_id.as_str())
        {
            return Err(OrbitError::InvalidInput(format!(
                "conflicting regression sources: `source_task_id` is '{source_task_id}' but `relations` carries a `regression_from` relation to '{differing}'; set `source_task_id` and the `regression_from` entry in `relations` to the same target, or send only one of them"
            )));
        }
        // Same target in both fields: the `relations` entry below is the
        // single link, so its regression classification is preserved.
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
