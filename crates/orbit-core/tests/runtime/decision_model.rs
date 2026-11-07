//! Model answers to decision requests: the `model` actor's citation, the
//! `model_answer_review` request it raises, human-only confirm and veto, the
//! digest's `model_answered` section, and the confirmation-only corpus rule
//! (design `docs/design/decision-assistance/specs/precedent-decisions.md`,
//! Recording, veto and feedback).

use std::path::PathBuf;

use orbit_common::DecisionRefusalCode;
use orbit_core::application::task::{ModelAnswerApplied, TaskAddParams};
use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::identity::is_automation_actor;
use orbit_types::policy::Role;
use orbit_types::task::{
    DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState, DecisionRiskTier,
    ModelAnswerCitation, ModelAnswerEnablement, ModelCitedPrecedent,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [workflow.agent_corpus]\nagent_implement = \"docs/agents/corpus/INDEX.md\"\n\n\
     [decision_channel]\nenabled = true\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";
const OPERATOR: &str = "human:operator";
const CHANNEL: &str = "email";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    workspace: PathBuf,
    runtime: OrbitRuntime,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), CONFIG).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        runtime
            .register_decision_channel(CHANNEL, OPERATOR, "decision-mail", vec![])
            .unwrap();
        Self {
            _root: root,
            global,
            workspace,
            runtime,
        }
    }

    fn operator(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.workspace)
            .unwrap()
            .with_actor(ActorIdentity::human(OPERATOR))
    }

    /// A human-crew backlog task carrying one open agent request whose
    /// options declare `tiers`.
    fn raised(&self, tiers: [&str; 2]) -> (String, DecisionRequest) {
        let task = self
            .runtime
            .add_task(TaskAddParams {
                title: "Pick the price".into(),
                description: "Fixture work".into(),
                acceptance_criteria: vec!["Done".into()],
                complexity: TaskComplexity::Low,
                crew: Some("dk".into()),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        let raised = self
            .runtime
            .run_tool(
                "orbit.decision.raise",
                json!({
                    "task_id": task,
                    "question": "Which price ships this year?",
                    "topic": "pricing/annual",
                    "model": "claude",
                    "options": [
                        {"label": "Keep the price", "consequence": "No change.", "recommended": true, "risk_tier": tiers[0]},
                        {"label": "Raise it 10%", "consequence": "The price table changes.", "risk_tier": tiers[1]},
                    ],
                }),
            )
            .unwrap();
        assert_eq!(raised["state"], "open", "{raised}");
        let request = self.request(raised["id"].as_str().unwrap());
        (task, request)
    }

    fn request(&self, id: &str) -> DecisionRequest {
        self.runtime.get_decision_request(id).unwrap().unwrap()
    }

    fn task_requests(&self, task: &str) -> Vec<DecisionRequest> {
        self.runtime
            .list_decision_requests(None, Some(task), true)
            .unwrap()
    }

    fn apply(&self, request: &DecisionRequest, answer: &str) -> ModelAnswerApplied {
        self.runtime
            .apply_model_decision_answer_for_test(&request.id, answer, citation(request))
            .unwrap()
    }

    /// `orbit.decision.digest` as the operator calls it.
    fn digest(&self) -> Value {
        self.operator()
            .run_tool_with_context_and_role(
                "orbit.decision.digest",
                json!({"channel": CHANNEL}),
                Role::Admin,
                ToolContext {
                    session_context: ToolSessionContext {
                        effective_capabilities: [McpCapability::Operator].into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap()
    }
}

fn citation(request: &DecisionRequest) -> ModelAnswerCitation {
    ModelAnswerCitation {
        assessment_id: "as-1".into(),
        logical_id: "lg-1".into(),
        category: request.category.clone().unwrap(),
        policy_version: "precedent-decision/1".into(),
        policy_digest: "sha256:policy".into(),
        question_set: "precedent-decision/1".into(),
        contract_version: "1".into(),
        adapter: "fixture".into(),
        requested_model: "decider".into(),
        returned_model: "decider-2026".into(),
        model_revision: "epoch-1".into(),
        probability: 0.97,
        threshold: 0.9,
        conflict_probability: 0.02,
        cited_precedents: vec![ModelCitedPrecedent {
            provider: "orbit-history".into(),
            precedent_id: "decision:dr-old".into(),
            decided_at: chrono::Utc::now() - chrono::Duration::days(3),
            source: "orbit".into(),
            reference: "decision:dr-old".into(),
            digest: "sha256:old".into(),
        }],
        enablement: ModelAnswerEnablement {
            operator: OPERATOR.into(),
            readout: "ORB-1/readout.json".into(),
        },
        previous_status: None,
    }
}

fn refusal_code(error: &OrbitError) -> Option<DecisionRefusalCode> {
    match error {
        OrbitError::DecisionRefused { code, .. } => Some(*code),
        _ => None,
    }
}

fn corpus_appends(fx: &Fixture, task: &str) -> Vec<Value> {
    fx.runtime
        .agent_corpus_append_proposals(task)
        .unwrap()
        .into_iter()
        .map(|append| append.entry)
        .collect()
}

fn model_answer_records_citation_raises_one_review_and_is_never_a_human_answer() {
    let fx = Fixture::new();
    let (task, request) = fx.raised(["routine", "reversible"]);
    assert!(is_automation_actor("model"));

    let applied = fx.apply(&request, "1");
    let answered = &applied.request;
    assert_eq!(answered.state, DecisionRequestState::Answered);
    let resolution = answered.resolution.as_ref().unwrap();
    assert_eq!(resolution.answered_by, "model");
    assert_eq!(resolution.answer_id, "1");
    assert!(resolution.confirmation.is_none());
    let model = resolution.model.as_ref().expect("model citation recorded");
    assert_eq!(model.previous_status, Some(TaskStatus::Backlog));
    assert_eq!(model.assessment_id, "as-1");
    assert_eq!(model.cited_precedents.len(), 1);
    assert_eq!(model.enablement.readout, "ORB-1/readout.json");
    assert_eq!(fx.runtime.get_task(&task).unwrap().status, TaskStatus::Done);

    let review = &applied.review;
    assert_eq!(review.gate, DecisionGate::ModelAnswerReview);
    assert_eq!(review.state, DecisionRequestState::Open);
    assert_eq!(
        review.binding,
        DecisionBinding::ModelAnswer {
            request_id: request.id.clone(),
            answered_at: resolution.answered_at,
        }
    );
    let tiers: Vec<_> = review
        .answers
        .iter()
        .map(|answer| (answer.id.as_str(), answer.risk_tier))
        .collect();
    assert_eq!(
        tiers,
        [
            ("confirm", DecisionRiskTier::Routine),
            ("veto", DecisionRiskTier::Reversible)
        ]
    );
    let reviews: Vec<_> = fx
        .task_requests(&task)
        .into_iter()
        .filter(|candidate| candidate.gate == DecisionGate::ModelAnswerReview)
        .collect();
    assert_eq!(reviews.len(), 1, "exactly one review request: {reviews:?}");
    let shown = fx
        .runtime
        .run_tool(
            "orbit.decision.show",
            json!({"id": request.id, "model": "claude"}),
        )
        .unwrap();
    assert_eq!(shown["resolution"]["answered_by"], "model", "{shown}");
    assert_eq!(shown["resolution"]["model"]["probability"], 0.97, "{shown}");

    // The model answer itself appends nothing to the corpus outbox.
    assert!(corpus_appends(&fx, &task).is_empty());

    // The model never answers its own review, nor answers twice.
    let own_review = fx
        .runtime
        .apply_model_decision_answer_for_test(&review.id, "confirm", citation(review))
        .expect_err("the model never answers a review");
    assert_eq!(
        refusal_code(&own_review),
        Some(DecisionRefusalCode::CallerNotAllowed)
    );
    let twice = fx
        .runtime
        .apply_model_decision_answer_for_test(&request.id, "2", citation(&request))
        .expect_err("an answered request is refused");
    assert_eq!(
        refusal_code(&twice),
        Some(DecisionRefusalCode::RequestSuperseded)
    );

    // Every human-only answer path refuses the model and agent labels.
    for label in ["model", "claude"] {
        let refused = fx
            .runtime
            .answer_decision_request_as_human(&review.id, "confirm", None, label.into(), None)
            .expect_err("only a human answers the review");
        assert_eq!(
            refusal_code(&refused),
            Some(DecisionRefusalCode::CallerNotAllowed),
            "{label}: {refused}"
        );
    }
    let as_model = fx
        .runtime
        .reverse_decision_answer(&request.id, "model")
        .expect_err("the model cannot reverse");
    assert!(as_model.to_string().contains("operator"), "{as_model}");
    assert!(
        fx.runtime
            .run_tool(
                "orbit.decision.answer",
                json!({"id": review.id, "answer": "veto"})
            )
            .is_err(),
        "an agent tool call cannot veto"
    );
    assert_eq!(fx.request(&review.id).state, DecisionRequestState::Open);
    assert_eq!(fx.runtime.get_task(&task).unwrap().status, TaskStatus::Done);
}

fn model_answer_is_refused_for_non_self_answerable_tiers() {
    let fx = Fixture::new();
    let (task, request) = fx.raised(["routine", "irreversible"]);
    assert_eq!(request.risk_tier, DecisionRiskTier::Irreversible);
    let refused = fx
        .runtime
        .apply_model_decision_answer_for_test(&request.id, "1", citation(&request))
        .expect_err("an irreversible request goes to the human");
    assert_eq!(
        refusal_code(&refused),
        Some(DecisionRefusalCode::AnswerNotApplicable)
    );
    let other = fx
        .runtime
        .add_task(TaskAddParams {
            title: "Undeclared".into(),
            description: "Fixture work".into(),
            acceptance_criteria: vec!["Done".into()],
            complexity: TaskComplexity::Low,
            crew: Some("dk".into()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    // One option leaves its tier undeclared, so the request reads unknown.
    let raised = fx
        .runtime
        .run_tool(
            "orbit.decision.raise",
            json!({
                "task_id": other,
                "question": "Which colour?",
                "model": "claude",
                "options": [
                    {"label": "Red", "consequence": "Red.", "recommended": true, "risk_tier": "routine"},
                    {"label": "Blue", "consequence": "Blue."},
                ],
            }),
        )
        .unwrap();
    let undeclared = fx.request(raised["id"].as_str().unwrap());
    assert_eq!(undeclared.risk_tier, DecisionRiskTier::Unknown);
    let refused = fx
        .runtime
        .apply_model_decision_answer_for_test(&undeclared.id, "1", citation(&undeclared))
        .expect_err("an unknown tier goes to the human");
    assert_eq!(
        refusal_code(&refused),
        Some(DecisionRefusalCode::AnswerNotApplicable)
    );
    assert_eq!(fx.request(&request.id).state, DecisionRequestState::Open);
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Backlog
    );
    assert!(
        fx.task_requests(&task)
            .iter()
            .all(|candidate| candidate.gate != DecisionGate::ModelAnswerReview)
    );
}

fn confirm_marks_the_model_answer_confirmed_and_appends_the_confirming_human() {
    let fx = Fixture::new();
    let (task, request) = fx.raised(["routine", "reversible"]);
    let applied = fx.apply(&request, "1");

    let digest = fx.digest();
    let rows = digest["model_answered"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{digest}");
    assert_eq!(rows[0]["review"]["id"], applied.review.id.as_str());
    assert_eq!(rows[0]["request"]["id"], request.id.as_str());
    assert_eq!(rows[0]["request"]["resolution"]["answer_id"], "1");
    assert_eq!(
        rows[0]["request"]["resolution"]["model"]["cited_precedents"][0]["precedent_id"],
        "decision:dr-old"
    );
    assert!(
        digest["expiring"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["gate"] != "model_answer_review"),
        "the review is listed only in model_answered: {digest}"
    );

    let reviewed = fx
        .operator()
        .answer_decision_request_as_human(
            &applied.review.id,
            "confirm",
            None,
            OPERATOR.into(),
            Some("cli".into()),
        )
        .unwrap();
    assert_eq!(reviewed.state, DecisionRequestState::Answered);
    let original = fx.request(&request.id);
    assert_eq!(original.state, DecisionRequestState::Answered);
    let resolution = original.resolution.unwrap();
    assert_eq!(resolution.answered_by, "model");
    let confirmation = resolution.confirmation.expect("confirmed");
    assert_eq!(confirmation.confirmed_by, OPERATOR);
    assert_eq!(confirmation.answered_at, resolution.answered_at);
    assert_eq!(fx.runtime.get_task(&task).unwrap().status, TaskStatus::Done);

    let appends = corpus_appends(&fx, &task);
    assert_eq!(appends.len(), 1, "{appends:?}");
    assert_eq!(appends[0]["request"], request.id.as_str());
    assert_eq!(appends[0]["human"], OPERATOR);
    assert_eq!(appends[0]["chosen"], "1");

    let digest = fx.digest();
    assert!(
        digest["model_answered"].as_array().unwrap().is_empty(),
        "{digest}"
    );
}

fn veto_reopens_the_original_request_and_restores_the_previous_status() {
    let fx = Fixture::new();
    let (task, request) = fx.raised(["routine", "reversible"]);
    let applied = fx.apply(&request, "1");
    assert_eq!(fx.runtime.get_task(&task).unwrap().status, TaskStatus::Done);

    let reviewed = fx
        .operator()
        .answer_decision_request_as_human(
            &applied.review.id,
            "veto",
            Some("Wrong call".into()),
            OPERATOR.into(),
            Some("cli".into()),
        )
        .unwrap();
    assert_eq!(reviewed.state, DecisionRequestState::Answered);
    let reopened = fx.request(&request.id);
    assert_eq!(reopened.state, DecisionRequestState::Open);
    assert!(reopened.resolution.is_none());
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Backlog
    );
    assert!(
        fx.runtime
            .get_task_comments(&task)
            .unwrap()
            .iter()
            .any(|comment| comment.by == OPERATOR && comment.message.contains("vetoed")),
        "the veto is commented"
    );
    assert!(corpus_appends(&fx, &task).is_empty());
    assert!(fx.digest()["model_answered"].as_array().unwrap().is_empty());

    // The human then answers the re-opened request as usual, and that human
    // answer is the one the corpus records.
    let answered = fx
        .operator()
        .answer_decision_request_as_human(
            &request.id,
            "2",
            None,
            OPERATOR.into(),
            Some("cli".into()),
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.unwrap().answered_by, OPERATOR);
    let appends = corpus_appends(&fx, &task);
    assert_eq!(appends.len(), 1, "{appends:?}");
    assert_eq!(appends[0]["chosen"], "2");
}

fn reversing_a_model_answer_reopens_it_and_retires_its_review() {
    let fx = Fixture::new();
    let (task, request) = fx.raised(["routine", "reversible"]);
    let applied = fx.apply(&request, "2");
    let reopened = fx
        .operator()
        .reverse_decision_answer(&request.id, OPERATOR)
        .unwrap();
    assert_eq!(reopened.state, DecisionRequestState::Open);
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Backlog
    );
    assert_eq!(
        fx.request(&applied.review.id).state,
        DecisionRequestState::Superseded
    );
    let late = fx
        .operator()
        .answer_decision_request_as_human(
            &applied.review.id,
            "confirm",
            None,
            OPERATOR.into(),
            None,
        )
        .expect_err("a retired review cannot confirm");
    assert_eq!(
        refusal_code(&late),
        Some(DecisionRefusalCode::RequestSuperseded)
    );
}

crate::scenario_table!(
    mutable_scenarios,
    model_answer_records_citation_raises_one_review_and_is_never_a_human_answer,
    model_answer_is_refused_for_non_self_answerable_tiers,
    confirm_marks_the_model_answer_confirmed_and_appends_the_confirming_human,
    veto_reopens_the_original_request_and_restores_the_previous_status,
    reversing_a_model_answer_reopens_it_and_retires_its_review,
);
