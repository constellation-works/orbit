use orbit_types::desktop::*;
use orbit_types::task::{TASK_EVENTS_FILE_NAME, TaskDecision};
use serde_json::{Value, json};

use super::desktop_support::{create, session};
use super::task_support::test_runtime;

// Rewrite only receipt rows to model bytes persisted by an older process or
// notes edited independently of their typed authority. Read them back through
// the public runtime and store APIs below.
fn rewrite_receipt(runtime: &orbit_core::OrbitRuntime, id: &str, change: impl Fn(&mut Value)) {
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    let path = registry
        .find_task_binding(id)
        .unwrap()
        .unwrap()
        .canonical_path
        .join(TASK_EVENTS_FILE_NAME);
    let rows: String = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).unwrap();
            if row["type"] == "desktop_mutation" {
                change(&mut row);
            }
            format!("{row}\n")
        })
        .collect();
    std::fs::write(path, rows).unwrap();
}

fn desktop_redacts_before_receipt_and_preserves_retries() {
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let snapshot = runtime
        .desktop_task_write(create("create"), None, None, &session)
        .unwrap()
        .snapshot;
    let secret = "abc123def456ghi789SECRETTOKEN";
    let request = DesktopTaskRequest {
        request_id: "redacted".into(),
        operation: DesktopTaskOperation::Comment {
            id: snapshot.task.id.clone(),
            expected_revision: snapshot.revision,
            comment: format!("Authorization: Bearer {secret}"),
        },
    };
    let once = runtime
        .desktop_task_write(request.clone(), None, None, &session)
        .unwrap();
    let twice = runtime
        .desktop_task_write(request, None, None, &session)
        .unwrap();
    assert!(twice.replayed);
    assert_eq!(once.snapshot.revision, twice.snapshot.revision);
    assert!(
        !runtime.get_task_comments(&snapshot.task.id).unwrap()[0]
            .message
            .contains(secret)
    );
}

// Persist a pre-status-extension receipt directly: using today's serializer to
// make that receipt would not exercise compatibility with an old process.
fn desktop_legacy_edit_receipt_survives_status_contract_extension() {
    let (_root, runtime) = test_runtime();
    let agent = session(false);
    let snapshot = runtime
        .desktop_task_write(create("legacy-create"), None, None, &agent)
        .unwrap()
        .snapshot;
    let old_payload = format!(
        r#"{{"request_id":"legacy-edit","operation":{{"kind":"edit","id":{},"expected_revision":{},"fields":{{"title":"Before upgrade","description":null,"acceptance_criteria":null,"priority":null,"crew":null}}}}}}"#,
        serde_json::to_string(&snapshot.task.id).unwrap(),
        serde_json::to_string(&snapshot.revision).unwrap()
    );
    let fields = DesktopTaskFields {
        title: Some("Before upgrade".into()),
        ..Default::default()
    };
    crate::admission_audit::store_support::tasks(&runtime)
        .apply_desktop_task_mutation(
            &snapshot.task.id,
            &orbit_store::contracts::DesktopTaskMutationParams {
                actor: "codex".into(),
                request_id: "legacy-edit".into(),
                payload_digest: orbit_common::security::release::sha256_hex(old_payload.as_bytes()),
                expected_revision: snapshot.revision.clone(),
                fields: fields.clone(),
                crew_source: None,
                comment: None,
                status: None,
                append_history: Vec::new(),
            },
        )
        .unwrap();
    rewrite_receipt(&runtime, &snapshot.task.id, |row| {
        row.as_object_mut().unwrap().remove("decision");
    });
    let committed = runtime
        .desktop_task_snapshot(&snapshot.task.id, &agent)
        .unwrap();
    let result = runtime
        .desktop_task_write(
            DesktopTaskRequest {
                request_id: "legacy-edit".into(),
                operation: DesktopTaskOperation::Edit {
                    id: snapshot.task.id,
                    expected_revision: snapshot.revision,
                    fields,
                },
            },
            None,
            None,
            &agent,
        )
        .unwrap();
    assert!(
        result.replayed,
        "the persisted old payload remains the same request"
    );
    assert_eq!(result.snapshot.revision, committed.revision);
    assert_eq!(result.snapshot.task.title, "Before upgrade");
}

