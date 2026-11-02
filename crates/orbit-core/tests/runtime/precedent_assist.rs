//! Precedent-decision category enablement, self-answer and demotion
//! (design `docs/design/decision-assistance/specs/precedent-decisions.md`,
//! Measured gate and enablement, Self-answer path, Drift): only routine and
//! reversible requests in an operator-enabled category are answered by the
//! model; a readout measured under another grant is refused; disable stops
//! answers at once; the rolling window demotes to shadow with its evidence.

use std::path::PathBuf;
use std::sync::Arc;

use orbit_config::assistance::{AssistancePolicy, AssistanceSettings, PrecedentProviderPolicy};
use orbit_core::application::assistance::{AssistanceAdapter, AssistanceService};
use orbit_core::application::precedent_decision::{
    self, PRECEDENT_DECISION_USE_CASE, PrecedentAssistState,
};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_store::contracts::assistance::AssistanceClaim;
use orbit_types::assistance::{
    self, AssistanceAnswer, AssistanceDisposition, AssistanceMode, AssistanceProbability,
    AssistanceQuestion, AssistanceReason, AssistanceRequest, AssistanceResult, AssistanceValue,
};
use orbit_types::task::{DecisionRequest, DecisionRequestState, TaskArtifact};
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [decision_channel]\nenabled = true\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";
const OPERATOR: &str = "human:operator";
const TOPIC: &str = "pricing/annual";

/// Answers every question with full confidence: the first offered option,
/// every precedent supporting it, and no conflict.
struct CertainAdapter;

