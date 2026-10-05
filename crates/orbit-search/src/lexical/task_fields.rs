//! Extract the task fields indexed for lexical search.
use orbit_types::task::Task;

use super::SearchField;

pub fn task_fields(task: &Task) -> Vec<SearchField> {
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
    fields
}

fn push_field(fields: &mut Vec<SearchField>, field: impl Into<String>, text: &str) {
    if !text.trim().is_empty() {
        fields.push(SearchField::new(field, text.trim().to_string()));
    }
}
