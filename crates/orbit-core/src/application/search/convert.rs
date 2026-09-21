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

/// Complete a task hit from the task record it names.
///
/// A semantic hit carries only what the vector index stores — an ID, a field
/// name, and a score — so it arrives without the title every lexical hit has,
/// and every reader (the CLI's TITLE column, MCP callers) then shows a blank
/// where the lexical rows are populated. The record is already loaded to apply
/// status filtering, so the hit is completed from it here rather than at each
/// surface [ORB-12113].
pub(super) fn fill_task_record_fields(hit: &mut GlobalSearchHit, task: &orbit_types::task::Task) {
    hit.title = Some(task.title.clone());
    hit.summary = Some(task.description.clone());
    hit.status = Some(task.status.to_string());
}

pub(super) fn semantic_hit_to_global(hit: orbit_search::SemanticHit) -> GlobalSearchHit {
    GlobalSearchHit {
        kind: hit.source_kind,
        source: "semantic".to_string(),
        id: Some(hit.source_id),
        path: None,
        title: None,
        summary: None,
        status: None,
        best_field: Some(hit.best_field),
        snippet: Some(hit.snippet),
        score: Some(hit.score),
        score_breakdown: Some(hit.score_breakdown),
        matched_by: None,
        workspace: None,
    }
}