impl AssistanceAdapter for CertainAdapter {
    fn assistance_adapter_identity(&self) -> &str {
        "certain-precedent-decision/1"
    }

    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        _: &[String],
        _: &AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason> {
        Ok(request)
    }

    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        _: &AssistancePolicy,
    ) -> AssistanceResult {
        let mut answers = std::collections::BTreeMap::new();
        for (name, question) in &claim.work.request.contract.questions {
            let answer = match question {
                AssistanceQuestion::Choice {
                    allowed,
                    abstain_values,
                    ..
                } => {
                    let chosen = allowed
                        .iter()
                        .find(|id| !abstain_values.contains(id))
                        .unwrap()
                        .clone();
                    AssistanceAnswer {
                        value: AssistanceValue::Choice(chosen.clone()),
                        probability: Some(1.0),
                        probabilities: allowed
                            .iter()
                            .map(|id| AssistanceProbability {
                                value: id.clone(),
                                probability: if *id == chosen { 1.0 } else { 0.0 },
                            })
                            .collect(),
                        criterion_refs: Vec::new(),
                    }
                }
                AssistanceQuestion::Predicate { .. } => {
                    let supports = name.starts_with("precedent_supports");
                    AssistanceAnswer {
                        value: AssistanceValue::Predicate(supports),
                        probability: Some(if supports { 1.0 } else { 0.0 }),
                        probabilities: Vec::new(),
                        criterion_refs: Vec::new(),
                    }
                }
            };
            answers.insert(name.clone(), answer);
        }
        AssistanceResult {
            disposition: AssistanceDisposition::Suggested,
            reason: AssistanceReason::Complete,
            answers,
            returned_model: Some("gpt-6-luna".into()),
            usage: None,
            latency_ms: None,
            estimated_cost_microusd: None,
            pricing_version: None,
        }
    }
}

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    workspace: PathBuf,
    runtime: OrbitRuntime,
    service: AssistanceService,
    workspace_id: String,
    filers: std::cell::Cell<usize>,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), CONFIG).unwrap();
        let workspace_id = OrbitRuntime::from_roots(&global, &workspace)
            .unwrap()
            .workspace_id()
            .unwrap();
        let fixture = Self {
            runtime: OrbitRuntime::from_roots(&global, &workspace).unwrap(),
            service: service(
                &OrbitRuntime::from_roots(&global, &workspace).unwrap(),
                &global,
            ),
            _root: root,
            global,
            workspace,
            workspace_id,
            filers: std::cell::Cell::new(0),
        };
        fixture.save_policy("fixture/1");
        fixture
    }

    /// Write the host's shadow grant at `model_revision`.
    fn save_policy(&self, model_revision: &str) {
        let template = precedent_decision::precedent_decision_question_template();
        let mut settings = AssistanceSettings::default();
        settings.budget.epoch = "fixture-budget/1".into();
        settings.budget.token_ceiling = 10_000_000;
        settings.budget.spend_ceiling_microusd = 10_000_000;
        settings.budget.input_microusd_per_million_tokens = Some(1);
        settings.budget.pricing_version = Some("fixture-price/1".into());
        settings.limits.max_attempts = 1;
        settings.precedent_providers.push(PrecedentProviderPolicy {
            workspace_id: self.workspace_id.clone(),
            built_in: true,
            plugin_tools: Vec::new(),
        });
        settings.policies.push(AssistancePolicy {
            workspace_id: self.workspace_id.clone(),
            use_case: PRECEDENT_DECISION_USE_CASE.into(),
            mode: AssistanceMode::Shadow,
            model: "gpt-6-luna".into(),
            model_revision: model_revision.into(),
            policy_version: "fixture-policy/1".into(),
            contract_digest: assistance::assistance_digest(&serde_json::to_vec(&template).unwrap()),
            evidence_fields: vec![
                "option_ids".into(),
                "request".into(),
                "task_context".into(),
                "precedents".into(),
                "provenance".into(),
            ],
            evidence_ref_prefixes: vec!["task:".into(), "decision:".into()],
            credential_env: "ORBIT_ASSISTANCE_FIXTURE".into(),
        });
        std::fs::write(
            self.global.join("config.toml"),
            format!(
                "assistance = {}\n",
                toml::Value::try_from(&settings).unwrap()
            ),
        )
        .unwrap();
    }

    fn operator(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.workspace)
            .unwrap()
            .with_actor(ActorIdentity::human(OPERATOR))
    }

    fn category(&self) -> String {
        format!("{}/agent_request/pricing", self.workspace_id)
    }

    /// One open agent request, with options declaring `tiers` (`None` is
    /// an undeclared tier), on a fresh human-crew task.
    fn raise(&self, tiers: [Option<&str>; 2]) -> DecisionRequest {
        self.raise_on(tiers, TOPIC)
    }

    /// [`Self::raise`] under `topic`, in the same `pricing` category.
    fn raise_on(&self, tiers: [Option<&str>; 2], topic: &str) -> DecisionRequest {
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
            .unwrap();
        let option = |label: &str, recommended: bool, tier: Option<&str>| {
            let mut option = json!({"label": label, "consequence": format!("{label}."), "recommended": recommended});
            if let Some(tier) = tier {
                option["risk_tier"] = json!(tier);
            }
            option
        };
        // Rotate the filer so the per-filer open-request cap never binds.
        let filers = ["claude", "codex", "gemini", "grok", "muse"];
        let filer = filers[self.filers.get() % filers.len()];
        self.filers.set(self.filers.get() + 1);
        let raised = self
            .runtime
            .run_tool(
                "orbit.decision.raise",
                json!({
                    "task_id": task.id,
                    "question": "Which price ships this year?",
                    "topic": topic,
                    "model": filer,
                    "options": [option("Keep the price", true, tiers[0]), option("Raise it 10%", false, tiers[1])],
                }),
            )
            .unwrap();
        assert_eq!(raised["state"], "open", "{raised}");
        self.request(raised["id"].as_str().unwrap())
    }

    fn request(&self, id: &str) -> DecisionRequest {
        self.runtime.get_decision_request(id).unwrap().unwrap()
    }

    fn answer(&self, request: &DecisionRequest, answer: &str) {
        self.runtime
            .answer_decision_request_as_human(
                &request.id,
                answer,
                None,
                OPERATOR.into(),
                Some("cli".into()),
            )
            .unwrap();
    }

    /// A human-answered request in the category, so the built-in provider
    /// supplies one precedent.
    fn precedent(&self) {
        let request = self.raise([Some("routine"), Some("routine")]);
        self.tick();
        self.answer(&request, "1");
    }

    fn tick(&self) {
        self.runtime
            .run_precedent_decision_with_service(&self.service)
            .unwrap();
    }

    fn live_digest(&self) -> String {
        self.service
            .assistance_shadow_policy_identity(&self.workspace_id, PRECEDENT_DECISION_USE_CASE)
            .unwrap()
            .unwrap()
            .digest
    }

    /// Attach a readout for the category under `digest` and return its
    /// `<task>/<artifact>` reference.
    fn readout(&self, digest: &str) -> String {
        let task = self
            .runtime
            .add_task(TaskAddParams {
                title: "Precedent readout".into(),
                description: "Holds the readout".into(),
                acceptance_criteria: vec!["Done".into()],
                complexity: TaskComplexity::Low,
                crew: Some("dk".into()),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
        let mut categories = serde_json::Map::new();
        categories.insert(
            self.category(),
            json!({
                "threshold": 0.9,
                "scored": 80,
                "coverage": {"value": 0.6, "covered": 48, "scored": 80},
                "accuracy": {"value": 1.0, "count": 48, "n": 48, "interval_95": [0.96, 1.0], "method": "Wilson"},
                "ece": {"value": 0.02},
                "targets": {"threshold": 0.9, "min_samples": 30, "min_accuracy": 0.95, "min_coverage": 0.3, "max_ece": 0.05},
            }),
        );
        let document = json!({
            "format_version": "decision-assistance-eval/4",
            "readout_version": "precedent-decision-readout/1",
            "evidence_status": "recorded human decisions; score is not an adoption authorization",
            "policy_digest": digest,
            "categories": categories,
            "validation_errors": [],
        });
        self.runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact::from_text(
                        "precedent-readout.json",
                        document.to_string(),
                    )],
                    ..Default::default()
                },
                OPERATOR.into(),
            )
            .unwrap();
        format!("{}/precedent-readout.json", task.id)
    }

    /// Enable the category from a passing readout, with no holdback and a
    /// window of `window` outcomes.
    fn enable(&self, window: u32) {
        let readout = self.readout(&self.live_digest());
        let enabled = self
            .operator()
            .enable_precedent_assist_with_service(
                &self.category(),
                &readout,
                OPERATOR,
                &self.service,
            )
            .unwrap();
        assert_eq!(enabled.effective_state, "enabled");
        self.runtime
            .tune_precedent_assist_for_test(&self.category(), 0.0, window)
            .unwrap();
    }

    fn state(&self) -> (String, Value) {
        let status = self
            .runtime
            .precedent_assist_status_with_service(&self.service)
            .unwrap();
        let row = status
            .into_iter()
            .find(|row| row.enablement.category == self.category())
            .expect("category enablement");
        let value = serde_json::to_value(&row).unwrap();
        (row.effective_state, value)
    }

    fn route(&self, request: &DecisionRequest) -> Option<String> {
        self.runtime
            .precedent_decision_receipt(&request.id)
            .unwrap()
            .and_then(|receipt| receipt.assist)
            .map(|routing| routing.route)
    }
}

