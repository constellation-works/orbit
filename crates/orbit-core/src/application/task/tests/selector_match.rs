//! Path-filtered task listing matches Windows-style separators in the
//! query path against canonical selectors.

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::listing::TaskListQuery;

#[test]
fn windows_style_query_paths_match_canonical_selectors() {
    if !enter_isolated_child(
        module_path!(),
        "windows_style_query_paths_match_canonical_selectors",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Selector match".into(),
            description: "A task scoped to one source file.".into(),
            acceptance_criteria: vec!["Windows-style queries find it.".into()],
            context_files: vec!["file:src/a.rs".into()],
            ..Default::default()
        })
        .expect("add task");

    for query in [".\\src\\a.rs", "\\src\\a.rs", "src\\a.rs"] {
        let page = runtime
            .query_task_rows(&TaskListQuery {
                path: Some(query.into()),
                ..Default::default()
            })
            .expect("query tasks");
        assert!(
            page.items.iter().any(|row| row.task.id == task.id),
            "query `{query}` must match file:src/a.rs"
        );
    }

    // A trailing separator names the directory holding the selector.
    let page = runtime
        .query_task_rows(&TaskListQuery {
            path: Some("src\\".into()),
            ..Default::default()
        })
        .expect("query tasks");
    assert!(
        page.items.iter().any(|row| row.task.id == task.id),
        "query `src\\` must match the directory of file:src/a.rs"
    );
}
