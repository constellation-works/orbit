use super::types::GlobalSearchHit;

const TASK_SEARCH_SUMMARY_MAX_CHARS: usize = 240;

fn task_search_summary(description: &str) -> String {
    let mut chars = description.chars();
    let excerpt: String = chars.by_ref().take(TASK_SEARCH_SUMMARY_MAX_CHARS).collect();
    if chars.next().is_some() {
        let mut excerpt: String = excerpt
            .chars()
            .take(TASK_SEARCH_SUMMARY_MAX_CHARS - 1)
            .collect();
        excerpt.push('…');
        excerpt
    } else {
        excerpt
    }
}

pub(super) fn lexical_task_hit(task: &orbit_types::task::Task) -> GlobalSearchHit {
    GlobalSearchHit {
        kind: "task".to_string(),
        source: "lexical".to_string(),
        id: Some(task.id.clone()),
        path: None,
        title: Some(task.title.clone()),
        summary: Some(task_search_summary(&task.description)),
        status: Some(task.status.to_string()),
        best_field: None,
        snippet: None,
        score: None,
        score_breakdown: None,
        matched_by: None,
        workspace: None,
        record: None,
    }
}

pub(super) fn fill_task_record_fields(hit: &mut GlobalSearchHit, task: &orbit_types::task::Task) {
    hit.title = Some(task.title.clone());
    hit.summary = Some(task_search_summary(&task.description));
    hit.status = Some(task.status.to_string());
}