// A note rewrite must not invalidate a typed replay, and note text must never
// override a conflicting typed payload, identity or decision kind.
fn desktop_typed_receipts_control_runtime_and_store_replay() {
    let (_root, runtime) = test_runtime();
    let agent = session(false);
    let snapshot = runtime
        .desktop_task_write(create("typed-create"), None, None, &agent)
        .unwrap()
        .snapshot;
    let request = DesktopTaskRequest {
        request_id: "typed-edit".into(),
        operation: DesktopTaskOperation::Edit {
            id: snapshot.task.id.clone(),
            expected_revision: snapshot.revision.clone(),
            fields: DesktopTaskFields {
                title: Some("Applied once".into()),
                ..Default::default()
            },
        },
    };
    let params = orbit_store::contracts::DesktopTaskMutationParams {
        actor: "codex".into(),
        request_id: request.request_id.clone(),
        payload_digest: orbit_common::security::release::sha256_hex(
            &serde_json::to_vec(&request).unwrap(),
        ),
        expected_revision: snapshot.revision,
        fields: match &request.operation {
            DesktopTaskOperation::Edit { fields, .. } => fields.clone(),
            _ => unreachable!(),
        },
        crew_source: None,
        comment: None,
        status: None,
        append_history: Vec::new(),
    };
    runtime
        .desktop_task_write(request.clone(), None, None, &agent)
        .unwrap();
    let history = runtime.get_task_history(&snapshot.task.id).unwrap();
    let entry = history
        .iter()
        .find(|h| h.event == "desktop_mutation")
        .unwrap();
    let Some(TaskDecision::DesktopMutation(receipt)) = &entry.decision else {
        panic!("store must persist a typed receipt")
    };
    assert_eq!(receipt.request_id, params.request_id);
    assert_eq!(receipt.payload_digest, params.payload_digest);
    assert_eq!(
        entry.note.as_deref(),
        Some(
            format!(
                "desktop_request={} digest={}",
                params.request_id, params.payload_digest
            )
            .as_str()
        )
    );
    let original_decision = serde_json::to_value(entry.decision.as_ref().unwrap()).unwrap();
    let original_note = entry.note.clone().unwrap();
    let store = super::store_support::tasks(&runtime);
    for shape in [
        "typed",
        "wrong-digest",
        "foreign-decision",
        "foreign-request",
        "legacy",
        "legacy-malformed",
    ] {
        rewrite_receipt(&runtime, &snapshot.task.id, |row| {
            row["decision"] = original_decision.clone();
            row["note"] = json!(original_note);
            match shape {
                "typed" => row["note"] = json!("A person rewrote this diagnostic note."),
                "wrong-digest" => row["decision"]["payload_digest"] = json!("b".repeat(64)),
                "foreign-decision" => {
                    row["decision"] =
                        json!({"kind":"drain_approval_held", "classification":"fixture"})
                }
                "foreign-request" => row["decision"]["request_id"] = json!("another-request"),
                "legacy" | "legacy-malformed" => {
                    row.as_object_mut().unwrap().remove("decision");
                    if shape == "legacy-malformed" {
                        row["note"] = json!("desktop_request=typed-edit missing-digest");
                    }
                }
                _ => unreachable!(),
            }
        });
        let before = runtime
            .desktop_task_snapshot(&snapshot.task.id, &agent)
            .unwrap();
        let core_result = runtime.desktop_task_write(request.clone(), None, None, &agent);
        let store_result = store.apply_desktop_task_mutation(&snapshot.task.id, &params);
        match shape {
            "typed" | "legacy" => {
                assert!(core_result.unwrap().replayed, "{shape}");
                assert_eq!(
                    store_result.unwrap(),
                    orbit_store::contracts::AtomicTaskMutationOutcome::AlreadyApplied,
                    "{shape}"
                );
            }
            "foreign-request" => {
                assert!(
                    matches!(
                        core_result,
                        Err(orbit_common::OrbitError::TaskRevisionConflict { .. })
                    ),
                    "{core_result:?}"
                );
                assert_eq!(
                    store_result.unwrap(),
                    orbit_store::contracts::AtomicTaskMutationOutcome::Stale
                );
            }
            _ => {
                let reason = if shape == "foreign-decision" {
                    "mismatched decision"
                } else {
                    "request identity reused with a different payload"
                };
                assert!(
                    core_result.unwrap_err().to_string().contains(reason),
                    "{shape}"
                );
                assert!(
                    store_result.unwrap_err().to_string().contains(reason),
                    "{shape}"
                );
            }
        }
        assert_eq!(
            runtime
                .desktop_task_snapshot(&snapshot.task.id, &agent)
                .unwrap()
                .revision,
            before.revision,
            "{shape} must not mutate"
        );
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    desktop_redacts_before_receipt_and_preserves_retries_scenarios,
    desktop_redacts_before_receipt_and_preserves_retries,
    desktop_legacy_edit_receipt_survives_status_contract_extension,
    desktop_typed_receipts_control_runtime_and_store_replay
);
