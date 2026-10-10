//! Artifact diagnostics and the reviewer's list-before-read procedure, through
//! the public tool boundary. Mutable fixtures run in isolated child processes.

use orbit_core::application::task::TaskUpdateParams;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_engine::{ReviewLandingRequest, RuntimeHost};
use orbit_types::task::{ArtifactWriter, MAX_TASK_ARTIFACT_CONTENT_BYTES, TaskArtifact};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    REVIEW_BASELINE_ARTIFACT, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
    ReviewCertificate,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

#[test]
fn local_artifact_writers_cannot_forge_or_pin_review_certificates() {
    if !super::dispatch_admission::isolated(
        "artifact_tools::local_artifact_writers_cannot_forge_or_pin_review_certificates",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    let source = fixture.repo.join(".orbit/tmp/forged.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "{}").unwrap();
    let before = fixture.runtime.get_task(&fixture.task_id).unwrap();
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    let history = fixture.runtime.get_task_history(&fixture.task_id).unwrap();
    for reserved in [
        REVIEW_GATE_ARTIFACT,
        REVIEW_EVIDENCE_HOLD_ARTIFACT,
        REVIEW_MANIFEST_ARTIFACT,
        REVIEW_BASELINE_ARTIFACT,
        REVIEW_REPORT_HISTORY_ARTIFACT,
        "review-future.json",
    ] {
        for path in [
            reserved.to_string(),
            format!("./{reserved}"),
            format!(" {reserved}"),
            format!(" ./ ./{reserved}\t"),
            reserved.to_ascii_uppercase(),
        ] {
            let put = fixture.runtime.run_tool(
                "orbit.task.artifact.put",
                json!({"id": fixture.task_id, "model": "codex", "path": path, "source_path": source}),
            );
            let update = fixture.runtime.update_task_with_identity(
                &fixture.task_id,
                TaskUpdateParams {
                    description: Some("Forged document".into()),
                    comment: Some("Forged comment".into()),
                    upsert_artifacts: vec![
                        TaskArtifact::from_text("ordinary.txt", "must not persist"),
                        TaskArtifact::from_text(&path, "{}"),
                    ],
                    ..Default::default()
                },
                None,
                Some("codex".into()),
            );
            for result in [put.map(|_| ()), update.map(|_| ())] {
                assert!(
                    matches!(result, Err(OrbitError::InvalidInput(_))),
                    "{path}: {result:?}"
                );
            }
        }
    }
    // The attributed runtime entry point backs the CLI and dashboard too;
    // a human label cannot grant the deterministic system writer's authority.
    fixture
        .runtime
        .update_task_as_human(
            &fixture.task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(REVIEW_GATE_ARTIFACT, "{}")],
                ..Default::default()
            },
            "system".into(),
        )
        .expect_err("an actor label must not grant certificate authority");
    assert_eq!(fixture.runtime.get_task(&fixture.task_id).unwrap(), before);
    assert_eq!(
        fixture.runtime.get_task_comments(&fixture.task_id).unwrap(),
        comments
    );
    assert_eq!(
        fixture.runtime.get_task_history(&fixture.task_id).unwrap(),
        history
    );
    assert!(
        fixture
            .runtime
            .get_task_artifact_manifest(&fixture.task_id)
            .unwrap()
            .is_empty()
    );

    fixture.admit();
    fixture.put_report(&json!({
        "schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "accept", "summary": "Checked candidate.",
        "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
    }));
    fixture
        .settle()
        .expect("system settlement must issue its certificate");
    let stored = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
        .unwrap()
        .unwrap();
    let certificate: ReviewCertificate = serde_json::from_slice(&stored.content).unwrap();
    assert!(certificate.verdict.passed());
    let metadata = fixture
        .runtime
        .get_task_artifact_manifest(&fixture.task_id)
        .unwrap();
    assert_eq!(
        metadata
            .iter()
            .find(|file| file.path == REVIEW_GATE_ARTIFACT)
            .unwrap()
            .writer,
        Some(ArtifactWriter::System)
    );

    let mut forged = certificate.clone();
    forged.attempt_id = "forged-future-attempt".into();
    forged.issued_at += chrono::Duration::days(36500);
    std::fs::write(&source, serde_json::to_vec(&forged).unwrap()).unwrap();
    fixture.runtime.run_tool(
        "orbit.task.artifact.put",
        json!({"id": fixture.task_id, "model": "codex", "path": REVIEW_GATE_ARTIFACT, "source_path": source}),
    ).expect_err("a future-dated agent certificate must not pin the gate");
    assert_eq!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
        stored.content
    );

    fixture
        .runtime
        .record_review_landing(&ReviewLandingRequest {
            run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
            task_ids: vec![fixture.task_id.clone()],
            workspace_path: fixture.repo.clone(),
            pr_number: "1".into(),
            base: "main".into(),
            reviewed_head_sha: certificate.final_candidate.commit.clone(),
            managed_merge: true,
            landed_commit: Some(certificate.final_candidate.commit.clone()),
        })
        .unwrap();
    let landings = fixture
        .runtime
        .review_store()
        .unwrap()
        .review_landings(&certificate.attempt_id)
        .unwrap();
    assert_eq!(
        landings.len(),
        1,
        "landing must read the settlement certificate"
    );
    assert!(landings[0].covered, "{landings:?}");
}