fn service(runtime: &OrbitRuntime, global: &std::path::Path) -> AssistanceService {
    let config = global.join("config.toml");
    AssistanceService::new(
        orbit_store::compose::assistance_store_from_store(runtime.sqlite_store().unwrap()),
        Arc::new(CertainAdapter),
        Arc::new(move || {
            Ok(
                orbit_config::ConfigStore::open(orbit_config::ConfigScope::Global, config.clone())?
                    .snapshot()?
                    .assistance_settings,
            )
        }),
    )
}

fn answered_by(request: &DecisionRequest) -> Option<String> {
    request
        .resolution
        .as_ref()
        .map(|resolution| resolution.answered_by.clone())
}

/// Criterion 1: an irreversible, authority, spend or unknown-tier request is
/// never self-answered, even when the model is certain and the category is
/// enabled; a routine/reversible one in the same category is.
#[test]
fn excluded_tiers_are_never_self_answered_even_at_probability_one() {
    let fixture = Fixture::new();
    fixture.precedent();
    fixture.enable(30);

    for tier in [Some("irreversible"), Some("authority"), Some("spend"), None] {
        let request = fixture.raise([Some("routine"), tier]);
        fixture.tick();
        let after = fixture.request(&request.id);
        assert_eq!(
            after.state,
            DecisionRequestState::Open,
            "a {tier:?} request must wait for the human: {after:?}"
        );
        assert_eq!(fixture.route(&request).as_deref(), Some("excluded_tier"));
        fixture.answer(&request, "1");
    }

    let routine = fixture.raise([Some("routine"), Some("reversible")]);
    fixture.tick();
    let answered = fixture.request(&routine.id);
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered_by(&answered).as_deref(), Some("model"));
    let model = answered.resolution.unwrap().model.unwrap();
    assert_eq!(model.enablement.operator, OPERATOR);
    assert!(!model.cited_precedents.is_empty(), "{model:?}");
    assert_eq!(fixture.route(&routine).as_deref(), Some("self_answered"));
}

