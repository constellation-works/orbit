//! The unknown-selector hint lists the whole projection vocabulary from the
//! same name list as the array, so the two cannot drift (ORB-24585).

use orbit_types::task::{TASK_SHOW_PROJECTION_FIELDS, unknown_task_show_field_message};

#[test]
fn unknown_field_message_lists_every_projection_field() {
    let message = unknown_task_show_field_message("not_a_task_field");
    let (_, valid_values) = message
        .split_once("Valid values: ")
        .expect("unknown-selector message names its valid values");
    let listed: Vec<&str> = valid_values.split(", ").collect();
    assert_eq!(
        listed, TASK_SHOW_PROJECTION_FIELDS,
        "valid-values list must match the projection vocabulary in order"
    );
    for field in TASK_SHOW_PROJECTION_FIELDS {
        assert!(
            message.contains(field),
            "valid-values list omits `{field}`: {message}"
        );
    }
    assert!(
        message.contains("completion_hold"),
        "valid-values list omits `completion_hold`: {message}"
    );
}