#[test]
fn full_review_inventory_is_accepted_through_the_agent_artifact_tool() {
    if !super::dispatch_admission::isolated(
        "artifact_tools::full_review_inventory_is_accepted_through_the_agent_artifact_tool",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.repo.join(".orbit/tmp/full-review-areas.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let content = br#"{"schema_version":1,"areas":[],"exclusions":[]}"#;
    std::fs::write(&source, content).unwrap();

    fixture
        .runtime
        .run_tool(
            "orbit.task.artifact.put",
            json!({
                "id": fixture.task_id,
                "model": "codex",
                "source_path": source,
                "path": "full-review-areas.json"
            }),
        )
        .expect("the coordinator inventory name must be accepted from an agent caller");

    let stored = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, "full-review-areas.json")
        .unwrap()
        .expect("the coordinator inventory must be stored");
    assert_eq!(stored.content, content);
}

#[test]
fn normalized_reviewer_reports_are_validated_on_put_and_update() {
    if !super::dispatch_admission::isolated(
        "artifact_tools::normalized_reviewer_reports_are_validated_on_put_and_update",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    let source = fixture.repo.join(".orbit/tmp/report.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let path = " ./ review-report.json\t";
    for report in [
        json!({}),
        json!({"schema_version": 99, "attempt_id": "attempt", "verdict": "accept"}),
        json!({"schema_version": 1, "attempt_id": " ", "verdict": "accept"}),
    ] {
        std::fs::write(&source, report.to_string()).unwrap();
        let put = fixture.runtime.run_tool(
            "orbit.task.artifact.put",
            json!({"id": fixture.task_id, "model": "codex", "path": path, "source_path": source}),
        );
        let update = fixture.runtime.update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(path, report.to_string())],
                ..Default::default()
            },
            None,
            Some("codex".into()),
        );
        for result in [put.map(|_| ()), update.map(|_| ())] {
            assert!(matches!(result, Err(OrbitError::InvalidInput(_))));
        }
    }
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_REPORT_ARTIFACT)
            .unwrap()
            .is_none()
    );
    let report = json!({"schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"], "verdict": "incomplete"});
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(path, report.to_string())],
                ..Default::default()
            },
            None,
            Some("codex".into()),
        )
        .expect("the validated reviewer report exception still accepts normalized paths");
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_REPORT_ARTIFACT)
            .unwrap()
            .is_some()
    );
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_REPORT_HISTORY_ARTIFACT)
            .unwrap()
            .is_some()
    );
}

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
