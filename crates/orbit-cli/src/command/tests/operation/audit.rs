use super::*;

#[test]
fn json_error_preferences_are_derived_from_operations() {
    assert_eq!(
        operation_for(&["orbit", "tool", "run", "orbit.task.show"]).json_error_preference,
        Some(false)
    );
    assert_eq!(
        operation_for(&["orbit", "tool", "run", "orbit.task.show", "--pretty",])
            .json_error_preference,
        Some(true)
    );
    assert_eq!(
        operation_for(&["orbit", "friction", "list", "--json"]).json_error_preference,
        Some(true)
    );
    assert_eq!(
        operation_for(&["orbit", "search", "registry", "--json"]).json_error_preference,
        Some(true)
    );
    assert_eq!(
        operation_for(&["orbit", "doctor"]).json_error_preference,
        None
    );
}

#[test]
fn audit_command_is_the_only_operation_without_audit_metadata() {
    assert!(
        operation_for(&["orbit", "audit", "list"])
            .audit_meta
            .is_none()
    );
    let meta = operation_for(&["orbit", "task", "show", "ORB-10200"])
        .audit_meta
        .expect("task show is audited");
    assert_eq!(meta.command, "task");
    assert_eq!(meta.subcommand.as_deref(), Some("show"));
    assert_eq!(meta.target_id.as_deref(), Some("ORB-10200"));
}

#[test]
fn hidden_pipeline_worker_uses_bounded_bootstrap_recovery() {
    assert_eq!(
        operation_for(&["orbit", "job", "run-pipeline-worker", "jrun-child"]).runtime_need,
        RuntimeNeed::PipelineWorker
    );
}
