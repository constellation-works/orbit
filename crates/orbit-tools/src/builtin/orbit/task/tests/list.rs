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
}
