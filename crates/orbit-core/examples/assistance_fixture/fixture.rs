//! Manual public-boundary fixtures; all responses and labels are synthetic.
#[path = "../../src/adapter/decisions/send_error.rs"]
mod send_error;

use chrono::{DateTime, Duration, Utc};
use orbit_config::assistance::{AssistancePolicy, AssistanceSettings};
use orbit_core::adapter::decisions::{
    AssistanceCredentialSource, AssistanceHttpResponse, DecisionsTransport, OpenAiDecisionsAdapter,
};
use orbit_core::application::assistance::AssistanceService;
use orbit_store::Store;
use orbit_store::contracts::assistance::{
    AssistanceAdmission, AssistanceRecord, AssistanceStoreBackend,
};
use orbit_types::assistance::{
    self, AssistanceContract, AssistanceEvidencePacket, AssistanceLimits, AssistanceMode,
    AssistanceQuestion, AssistanceReason, AssistanceRequest,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
pub(super) type FixtureResult<T = ()> = Result<T, Box<dyn Error>>;
pub(super) const CREDENTIAL: &str = "fixture-project-credential-only";
const SUPPLIED: &str = "fixture-secret\"with\nencoding";

pub(super) fn ambiguous_non_timeout_send_failure_reason() -> AssistanceReason {
    send_error::decisions_send_failure_reason(send_error::DecisionsSendFailureClass::Other)
}

pub(super) fn builder_send_failure_reason() -> AssistanceReason {
    send_error::decisions_send_failure_reason(send_error::DecisionsSendFailureClass::Builder)
}

pub(super) fn connect_send_failure_reason() -> AssistanceReason {
    send_error::decisions_send_failure_reason(send_error::DecisionsSendFailureClass::Connect)
}

pub(super) fn timeout_send_failure_reason() -> AssistanceReason {
    send_error::decisions_send_failure_reason(send_error::DecisionsSendFailureClass::Timeout)
}

pub(super) struct FixtureCredential(pub(super) AtomicUsize, pub(super) AtomicBool);
impl AssistanceCredentialSource for FixtureCredential {
    fn assistance_credential(&self, _: &str) -> Option<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1.load(Ordering::SeqCst).then(|| CREDENTIAL.into())
    }
}

