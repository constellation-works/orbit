//! Extract the task fields indexed for lexical search.
use orbit_types::task::Task;

use super::SearchField;

/// The sidecar text a [`Task`] does not carry but task search matches on:
/// comment bodies and artifact manifest paths. Callers that index from a
/// bundle read supply it so the lexical store covers every field the
/// bundle matcher reads; the default (both empty) is the legacy coverage.
#[derive(Debug, Clone, Default)]
pub struct TaskSidecarSearchText {
    /// One entry per comment, in stored order.
    pub comments: Vec<String>,
    /// One entry per artifact manifest file.
    pub artifact_paths: Vec<String>,
}

pub fn task_fields_with_sidecars(
    task: &Task,
    sidecars: &TaskSidecarSearchText,
) -> Vec<SearchField> {
    let mut fields = Vec::new();
    push_field(&mut fields, "title", &task.title);
    push_field(&mut fields, "description", &task.description);
    push_field(&mut fields, "plan", &task.plan);
    push_field(&mut fields, "execution_summary", &task.execution_summary);
    if !task.acceptance_criteria.is_empty() {
        push_field(
            &mut fields,
            "acceptance",
            &task.acceptance_criteria.join("\n"),
        );
    }
    if !task.external_refs.is_empty() {
        // One field for every ref (`system` then `id`, as the bundle
        // matcher's envelope check reads them): chunk addresses are unique
        // per (field, chunk_idx), so one entry per ref would collide.
        let refs = task
            .external_refs
            .iter()
            .map(|external_ref| format!("{} {}", external_ref.system, external_ref.id))
            .collect::<Vec<_>>()
            .join("\n");
        push_field(&mut fields, "external_refs", &refs);
    }
    if !sidecars.comments.is_empty() {
        push_field(&mut fields, "comments", &sidecars.comments.join("\n"));
    }
    if !sidecars.artifact_paths.is_empty() {
        push_field(
            &mut fields,
            "artifact_paths",
            &sidecars.artifact_paths.join("\n"),
        );
    }
    fields
}

fn push_field(fields: &mut Vec<SearchField>, field: impl Into<String>, text: &str) {
    if !text.trim().is_empty() {
        fields.push(SearchField::new(field, text.trim().to_string()));
    }
}