/// Criterion 2: a readout measured under another policy digest is refused
/// and nothing is enabled; disabling stops self-answers immediately.
#[test]
fn enable_refuses_a_foreign_digest_and_disable_stops_answers() {
    let fixture = Fixture::new();
    fixture.precedent();

    let foreign = fixture.readout(&format!("sha256:{}", "0".repeat(64)));
    let refused = fixture
        .operator()
        .enable_precedent_assist_with_service(
            &fixture.category(),
            &foreign,
            OPERATOR,
            &fixture.service,
        )
        .unwrap_err();
    assert!(
        matches!(&refused, OrbitError::PolicyDenied(message) if message.contains("policy digest")),
        "{refused:?}"
    );
    assert!(
        fixture
            .runtime
            .precedent_assist_status_with_service(&fixture.service)
            .unwrap()
            .is_empty(),
        "a refused readout enables nothing"
    );
    let shadow_only = fixture.raise([Some("routine"), Some("routine")]);
    fixture.tick();
    assert_eq!(
        fixture.request(&shadow_only.id).state,
        DecisionRequestState::Open
    );
    fixture.answer(&shadow_only, "1");

    fixture.enable(30);
    fixture
        .operator()
        .disable_precedent_assist(&fixture.category(), OPERATOR)
        .unwrap();
    let after_disable = fixture.raise([Some("routine"), Some("routine")]);
    fixture.tick();
    assert_eq!(
        fixture.request(&after_disable.id).state,
        DecisionRequestState::Open,
        "a disabled category never answers"
    );
    assert_eq!(fixture.state().0, "disabled");
}