pub(super) struct FixtureTransport {
    pub(super) store: Arc<Store>,
    pub(super) replies: Mutex<VecDeque<Result<AssistanceHttpResponse, AssistanceReason>>>,
    pub(super) bodies: Mutex<Vec<Value>>,
    pub(super) on_send: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl DecisionsTransport for FixtureTransport {
    fn send_assistance(
        &self,
        body: &Value,
        credential: &str,
        _: &str,
        _: DateTime<Utc>,
        _: &AssistanceLimits,
    ) -> Result<AssistanceHttpResponse, AssistanceReason> {
        assert_eq!(credential, CREDENTIAL);
        let encoded = body.to_string();
        assert!(!encoded.contains(CREDENTIAL));
        assert!(!encoded.contains("fixture-secret"));
        // Deterministically prove the application released its store lock.
        let connection = self.store.connection();
        let conn = connection
            .try_lock()
            .map_err(|_| AssistanceReason::Unavailable)?;
        conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
            .map_err(|_| AssistanceReason::Unavailable)?;
        drop(conn);
        self.bodies
            .lock()
            .map_err(|_| AssistanceReason::Unavailable)?
            .push(body.clone());
        if let Some(action) = self
            .on_send
            .lock()
            .map_err(|_| AssistanceReason::Unavailable)?
            .take()
        {
            action();
        }
        self.replies
            .lock()
            .map_err(|_| AssistanceReason::Unavailable)?
            .pop_front()
            .ok_or(AssistanceReason::Unavailable)?
    }
}

pub(super) struct AssistanceFixture {
    pub(super) store: Arc<Store>,
    pub(super) settings: Arc<Mutex<AssistanceSettings>>,
    pub(super) clock: Arc<Mutex<DateTime<Utc>>>,
    pub(super) credential: Arc<FixtureCredential>,
    pub(super) transport: Arc<FixtureTransport>,
    pub(super) service: AssistanceService,
    pub(super) contract: AssistanceContract,
}

impl AssistanceFixture {
    pub(super) fn open_fixture(root: &Path, schema: &Value, consumer: &str) -> FixtureResult<Self> {
        std::fs::create_dir_all(root)?;
        let store = Arc::new(Store::open(&root.join("orbit.db"))?);
        let mut contract = AssistanceContract {
            format_version: schema["format_version"]
                .as_str()
                .ok_or("schema format missing")?
                .into(),
            question_set_version: schema["question_sets"][consumer]["version"]
                .as_str()
                .ok_or("question version missing")?
                .into(),
            rubric_version: "evaluation-rubric/1".into(),
            evidence_builder_version: "synthetic-packet/1".into(),
            result_schema_version: assistance::ASSISTANCE_RESULT_SCHEMA.into(),
            questions: serde_json::from_value(
                schema["question_sets"][consumer]["questions"].clone(),
            )?,
        };
        for q in contract.questions.values_mut() {
            match q {
                AssistanceQuestion::Predicate { min_confidence, .. } => *min_confidence = Some(0.9),
                AssistanceQuestion::Choice {
                    min_confidence,
                    abstain_values,
                    ..
                } => {
                    *min_confidence = Some(0.9);
                    abstain_values.push("insufficient_evidence".into());
                }
            }
        }
        let mut settings = AssistanceSettings::default();
        settings.budget.epoch = "synthetic-only/1".into();
        settings.budget.token_ceiling = 1_000_000;
        settings.budget.spend_ceiling_microusd = 1_000_000;
        settings.budget.input_microusd_per_million_tokens = Some(1_000_000);
        settings.budget.pricing_version = Some("synthetic-price/1".into());
        settings.policies.push(AssistancePolicy {
            workspace_id: "ws_fixture".into(),
            use_case: consumer.into(),
            mode: AssistanceMode::Advisory,
            model: "gpt-6-luna".into(),
            model_revision: "fixture-model/1".into(),
            policy_version: "synthetic-policy/1".into(),
            contract_digest: assistance::assistance_digest(&serde_json::to_vec(&contract)?),
            evidence_fields: vec![
                "excerpts".into(),
                "typed_errors".into(),
                "source_revision".into(),
                "criteria".into(),
            ],
            evidence_ref_prefixes: vec!["fixture:".into()],
            credential_env: "ORBIT_ASSISTANCE_FIXTURE_KEY".into(),
        });
        let settings = Arc::new(Mutex::new(settings));
        let clock = Arc::new(Mutex::new(
            DateTime::parse_from_rfc3339("2026-10-28T12:00:00Z")?.with_timezone(&Utc),
        ));
        let credential = Arc::new(FixtureCredential(
            AtomicUsize::new(0),
            AtomicBool::new(true),
        ));
        let transport = Arc::new(FixtureTransport {
            store: store.clone(),
            replies: Mutex::new(VecDeque::new()),
            bodies: Mutex::new(vec![]),
            on_send: Mutex::new(None),
        });
        let loader = settings.clone();
        let time = clock.clone();
        let service = AssistanceService::new(
            store.clone(),
            Arc::new(OpenAiDecisionsAdapter::with_assistance_transport(
                credential.clone(),
                transport.clone(),
            )),
            Arc::new(move || {
                loader.lock().map(|s| s.clone()).map_err(|_| {
                    orbit_common::OrbitError::InvalidInput("fixture settings lock".into())
                })
            }),
        )
        .with_assistance_clock(Arc::new(move || {
            *time.lock().unwrap_or_else(|e| e.into_inner())
        }));
        Ok(Self {
            store,
            settings,
            clock,
            credential,
            transport,
            service,
            contract,
        })
    }

