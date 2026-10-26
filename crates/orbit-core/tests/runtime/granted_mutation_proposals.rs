//! A mutating granted tool travels as a reviewed proposal [granted-tools §4]:
//! no live change before review, one application after the gate approves,
//! and a visible failure when application cannot happen. Every case runs on
//! a disposable workspace in a temporary directory.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_types::task::{MutationProposal, MutationReceipt, MutationReceiptState};
use orbit_types::workflow::{REVIEW_MANIFEST_ARTIFACT, ReviewManifest, ReviewTiming};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

const GRANT: &str = "orbit.auto_task.update";
const DEFINITION: &str = "proposal-target";
const PROPOSAL: &str = "mutation-proposals/describe.json";

fn granted_fixture() -> Fixture {
    granted_fixture_with(ReviewTiming::BeforePr)
}

fn granted_fixture_with(timing: ReviewTiming) -> Fixture {
    let fixture = Fixture::granted(timing, &[GRANT]);
    fixture
        .runtime
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": DEFINITION,
                "description": "before",
                "schedule": {"every_minutes": 60},
                "template": {"title": "Proposal target"},
            }),
        )
        .unwrap();
    fixture
}

/// Put `content` at `path` the way the implementing agent does: through the
/// public `orbit.task.artifact.put` tool from a scratch file.
fn put(fixture: &Fixture, path: &str, content: &Value) -> Result<Value, orbit_common::OrbitError> {
    let source = fixture.repo.join(".orbit/tmp/put.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, content.to_string()).unwrap();
    fixture.runtime.run_tool(
        "orbit.task.artifact.put",
        json!({"id": fixture.task_id, "model": "codex", "path": path, "source_path": source}),
    )
}

fn describe(description: &str) -> Value {
    json!({
        "tool": GRANT,
        "input": {"name": DEFINITION, "description": description},
        "rationale": "The definition's description is stale.",
    })
}

fn definition_description(runtime: &OrbitRuntime) -> String {
    let listed = runtime
        .run_tool("orbit.auto_task.list", json!({}))
        .unwrap()
        .to_string();
    for description in ["before", "revised", "rewritten"] {
        if listed.contains(&format!("\"description\":\"{description}\"")) {
            return description.to_string();
        }
    }
    panic!("no known description in {listed}");
}

fn artifact<T: serde::de::DeserializeOwned>(fixture: &Fixture, path: &str) -> Option<T> {
    fixture
        .runtime
        .get_task_artifact(&fixture.task_id, path)
        .unwrap()
        .map(|artifact| serde_json::from_slice(&artifact.content).unwrap())
}

fn receipt(fixture: &Fixture, proposal: &MutationProposal) -> Option<MutationReceipt> {
    artifact(
        fixture,
        &format!("mutation-receipts/{}.json", proposal.idempotency_key),
    )
}

fn admitted_proposal_changes_nothing_live_case() {
    let fixture = granted_fixture();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();

    let proposal: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    assert_eq!(proposal.task_id, fixture.task_id);
    assert_eq!(proposal.tool, GRANT);
    assert_eq!(proposal.input["description"], "revised");
    assert_eq!(proposal.idempotency_key.len(), 64, "{proposal:?}");
    assert_eq!(definition_description(&fixture.runtime), "before");

    // An inner field-level gate still applies: the grant is not operator
    // authority.
    let error = put(
        &fixture,
        "mutation-proposals/toggle.json",
        &json!({
            "tool": GRANT,
            "input": {"name": DEFINITION, "enabled": false, "expected_enabled": true, "workspace": "repo"},
            "rationale": "Disable it.",
        }),
    )
    .expect_err("expected_enabled needs orbit.routine.control");
    assert!(
        error.to_string().contains("orbit.routine.control"),
        "{error}"
    );
    let error = put(
        &fixture,
        "mutation-proposals/mint.json",
        &json!({"tool": "orbit.auto_task.add", "input": {"name": "x"}, "rationale": "r"}),
    )
    .expect_err("an ungranted tool cannot be proposed");
    assert!(error.to_string().contains("does not grant"), "{error}");
    let error = put(
        &fixture,
        &format!("mutation-receipts/{}.json", proposal.idempotency_key),
        &json!({"state": "applied"}),
    )
    .expect_err("receipts are the pipeline's");
    assert!(error.to_string().contains("reserved"), "{error}");
    assert!(receipt(&fixture, &proposal).is_none());
}

fn approved_proposal_applies_once_case() {
    let mut fixture = granted_fixture();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    let proposal: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    fixture.admit();
    let manifest: ReviewManifest = artifact(&fixture, REVIEW_MANIFEST_ARTIFACT).unwrap();
    assert_eq!(manifest.mutation_proposals.len(), 1, "{manifest:?}");
    assert_eq!(manifest.mutation_proposals[0].artifact, PROPOSAL);
    assert_eq!(manifest.mutation_proposals[0].proposal, proposal);
    assert_eq!(definition_description(&fixture.runtime), "before");

    fixture.report("accept");
    fixture.settle().unwrap();
    let applied = receipt(&fixture, &proposal).expect("an applied receipt");
    assert_eq!(applied.state, MutationReceiptState::Applied, "{applied:?}");
    assert_eq!(applied.applied_input, proposal.input);
    assert_eq!(applied.proposal_artifact, PROPOSAL);
    assert_eq!(definition_description(&fixture.runtime), "revised");

    // A replayed settlement, and a retried run re-emitting the same proposal,
    // apply nothing again.
    fixture.settle().unwrap();
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    fixture.settle().unwrap();
    assert_eq!(receipt(&fixture, &proposal), Some(applied));

    // A changed proposal is a new key the settled review never saw.
    put(&fixture, PROPOSAL, &describe("rewritten")).unwrap();
    let error = fixture
        .settle()
        .expect_err("an unreviewed proposal never applies");
    assert!(
        error.to_string().contains("mutation_application_failed"),
        "{error}"
    );
    assert!(
        error.to_string().contains("after review attempt"),
        "{error}"
    );
    assert_eq!(definition_description(&fixture.runtime), "revised");
    let changed: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    assert_ne!(changed.idempotency_key, proposal.idempotency_key);
    assert!(receipt(&fixture, &changed).is_none());
}

fn failed_application_is_visible_case() {
    let mut fixture = granted_fixture();
    put(
        &fixture,
        PROPOSAL,
        &json!({
            "tool": GRANT,
            "input": {"name": "no-such-definition", "description": "revised"},
            "rationale": "Targets a definition that does not exist.",
        }),
    )
    .unwrap();
    let proposal: MutationProposal = artifact(&fixture, PROPOSAL).unwrap();
    fixture.admit();
    fixture.report("accept");

    let error = fixture
        .settle()
        .expect_err("a failed application fails the gate");
    assert!(
        error.to_string().contains("mutation_application_failed"),
        "{error}"
    );
    let failed = receipt(&fixture, &proposal).expect("a failed receipt");
    assert_eq!(failed.state, MutationReceiptState::Failed, "{failed:?}");
    assert!(failed.error.is_some(), "{failed:?}");
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains("mutation_application_failed")),
        "{comments:?}"
    );
}

