//! Artifact diagnostics and the reviewer's list-before-read procedure, through
//! the public tool boundary. Mutable fixtures run in isolated child processes.

use orbit_core::OrbitRuntime;
use orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

#[test]
fn artifact_errors_explain_the_source_file_listing_and_size_remedies() {
    if !super::dispatch_admission::isolated(
        "artifact_tools::artifact_errors_explain_the_source_file_listing_and_size_remedies",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let scratch = fixture.repo.join(".orbit/tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let source = scratch.join("evidence.txt");
    std::fs::write(&source, "bounded evidence").unwrap();

    for field in ["content", "file", "file_path", "src", "name"] {
        let mut input = json!({"id": fixture.task_id, "path": "evidence.txt"});
        input[field] = json!("wrong argument");
        let error = fixture
            .runtime
            .run_tool("orbit.task.artifact.put", input)
            .expect_err("incorrect artifact fields remain rejected");
        let expected = if field == "name" {
            "path"
        } else {
            "source_path"
        };
        assert_eq!(
            error.did_you_mean(),
            Some([expected.to_string()].as_slice()),
            "{field}: {error}"
        );
        assert!(error.to_string().contains(expected), "{field}: {error}");
    }
    let error = fixture
        .runtime
        .run_tool(
            "orbit.task.artifact.put",
            json!({"id": fixture.task_id, "path": "evidence.txt"}),
        )
        .expect_err("an artifact name alone cannot supply the bytes");
    for hint in ["source_path", "artifact name", ".orbit/tmp/"] {
        assert!(error.to_string().contains(hint), "{error}");
    }

    for input in [
        json!({"id": fixture.task_id}),
        json!({"id": fixture.task_id, "path": "."}),
    ] {
        let error = fixture
            .runtime
            .run_tool("orbit.task.artifact.get", input)
            .expect_err("get reads one artifact rather than listing a directory");
        for hint in ["orbit.task.show", "field: \"artifacts\""] {
            assert!(error.to_string().contains(hint), "{error}");
        }
    }

    let put = || {
        fixture.runtime.run_tool(
            "orbit.task.artifact.put",
            json!({"id": fixture.task_id, "source_path": source, "path": "evidence.txt"}),
        )
    };
    std::fs::File::create(&source)
        .unwrap()
        .set_len(MAX_TASK_ARTIFACT_CONTENT_BYTES + 1)
        .unwrap();
    let error = put().expect_err("oversized evidence must stay bounded");
    for hint in ["digest", "bounded", "gzipped excerpt"] {
        assert!(error.to_string().contains(hint), "{error}");
    }
    assert!(
        fixture
            .runtime
            .get_task_artifact_manifest(&fixture.task_id)
            .unwrap()
            .is_empty(),
        "refused inputs must not attach artifacts"
    );
    std::fs::write(&source, "bounded evidence").unwrap();
    put().expect("a complete source_path call still attaches evidence");
    let read = fixture
        .runtime
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task_id, "path": "evidence.txt"}),
        )
        .unwrap();
    assert_eq!(read["content"], "bounded evidence");
}

/// Execute the shipped review procedure's preflight against a real admitted
/// candidate, including its required manifest read and one optional listing.
fn read_review_evidence(fixture: &Fixture) -> Vec<String> {
    let call = |tool: &str, input: Value| {
        fixture
            .runtime
            .execute_tool_command(tool, input, None, Some("codex".into()))
            .unwrap()
    };
    call(
        "orbit.task.artifact.get",
        json!({"id": fixture.task_id, "path": REVIEW_MANIFEST_ARTIFACT}),
    );
    let mut fetched = vec![REVIEW_MANIFEST_ARTIFACT.to_string()];
    let listing = call(
        "orbit.task.show",
        json!({"id": fixture.task_id, "field": "artifacts"}),
    );
    let available = listing.as_array().expect("artifact metadata list");
    for path in [
        "review-evidence-hold.json",
        REVIEW_REPORT_ARTIFACT,
        "review-report-history.json",
    ] {
        if available.iter().any(|artifact| artifact["path"] == path) {
            call(
                "orbit.task.artifact.get",
                json!({"id": fixture.task_id, "path": path}),
            );
            fetched.push(path.to_string());
        }
    }
    fetched
}

fn successful_reads(runtime: &OrbitRuntime) -> usize {
    runtime
        .list_audit_events(
            None,
            Some("orbit.task.artifact.get".into()),
            None,
            None,
            100,
        )
        .unwrap()
        .into_iter()
        .inspect(|row| assert_eq!(row.status, AuditEventStatus::Success, "{row:?}"))
        .count()
}

#[test]
fn review_preflight_fetches_only_existing_optional_artifacts() {
    if !super::dispatch_admission::isolated(
        "artifact_tools::review_preflight_fetches_only_existing_optional_artifacts",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    let paths = read_review_evidence(&fixture);
    assert_eq!(
        paths,
        [REVIEW_MANIFEST_ARTIFACT],
        "a fresh review must make no get call for any absent optional artifact"
    );
    assert_eq!(
        successful_reads(&fixture.runtime),
        1,
        "only the required manifest was fetched"
    );
    fixture.put_report(&json!({
        "schema_version": 1,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete", "summary": "Retained review progress.",
        "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
    }));
    let paths = read_review_evidence(&fixture);
    assert_eq!(
        successful_reads(&fixture.runtime),
        4,
        "manifest twice plus the listed report and history"
    );
    assert!(paths.iter().any(|path| path == REVIEW_REPORT_ARTIFACT));
    assert!(
        paths
            .iter()
            .any(|path| path == "review-report-history.json")
    );
    assert!(paths.iter().all(|path| path != "review-evidence-hold.json"));
    let listings = fixture
        .runtime
        .list_audit_events(None, Some("orbit.task.show".into()), None, None, 100)
        .unwrap();
    assert_eq!(listings.len(), 2, "list exactly once per review preflight");
}