    pub(super) fn request(&self, subject: &str) -> FixtureResult<AssistanceRequest> {
        Ok(AssistanceRequest {
            workspace_id: "ws_fixture".into(),
            owner_id: "fixture-host".into(),
            use_case: self.settings.lock().map_err(|_| "settings lock")?.policies[0]
                .use_case
                .clone(),
            source_digest: assistance::assistance_digest(subject.as_bytes()),
            contract: self.contract.clone(),
            evidence_packet: AssistanceEvidencePacket {
                packet_id: subject.into(),
                subject_id: subject.into(),
                collected_at: *self.clock.lock().map_err(|_| "clock lock")?,
                evidence_refs: vec!["fixture:permitted".into()],
                omissions: vec![],
                truncated: false,
                fields: BTreeMap::from([
                    (
                        "excerpts".into(),
                        json!(format!("synthetic evidence {CREDENTIAL} {SUPPLIED}")),
                    ),
                    ("source_revision".into(), json!("synthetic/1")),
                    ("typed_errors".into(), json!([])),
                    ("criteria".into(), json!([])),
                ]),
            },
        })
    }

    pub(super) fn enqueue(&self, request: AssistanceRequest) -> FixtureResult<String> {
        match self
            .service
            .enqueue_assistance(request, &[SUPPLIED.into()])?
        {
            AssistanceAdmission::Enqueued(id) | AssistanceAdmission::Existing(id) => Ok(id),
            _ => Err("fixture was not admitted".into()),
        }
    }

    pub(super) fn good_response(&self) -> Value {
        let answers:Vec<Value>=self.contract.questions.iter().map(|(name,q)| match q {
            AssistanceQuestion::Predicate { .. }=>json!({"type":"predicate","name":name,"probability":0.98}),
            AssistanceQuestion::Choice { allowed,.. }=>json!({"type":"choice","name":name,"choice":allowed[0],"confidence":0.98,
                "probabilities":allowed.iter().enumerate().map(|(i,v)| json!({"value":v,"probability":if i==0 {1.0} else {0.0}})).collect::<Vec<_>>()}),
        }).collect();
        json!({"model":"gpt-6-luna","answers":answers,"usage":{"input_tokens":100,"output_tokens":0}})
    }

    pub(super) fn respond(&self, body: Value) -> FixtureResult {
        self.transport
            .replies
            .lock()
            .map_err(|_| "reply lock")?
            .push_back(Ok(AssistanceHttpResponse {
                status: 200,
                body: serde_json::to_vec(&body)?,
            }));
        Ok(())
    }

    pub(super) fn fail_send(&self, reason: AssistanceReason) -> FixtureResult {
        self.transport
            .replies
            .lock()
            .map_err(|_| "reply lock")?
            .push_back(Err(reason));
        Ok(())
    }

    pub(super) fn process(&self) -> FixtureResult {
        let consumer = self.settings.lock().map_err(|_| "settings lock")?.policies[0]
            .use_case
            .clone();
        self.service
            .process_next_assistance("ws_fixture", &consumer)?;
        Ok(())
    }

    pub(super) fn record(&self, id: &str) -> FixtureResult<AssistanceRecord> {
        self.store
            .assistance_record(id)?
            .ok_or_else(|| "missing record".into())
    }
    pub(super) fn advance(&self, seconds: i64) -> FixtureResult {
        *self.clock.lock().map_err(|_| "clock lock")? += Duration::seconds(seconds);
        Ok(())
    }
    pub(super) fn now(&self) -> FixtureResult<DateTime<Utc>> {
        Ok(*self.clock.lock().map_err(|_| "clock lock")?)
    }
}

impl AssistanceFixture {
    pub(super) fn request_for_off(&self) -> FixtureResult<AssistanceRequest> {
        Ok(AssistanceRequest {
            workspace_id: "ws_fixture".into(),
            owner_id: "fixture-host".into(),
            use_case: "task_quality".into(),
            source_digest: assistance::assistance_digest(b"off"),
            contract: self.contract.clone(),
            evidence_packet: AssistanceEvidencePacket {
                packet_id: "off".into(),
                subject_id: "off".into(),
                collected_at: self.now()?,
                evidence_refs: vec![],
                omissions: vec![],
                truncated: false,
                fields: BTreeMap::new(),
            },
        })
    }
}