fn run_without_review_refuses_proposals_case() {
    let fixture = granted_fixture_with(ReviewTiming::None);
    put(&fixture, PROPOSAL, &describe("revised")).unwrap();
    let error = fixture
        .runtime
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &fixture.input,
            Default::default(),
        )
        .expect_err("a run that reviews nothing cannot deliver a proposal");
    let message = error.to_string();
    assert!(
        message.contains("mutation_proposals_unreviewed"),
        "{message}"
    );
    assert!(message.contains(PROPOSAL), "{message}");
    assert_eq!(definition_description(&fixture.runtime), "before");
}

fn unapplied_proposal_blocks_completion_case() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Granted completion".into(),
            plan: "Propose the definition change.".into(),
            granted_tools: vec![GRANT.into()],
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
                    PROPOSAL,
                    describe("revised").to_string(),
                )],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }
    let error = runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .expect_err("a task is never done while its proposed change did not happen");
    assert!(error.to_string().contains(PROPOSAL), "{error}");
}

crate::scenario_table!(
    granted_mutation_proposal_scenarios,
    admitted_proposal_changes_nothing_live_case,
    approved_proposal_applies_once_case,
    failed_application_is_visible_case,
    run_without_review_refuses_proposals_case,
    unapplied_proposal_blocks_completion_case
);
