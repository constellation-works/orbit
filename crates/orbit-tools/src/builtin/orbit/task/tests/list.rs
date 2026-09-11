//! Schema tests for `orbit.task.list`.

use super::super::list::OrbitTaskListTool;
use crate::Tool;

#[test]
fn schema_exposes_multi_status_filter() {
    let schema = OrbitTaskListTool.schema();
    let status = schema
        .parameters
        .iter()
        .find(|parameter| parameter.name == "status")
        .expect("status parameter");

    assert_eq!(status.param_type, "string_list");
    assert!(!status.required);
    assert!(
        status
            .description
            .contains("comma-separated string or an array")
    );
    assert!(schema.description.contains("{tasks, total, truncated}"));
    assert!(schema.description.contains("non-terminal tasks come first"));
    assert!(schema.description.contains("`truncated`"));
}
