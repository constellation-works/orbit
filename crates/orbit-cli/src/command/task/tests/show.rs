use clap::{Parser, error::ErrorKind};
use orbit_types::task::{TASK_SHOW_PROJECTION_FIELDS, TASK_SHOW_PROJECTION_FIELDS_CSV};

use crate::command::Cli;
use crate::command::task::show::normalize_task_show_fields;

#[test]
fn normalize_accepts_ordinary_top_level_fields() {
    let fields = normalize_task_show_fields(&[
        "status".to_string(),
        "terminal".to_string(),
        " id ".to_string(),
        "title".to_string(),
        "type".to_string(),
        "priority".to_string(),
        "complexity".to_string(),
        "created_at".to_string(),
        "updated_at".to_string(),
        "relations".to_string(),
        "job_run_id".to_string(),
        "external_refs".to_string(),
    ])
    .expect("ordinary top-level fields are projectable");
    assert_eq!(
        fields,
        vec![
            "status",
            "terminal",
            "id",
            "title",
            "type",
            "priority",
            "complexity",
            "created_at",
            "updated_at",
            "relations",
            "job_run_id",
            "external_refs",
        ]
    );
}

#[test]
fn normalize_accepts_derived_terminal_field() {
    assert_eq!(
        normalize_task_show_fields(&["terminal".to_string()])
            .expect("terminal is a projectable derived field"),
        ["terminal"]
    );
}

#[test]
fn normalize_rejects_unknown_fields_with_the_shared_vocabulary() {
    let error = normalize_task_show_fields(&["not_a_field".to_string()])
        .expect_err("unknown projection must fail");
    let message = error.to_string();
    assert!(message.contains("unknown field selector `not_a_field`"),);
    assert!(message.contains(TASK_SHOW_PROJECTION_FIELDS_CSV));
}

#[test]
fn task_show_help_advertises_the_authoritative_field_vocabulary() {
    let err = match Cli::try_parse_from(["orbit", "task", "show", "--help"]) {
        Ok(_) => panic!("show help should exit before parsing"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::DisplayHelp);
    let help = err.to_string();
    for field in TASK_SHOW_PROJECTION_FIELDS {
        assert!(
            help.contains(field),
            "task show help must advertise `{field}`:\n{help}"
        );
    }
    assert!(
        !help.contains("with-context"),
        "task show help must not advertise removed docs context:\n{help}"
    );
}

/// [ORB-13625] A claimed leaf's routed owner read returns the typed value it
/// asked for. History, comments and task lists are arrays, so decorating them
/// with the owning workspace would fail every one of them.
#[test]
fn worker_reads_are_returned_undecorated() {
    use crate::command::task::show::attach_bound_workspace_identity;
    use orbit_cmd::task_owner::WorkspaceIdentity;
    use serde_json::json;

    let owner = WorkspaceIdentity {
        id: "ws_orbit".into(),
        name: "orbit".into(),
    };
    let history = json!([{"event": "created"}]);
    let answered = attach_bound_workspace_identity(
        "orbit.task.show",
        &json!({"id": "ORB-1", "_worker_read": "history"}),
        Some(&owner),
        history.clone(),
    )
    .expect("a worker read passes through");
    assert_eq!(answered, history);

    let record = attach_bound_workspace_identity(
        "orbit.task.show",
        &json!({"id": "ORB-1"}),
        Some(&owner),
        json!({"id": "ORB-1"}),
    )
    .expect("an ordinary show is decorated");
    assert_eq!(record["workspace"]["id"], "ws_orbit");
}