/// Criterion 3: vetoes reaching `max_vetoes` demote the category to shadow
/// with recorded evidence; nothing re-enables it but the operator.
#[test]
fn vetoes_demote_to_shadow_and_only_the_operator_reenables() {
    let fixture = Fixture::new();
    fixture.precedent();
    fixture.enable(30);

    // Each veto reverses its answer, a contradiction for its own topic, so
    // the two answers sit on two topics of the one category.
    for topic in ["pricing/annual-0", "pricing/annual-1"] {
        let request = fixture.raise_on([Some("routine"), Some("reversible")], topic);
        fixture.tick();
        let answered = fixture.request(&request.id);
        assert_eq!(answered_by(&answered).as_deref(), Some("model"));
        let review = fixture
            .runtime
            .list_decision_requests(None, Some(&request.task_id), false)
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id != request.id)
            .expect("model answer review");
        fixture.answer(&review, "veto");
        fixture.answer(&fixture.request(&request.id), "2");
    }
    let (state, value) = fixture.state();
    assert_eq!(state, "demoted", "{value}");
    assert_eq!(value["ended"]["reason"], "demoted", "{value}");
    assert_eq!(
        value["ended"]["demotion"]["trigger"], "max_vetoes",
        "{value}"
    );
    assert_eq!(value["ended"]["demotion"]["vetoes"], 2, "{value}");

    let demoted = fixture.raise([Some("routine"), Some("routine")]);
    fixture.tick();
    assert_eq!(
        fixture.request(&demoted.id).state,
        DecisionRequestState::Open,
        "a demoted category is shadow only"
    );
    fixture.answer(&demoted, "1");

    fixture.enable(30);
    assert_eq!(fixture.state().0, "enabled");
    let reenabled = fixture.raise_on([Some("routine"), Some("routine")], "pricing/annual-2");
    fixture.tick();
    assert_eq!(
        answered_by(&fixture.request(&reenabled.id)).as_deref(),
        Some("model")
    );
}

/// Two recent human answers on the request's topic that chose different
/// options are a deterministic contradiction: the request goes to the
/// human whatever the model's own conflict flag says.
#[test]
fn contradicting_human_precedents_send_the_request_to_the_human() {
    let fixture = Fixture::new();
    fixture.precedent();
    let other = fixture.raise([Some("routine"), Some("routine")]);
    fixture.tick();
    fixture.answer(&other, "2");
    fixture.enable(30);

    let request = fixture.raise([Some("routine"), Some("routine")]);
    fixture.tick();
    assert_eq!(
        fixture.request(&request.id).state,
        DecisionRequestState::Open
    );
    assert_eq!(
        fixture.route(&request).as_deref(),
        Some("declined:contradiction")
    );
}

/// Criterion 3: holdback predictions that disagree with the human drop the
/// window's Wilson lower bound below `min_accuracy`, which demotes the
/// category with the window as evidence.
#[test]
fn a_failing_holdback_window_demotes_with_evidence() {
    let fixture = Fixture::new();
    fixture.precedent();
    fixture.enable(2);
    fixture
        .runtime
        .tune_precedent_assist_for_test(&fixture.category(), 1.0, 2)
        .unwrap();

    for _ in 0..2 {
        let request = fixture.raise([Some("routine"), Some("routine")]);
        fixture.tick();
        assert_eq!(fixture.route(&request).as_deref(), Some("holdback"));
        assert_eq!(
            fixture.request(&request.id).state,
            DecisionRequestState::Open
        );
        fixture.answer(&request, "2");
        fixture.tick();
    }
    let (state, value) = fixture.state();
    assert_eq!(state, "demoted", "{value}");
    let demotion = &value["ended"]["demotion"];
    assert_eq!(demotion["trigger"], "accuracy", "{value}");
    assert_eq!(demotion["outcomes"], 2, "{value}");
    assert_eq!(demotion["agreed"], 0, "{value}");
    assert_eq!(demotion["request_ids"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        serde_json::to_value(PrecedentAssistState::Demoted).unwrap(),
        "demoted"
    );
}

/// A model-revision change on the live grant suspends the enablement.
#[test]
fn a_model_revision_change_suspends_the_enablement() {
    let fixture = Fixture::new();
    fixture.precedent();
    fixture.enable(30);
    fixture.save_policy("fixture/2");
    let request = fixture.raise([Some("routine"), Some("routine")]);
    fixture.tick();
    assert_eq!(
        fixture.request(&request.id).state,
        DecisionRequestState::Open
    );
    let (state, value) = fixture.state();
    assert_eq!(state, "suspended", "{value}");
    assert_eq!(
        value["ended"]["reason"], "model_revision_changed",
        "{value}"
    );
}
