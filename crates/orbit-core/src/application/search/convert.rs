use super::types::GlobalSearchHit;

pub(super) fn lexical_task_hit(task: &orbit_types::task::Task) -> GlobalSearchHit {
    GlobalSearchHit {
        kind: "task".to_string(),
        source: "lexical".to_string(),
        id: Some(task.id.clone()),
        path: None,
        title: Some(task.title.clone()),
        summary: Some(task.description.clone()),
        status: Some(task.status.to_string()),
        best_field: None,
        snippet: None,
        score: None,
        score_breakdown: None,
        matched_by: None,
        workspace: None,
    }
}

pub(super) fn fill_task_record_fields(hit: &mut GlobalSearchHit, task: &orbit_types::task::Task) {
    hit.title = Some(task.title.clone());
    hit.summary = Some(task.description.clone());
    hit.status = Some(task.status.to_string());
}
