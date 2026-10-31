//! Multiple completed checks must leave findings readable through the artifact tool.

use orbit_core::TaskStatus;
use orbit_core::test_support::with_review_evidence_completion;
use orbit_engine::RuntimeHost;
use orbit_tools::{ReservationOwnerContext, ToolContext};
use orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES;
use orbit_types::workflow::{
    REVIEW_EVIDENCE_FINDINGS_ARTIFACT, ReviewEvidenceFinding, ReviewEvidenceFindings,
    ReviewEvidenceFlowStep,
};
use serde_json::json;

use super::{CODEQL, Stub, held, seed_persisted_hold};

#[test]
fn combined_findings_remain_readable_with_near_limit_results_and_json_escaping() {
    if !super::super::dispatch_admission::isolated(
        "review_evidence_fulfilment::bounded_findings::combined_findings_remain_readable_with_near_limit_results_and_json_escaping",
    ) {
        return;
    }
    for (case, message) in [
        ("ascii", "x".repeat(500)),
        ("escaped", "\u{0001}".repeat(500)),
    ] {
        let (fixture, mut hold) = held(Stub::Findings, CODEQL);
        // Three valid requirements with distinct evidence paths. Completed
        // analysis is injected so the public fulfilment/artifact-read boundary
        // is exercised even on hosts without Bubblewrap or CodeQL.
        hold.requirements = (0..3)
            .map(|index| {
                let mut requirement = hold.requirements[0].clone();
                requirement.artifact = format!("evidence/codeql-{index}.json");
                requirement
            })
            .collect();
        seed_persisted_hold(&fixture, &hold);
        // Short, valid path components near the per-field byte limit.
        let file = format!("src/{}finding.rs", "component/".repeat(48));
        let findings = (0..50)
            .map(|index| ReviewEvidenceFinding {
                check: hold.requirements[0].name.clone(),
                rule: format!("rust/near-limit-{index}"),
                message: message.clone(),
                file: Some(file.clone()),
                line: Some(42),
                code_flow: (0..8)
                    .map(|step| ReviewEvidenceFlowStep {
                        file: Some(file.clone()),
                        line: Some(step + 1),
                        message: Some(message.clone()),
                    })
                    .collect(),
            })
            .collect();
        let output = with_review_evidence_completion(
            findings,
            || {},
            || {},
            || {
                fixture
                    .runtime
                    .run_deterministic(
                        "fulfil_review_evidence",
                        &json!({}),
                        &json!({
                            "task_id": fixture.task_id,
                            "hold_key": format!("{}:{}", hold.attempt_id, hold.candidate.commit),
                        }),
                        ToolContext {
                            reservation_owner: Some(ReservationOwnerContext {
                                owner_run_id: hold.run_id.clone(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .unwrap()
            },
        );
        assert_eq!(output["reason"], "findings_reported", "{case}: {output}");
        assert_eq!(output["requeued"], true, "{case}: {output}");
        assert_eq!(
            fixture.runtime.get_task(&fixture.task_id).unwrap().status,
            TaskStatus::Backlog,
        );

        let read = |path: &str| {
            fixture
                .runtime
                .execute_tool_command(
                    "orbit.task.artifact.get",
                    json!({"id": fixture.task_id, "path": path}),
                    None,
                    Some("codex".into()),
                )
                .unwrap_or_else(|error| panic!("{case}: {path} must remain retrievable: {error}"))
        };
        let artifact = read(REVIEW_EVIDENCE_FINDINGS_ARTIFACT);
        let content = artifact["content"].as_str().unwrap();
        assert!(content.len() as u64 <= MAX_TASK_ARTIFACT_CONTENT_BYTES);
        let retained: ReviewEvidenceFindings = serde_json::from_str(content).unwrap();
        assert!(
            !retained.results.is_empty(),
            "{case}: retain repair evidence"
        );
        assert_eq!(
            retained.results.len() + retained.omitted,
            150,
            "{case}: every result dropped by the shared budget must be counted",
        );
        if case == "ascii" {
            assert_eq!(retained.results.len(), 50, "the global result budget");
        } else {
            assert!(
                retained.results.len() < 50,
                "escaping consumes the byte budget"
            );
        }
        assert!(retained.results.iter().all(|finding| {
            finding.message == message
                && finding.file.as_deref() == Some(file.as_str())
                && finding.code_flow.len() == 8
        }));
        for index in 0..3 {
            let log = read(&format!("evidence/codeql-{index}.log.json"));
            let log: serde_json::Value =
                serde_json::from_str(log["content"].as_str().unwrap()).unwrap();
            assert_eq!(log["findings_artifact"], REVIEW_EVIDENCE_FINDINGS_ARTIFACT);
        }
    }
}
