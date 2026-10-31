//! The delegated-human decision channel (design
//! `docs/design/human-decision-channel/2_design.md` §3–§5): a registered
//! test channel submits a human's answer through core with a per-request
//! token, and every refusal in the §5 table applies nothing.

use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use orbit_core::application::task::{
    DecisionDigest, DecisionSubmitCaller, DecisionSubmitParams, DkimTxtResolver,
    FinalRecoveryCompletion, FinalRecoveryRequest, TaskUpdateParams,
};
use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DECISION_REQUESTED_EVENT, DecisionAnswer, DecisionBinding,
    DecisionGate, DecisionRequest, DecisionRequestState, TaskComment, TaskDecision,
    TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{CANDIDATE_HELD_EVENT, HeldCandidate};
use rsa::RsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

const CHANNEL: &str = "email";
const PLUGIN: &str = "decision-mail";
const HUMAN: &str = "human:operator";
const SENDER: &str = "operator@example.com";
const AUTH_PASS: &str = "mx.example.com; dkim=pass header.d=example.com; spf=pass smtp.mailfrom=example.com; dmarc=pass";
const AUTH_DMARC_FAIL: &str = "mx.example.com; dkim=pass; spf=pass; dmarc=fail";
const FORGED: &str = "forged@example.net";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    runs: std::cell::Cell<u32>,
}

/// A blocked task, its open request and the token minted for that request.
struct Blocked {
    task: String,
    request: DecisionRequest,
    token: String,
}

fn fixture() -> Fixture {
    fixture_with_config("[decision_channel]\nenabled = true\n")
}

/// A fixture whose channel is registered for the bound plugin and sender.
fn registered_fixture() -> Fixture {
    let fx = fixture();
    fx.register();
    fx
}

fn fixture_with_config(config: &str) -> Fixture {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    if !config.is_empty() {
        std::fs::write(repo.join(".orbit/config.toml"), config).unwrap();
    }
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    Fixture {
        _root: root,
        global,
        repo,
        runtime,
        runs: std::cell::Cell::new(0),
    }
}

/// The submission the bound plugin makes for `request`: the operator's
/// sender and a passing `Authentication-Results`.
fn plugin_submission(request: &DecisionRequest, answer: &str, token: &str) -> DecisionSubmitParams {
    DecisionSubmitParams {
        channel: CHANNEL.to_string(),
        request_id: request.id.clone(),
        answer_id: answer.to_string(),
        note: None,
        token: token.to_string(),
        sender: Some(SENDER.to_string()),
        auth_results: Some(AUTH_PASS.to_string()),
        raw_message: None,
        caller: Fixture::plugin_caller(),
    }
}

/// The refusal `result` must carry: its message names `code`.
fn refusal<T: Debug>(result: Result<T, OrbitError>, code: &str) -> String {
    let error = result.expect_err("the call is refused").to_string();
    assert!(error.contains(code), "want {code}: {error}");
    error
}

/// The digest's request for `task`.
fn request_for(requests: &[DecisionRequest], task: &str) -> DecisionRequest {
    requests
        .iter()
        .find(|request| request.task_id == task)
        .unwrap_or_else(|| panic!("the digest raised a request for {task}"))
        .clone()
}

/// The candidate and status a task-revision request is bound to.
fn revision(request: &DecisionRequest) -> (Option<&str>, &TaskStatus) {
    match &request.binding {
        DecisionBinding::TaskRevision {
            candidate, status, ..
        } => (candidate.as_deref(), status),
        other => panic!(
            "a {:?} request binds to a task revision: {other:?}",
            request.gate
        ),
    }
}

/// A plugin the channel is not bound to.
fn rogue_plugin() -> DecisionSubmitCaller {
    DecisionSubmitCaller::PluginCallback {
        plugin: "other-plugin".to_string(),
    }
}

fn decision_answer(id: &str, label: &str, consequence: &str) -> DecisionAnswer {
    DecisionAnswer {
        id: id.into(),
        label: label.into(),
        consequence: consequence.into(),
    }
}

impl Fixture {
    fn human(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_actor(ActorIdentity::human(HUMAN))
    }

    fn add_task(&self, title: &str, description: &str, criterion: &str) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": title,
                    "description": description,
                    "acceptance_criteria": [criterion],
                    "complexity": "low",
                    "type": "chore",
                    "workspace": self.repo.to_string_lossy(),
                    "model": "codex"
                }),
            )
            .unwrap();
        task["id"].as_str().unwrap().to_string()
    }

    fn task(&self) -> String {
        let id = self.add_task(
            "Decide an escalated block",
            "Decision channel fixture task.",
            "Recovered.",
        );
        self.update(json!({"id": id, "plan": "1. Do it."}));
        self.update(json!({"id": id, "status": "backlog"}));
        id
    }

    /// A task left in `proposed`, as `orbit.task.add` files it.
    fn proposed_task(&self, title: &str) -> String {
        let task = self.add_task(
            title,
            "A proposed task awaiting approval.",
            "It is decided.",
        );
        assert_eq!(self.status(&task), TaskStatus::Proposed);
        task
    }

    fn update(&self, mut input: Value) {
        input["model"] = json!("codex");
        self.runtime.run_tool("orbit.task.update", input).unwrap();
    }

    fn escalate(&self, task: &str) {
        self.update(json!({"id": task, "status": "in_progress"}));
        let observed = self
            .runtime
            .final_recovery_revision(&self.runtime.get_task(task).unwrap())
            .unwrap();
        self.runs.set(self.runs.get() + 1);
        self.runtime
            .apply_final_recovery(
                &FinalRecoveryRequest {
                    task_id: task.to_string(),
                    run_id: format!("jrun-channel-{}", self.runs.get()),
                    observed,
                    repo_root: self.repo.clone(),
                    base_ref: "main".to_string(),
                    completion: FinalRecoveryCompletion::Review,
                    requeue_bound: Default::default(),
                },
                Some(&json!({
                    "decision": "escalate",
                    "diagnosis": "the worker crashed while the host was rebooting",
                    "human_action": "decide how to proceed",
                })),
            )
            .unwrap();
        assert_eq!(self.status(task), TaskStatus::Blocked);
    }

    fn status(&self, task: &str) -> TaskStatus {
        self.runtime.get_task(task).unwrap().status
    }

    fn comments(&self, task: &str) -> usize {
        self.runtime.get_task_comments(task).unwrap().len()
    }

    fn mint(&self, request: &DecisionRequest) -> String {
        self.runtime
            .mint_decision_token(CHANNEL, &request.id)
            .unwrap()
    }

    /// A blocked task with one open block-triage request and its token.
    fn blocked(&self) -> Blocked {
        self.blocked_expiring(None)
    }

    /// A blocked task whose open block-triage request expires at `expires_at`
    /// when given.
    fn blocked_expiring(&self, expires_at: Option<chrono::DateTime<chrono::Utc>>) -> Blocked {
        let task = self.task();
        self.escalate(&task);
        let blocked_at = self
            .runtime
            .get_task_history(&task)
            .unwrap()
            .iter()
            .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .expect("seeded blocked task records its block")
            .at;
        let mut request = DecisionRequest::new(
            task.clone(),
            DecisionGate::BlockTriage,
            vec![
                decision_answer("1", "Drop criterion 2", "the old API stays retired"),
                decision_answer(
                    "dismiss",
                    "Dismiss",
                    "Leave blocked, stop asking for this episode.",
                ),
            ],
            DecisionBinding::BlockEpisode {
                episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            },
            "block_triage".to_string(),
            chrono::Utc::now(),
        );
        request.expires_at = expires_at.unwrap_or(request.expires_at);
        self.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    append_history: vec![request.history_entry("system")],
                    ..Default::default()
                },
            )
            .unwrap();
        // The proposal comment that raised the request carries the option's
        // target and replacement, which an answer applies.
        self.runtime
            .force_update_task_with_identity(
                &task,
                TaskUpdateParams {
                    comment: Some(
                        "blocked_task_triage disposition=decision outcome=proposed\n\nOption 1: Retire the old API criterion\nConsequences: the criterion names the retired API\nApplies to: acceptance_criteria\nReplacement text:\n- The old API stays retired.\n\nRecommended: option 1 (Retire the old API criterion)"
                            .to_string(),
                    ),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        let stored = self
            .requests(&task)
            .into_iter()
            .find(|candidate| candidate.id == request.id)
            .unwrap();
        let token = self.mint(&stored);
        Blocked {
            task,
            request: stored,
            token,
        }
    }

    fn register(&self) {
        self.register_with_senders(vec![SENDER.to_string()]);
    }

    fn register_with_senders(&self, allowed_senders: Vec<String>) {
        self.runtime
            .register_decision_channel(CHANNEL, HUMAN, PLUGIN, allowed_senders)
            .unwrap();
    }

    fn register_dkim(&self, required: bool) {
        self.runtime
            .register_decision_channel_with_dkim_requirement(
                CHANNEL,
                HUMAN,
                PLUGIN,
                vec![SENDER.to_string()],
                required,
            )
            .unwrap();
    }

    fn plugin_caller() -> DecisionSubmitCaller {
        DecisionSubmitCaller::PluginCallback {
            plugin: PLUGIN.to_string(),
        }
    }

    fn submit(
        &self,
        request: &DecisionRequest,
        answer: &str,
        token: &str,
    ) -> Result<DecisionRequest, OrbitError> {
        self.runtime
            .submit_decision_via_channel(plugin_submission(request, answer, token))
    }

    /// Submits answer `1` for `b` with the token minted for it.
    fn submit_blocked(&self, b: &Blocked, answer: &str) -> Result<DecisionRequest, OrbitError> {
        self.submit(&b.request, answer, &b.token)
    }

    /// Submits `answer` for `request` with a token minted for it now.
    fn submit_minted(
        &self,
        request: &DecisionRequest,
        answer: &str,
    ) -> Result<DecisionRequest, OrbitError> {
        self.submit(request, answer, &self.mint(request))
    }

    fn submit_dkim(
        &self,
        b: &Blocked,
        raw_message: Option<String>,
        resolver: &dyn DkimTxtResolver,
    ) -> Result<DecisionRequest, OrbitError> {
        let mut params = plugin_submission(&b.request, "1", &b.token);
        params.raw_message = raw_message;
        self.runtime
            .submit_decision_via_channel_with_resolver(params, resolver)
    }

    /// `submit` is refused with `code`, and the blocked task gains no comment.
    fn assert_refused_untouched<T: Debug>(
        &self,
        b: &Blocked,
        code: &str,
        submit: impl FnOnce() -> Result<T, OrbitError>,
    ) {
        let comments = self.comments(&b.task);
        refusal(submit(), code);
        assert_eq!(self.status(&b.task), TaskStatus::Blocked, "{code} applied");
        assert_eq!(self.comments(&b.task), comments, "{code} wrote a comment");
    }

    /// A signed raw message is refused as a DKIM failure, leaving the task
    /// blocked.
    fn assert_dkim_refused(&self, b: &Blocked, raw: String, label: &str) {
        let refused = self
            .submit_dkim(b, Some(raw), &FixtureResolver)
            .expect_err(label)
            .to_string();
        assert!(
            refused.contains("dkim_verification_failed"),
            "{label}: {refused}"
        );
        assert_eq!(self.status(&b.task), TaskStatus::Blocked, "{label}");
    }

    /// A valid signed raw message applies the answer as the human.
    fn assert_dkim_applies(&self, b: &Blocked, raw: String, label: &str) {
        let answered = self
            .submit_dkim(b, Some(raw), &FixtureResolver)
            .unwrap_or_else(|error| panic!("{label}: valid message refused: {error}"));
        assert_eq!(answered.state, DecisionRequestState::Answered, "{label}");
        assert_eq!(self.status(&b.task), TaskStatus::Backlog, "{label}");
    }

    /// Declares an existing workspace file as the task's footprint.
    fn prepare_footprint(&self, task: &str) {
        std::fs::write(self.repo.join("README.md"), "# fixture\n").unwrap();
        self.update(json!({"id": task, "context_files": ["file:README.md"]}));
    }

    /// Records an applied task-pilot assessment holding the task as a
    /// duplicate, in the receipt-comment form pilot admission reads.
    fn seed_pilot_duplicate_hold(&self, task: &str) {
        let audit = json!({"assessment": {
            "disposition": "selectors",
            "duplicate_of": {
                "task_id": task,
                "evidence": "The same work is already covered by existing work.",
            },
            "already_landed": null,
        }});
        self.runtime
            .apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    append_comments: vec![TaskComment {
                        at: chrono::Utc::now(),
                        by: "task-pilot".to_string(),
                        message: format!("operation_id={}\n{audit}", "a".repeat(64)),
                        operation_id: None,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
    }

    fn digest(&self, caller: DecisionSubmitCaller) -> Result<DecisionDigest, OrbitError> {
        self.runtime.decision_digest(CHANNEL, caller)
    }

    fn requests(&self, task: &str) -> Vec<DecisionRequest> {
        self.runtime.get_task_decision_requests(task).unwrap()
    }

    fn state_of(&self, task: &str, request: &str) -> DecisionRequestState {
        self.requests(task)
            .into_iter()
            .find(|candidate| candidate.id == request)
            .unwrap()
            .state
    }

    fn assert_state(&self, task: &str, request: &str, state: DecisionRequestState) {
        assert_eq!(self.state_of(task, request), state, "request {request}");
    }

    /// The open proposal approval on `task`.
    fn open_proposal(&self, task: &str) -> DecisionRequest {
        self.requests(task)
            .into_iter()
            .find(|request| {
                request.gate == DecisionGate::ProposedApproval
                    && request.state == DecisionRequestState::Open
            })
            .unwrap()
    }

    fn requested_entries(&self, task: &str) -> usize {
        self.runtime
            .get_task_history(task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == DECISION_REQUESTED_EVENT)
            .count()
    }

    /// Records a candidate with `head` held against `task`, as an owner-local
    /// run does when its candidate waits for review.
    fn hold_candidate(&self, task: &str, head: &str) {
        let held = HeldCandidate {
            run_id: format!("jrun-{head}"),
            machine_id: None,
            branch: "orbit/candidate".to_string(),
            head_sha: head.to_string(),
            failed_step_id: "review".to_string(),
            durable_ref: None,
            carry_failure: None,
            task_spec_digest: self.runtime.get_task(task).unwrap().spec_digest(),
        };
        let entry = TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: "system".to_string(),
            event: CANDIDATE_HELD_EVENT.to_string(),
            note: Some(held.text()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: Some(TaskDecision::CandidateHeld(held)),
        };
        self.runtime
            .apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    append_history: vec![entry],
                    ..Default::default()
                },
            )
            .unwrap();
    }

    fn plugin_secrets(&self) -> Value {
        let path = self
            .global
            .join("state/plugin-secrets")
            .join(format!("{PLUGIN}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }
}

/// Parks the first task-boundary lock report until the test releases it, so
/// the channel store lock can be probed while a slow task write is in flight.
struct ParkTaskBoundaryReport {
    parked: Sender<()>,
    release: Mutex<Receiver<()>>,
    captured: AtomicBool,
}

#[derive(Default)]
struct BoundaryFields {
    label: String,
    message: String,
}

impl tracing::field::Visit for BoundaryFields {
    // `record_str` defaults to this path, so strings arrive quoted; the
    // checks below are substring matches, which quoting leaves unchanged.
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn Debug) {
        match field.name() {
            "label" => self.label = format!("{value:?}"),
            "message" => self.message = format!("{value:?}"),
            _ => {}
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for ParkTaskBoundaryReport {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() != "orbit.common.fs.file_lock" {
            return;
        }
        let mut fields = BoundaryFields::default();
        event.record(&mut fields);
        let boundary = fields.label.contains("task commit boundary")
            && fields
                .message
                .contains("advisory file lock held past its threshold");
        if boundary && !self.captured.swap(true, Ordering::SeqCst) {
            let _ = self.parked.send(());
            let _ = self.release.lock().expect("report release lock").recv();
        }
    }
}

#[test]
fn a_slow_decision_task_report_does_not_hold_the_channel_store_lock() {
    let fx = registered_fixture();
    let b = fx.blocked();
    let lock_path = fx.global.join("state/decision-channels/channels.lock");
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let runtime = fx.runtime.clone();
    let params = plugin_submission(&b.request, "1", &b.token);
    let submit = std::thread::spawn(move || {
        let subscriber = tracing_subscriber::registry().with(ParkTaskBoundaryReport {
            parked: parked_tx,
            release: Mutex::new(release_rx),
            captured: AtomicBool::new(false),
        });
        tracing::subscriber::with_default(subscriber, || {
            orbit_store::test_support::with_task_commit_write_hold(
                Duration::from_millis(2_100),
                || runtime.submit_decision_via_channel(params),
            )
        })
    });

    let parked = parked_rx.recv_timeout(Duration::from_secs(10)).is_ok();
    let acquired_while_report_parked = parked
        && orbit_common::fs::io::try_acquire_exclusive_file_lock(
            &lock_path,
            "decision channel report regression probe",
        )
        .expect("probe the decision-channel store lock")
        .is_some();
    let _ = release_tx.send(());
    let result = submit.join().expect("decision submission thread");

    assert!(
        parked,
        "the slow task write did not emit a parked boundary report"
    );
    assert!(
        acquired_while_report_parked,
        "channels.lock stayed held while the task-boundary report subscriber was parked"
    );
    let answered = result.expect("the public decision-channel submission applies");
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(fx.status(&b.task), TaskStatus::Backlog);
}

#[test]
fn a_registered_channel_applies_an_answer_as_the_human_with_provenance() {
    let fx = registered_fixture();
    let channels = fx.runtime.list_decision_channels().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].epoch, 1);
    assert!(!channels[0].revoked);
    assert_eq!(channels[0].human, HUMAN);
    assert_eq!(channels[0].plugin, PLUGIN);
    // The key is mirrored to the bound plugin's secret store at
    // registration, without the store file ever naming it in listings.
    let secrets = fx.plugin_secrets();
    assert!(secrets.to_string().contains("channel_key"), "{secrets}");

    let b = fx.blocked();
    assert_eq!(b.request.state, DecisionRequestState::Open);
    assert_eq!(b.token.len(), 26, "{}", b.token);

    let answered = fx.submit_blocked(&b, "1").unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    let resolution = answered.resolution.unwrap();
    assert_eq!(resolution.answered_by, HUMAN);
    assert_eq!(resolution.answer_id, "1");
    let provenance = resolution.provenance.unwrap();
    assert!(
        provenance.starts_with("decision-channel email (request ")
            && provenance.contains(", token sha256:"),
        "{provenance}"
    );
    assert_eq!(
        fx.status(&b.task),
        TaskStatus::Backlog,
        "the answer applied"
    );
    let history = fx.runtime.get_task_history(&b.task).unwrap();
    let answered_entry = history
        .iter()
        .find(|entry| entry.event == DECISION_ANSWERED_EVENT)
        .expect("the applied answer is on the task history");
    assert_eq!(answered_entry.by, HUMAN, "written as the human");

    // The token is single-use: presenting it again is refused and the
    // original answer stands.
    let comments = fx.comments(&b.task);
    refusal(fx.submit_blocked(&b, "dismiss"), "token_consumed");
    assert_eq!(fx.status(&b.task), TaskStatus::Backlog);
    assert_eq!(
        fx.comments(&b.task),
        comments,
        "the replay wrote no comment"
    );
}

#[test]
fn every_refusal_applies_nothing() {
    use DecisionSubmitCaller::{Agent, Operator, Unknown};

    let fx = registered_fixture();

    // One fresh request is refused several unauthenticated and
    // parse-level ways, then still accepts the real answer — proving
    // none of those refusals consumed the token or moved the task.
    let b = fx.blocked();
    // Each row names the refusal and tweaks one field of the plugin's
    // otherwise-valid submission.
    type Tweak = fn(&mut DecisionSubmitParams);
    let cases: [(&str, Tweak); 14] = [
        ("caller_not_allowed", |p| p.caller = Agent),
        ("caller_not_allowed", |p| p.caller = Unknown),
        ("channel_not_registered", |p| p.caller = rogue_plugin()),
        ("sender_not_allowed", |p| p.sender = Some(FORGED.into())),
        ("sender_not_allowed", |p| {
            p.caller = Operator;
            p.sender = Some(FORGED.into());
        }),
        ("authentication_failed", |p| {
            p.auth_results = Some(AUTH_DMARC_FAIL.into())
        }),
        ("authentication_failed", |p| p.auth_results = None),
        ("sender_not_allowed", |p| p.sender = None),
        ("token_invalid", |p| p.token = "A".repeat(26)),
        ("token_invalid", |p| p.token = "not-a-token".into()),
        ("answer_not_allowed", |p| p.answer_id = "bogus".into()),
        ("answer_not_allowed", |p| p.answer_id = String::new()),
        // An unregistered channel and an unknown request are refused by name too.
        ("channel_not_registered", |p| p.channel = "pager".into()),
        ("request_not_found", |p| p.request_id = "dr-gone".into()),
    ];
    let comments = fx.comments(&b.task);
    for (index, (want, tweak)) in cases.iter().enumerate() {
        let mut params = plugin_submission(&b.request, "1", &b.token);
        tweak(&mut params);
        refusal(fx.runtime.submit_decision_via_channel(params), want);
        assert_eq!(fx.status(&b.task), TaskStatus::Blocked, "case {index}");
        assert_eq!(
            fx.comments(&b.task),
            comments,
            "case {index} wrote a comment"
        );
        fx.assert_state(&b.task, &b.request.id, DecisionRequestState::Open);
    }

    // None of it burned the credential (lowercase presentation included).
    let lower = b.token.to_ascii_lowercase();
    let answered = fx.submit(&b.request, "dismiss", &lower).unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(
        fx.status(&b.task),
        TaskStatus::Blocked,
        "dismiss stays blocked"
    );

    // Expired: the token dies with its request, and the authenticated
    // refusal consumes it.
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    let expired = fx.blocked_expiring(Some(past));
    assert_eq!(expired.request.state, DecisionRequestState::Expired);
    refusal(fx.submit_blocked(&expired, "1"), "request_expired");
    assert_eq!(fx.status(&expired.task), TaskStatus::Blocked);
    // A consumed token stays consumed.
    refusal(fx.submit_blocked(&expired, "1"), "token_consumed");

    // Superseded: a human answered on another surface first.
    let superseded = fx.blocked();
    fx.human()
        .answer_decision_request_as_human(
            &superseded.request.id,
            "dismiss",
            None,
            HUMAN.to_string(),
            Some("dashboard".into()),
        )
        .unwrap();
    fx.assert_refused_untouched(&superseded, "request_superseded", || {
        fx.submit_blocked(&superseded, "1")
    });

    // Rotation invalidates every outstanding token (§4): one minted
    // under the previous epoch is refused even though unexpired, and a
    // fresh token under the new epoch applies.
    let rotated = fx.blocked();
    let info = fx.runtime.rotate_decision_channel_key(CHANNEL).unwrap();
    assert_eq!(info.epoch, 2);
    refusal(fx.submit_blocked(&rotated, "1"), "token_consumed");
    assert_eq!(fx.status(&rotated.task), TaskStatus::Blocked);
    let fresh = fx.mint(&rotated.request);
    assert_ne!(fresh, rotated.token);
    fx.submit(&rotated.request, "1", &fresh).unwrap();
    assert_eq!(fx.status(&rotated.task), TaskStatus::Backlog);

    // Revocation is immediate.
    let revoked = fx.blocked();
    fx.runtime.revoke_decision_channel(CHANNEL).unwrap();
    fx.assert_refused_untouched(&revoked, "channel_revoked", || {
        fx.submit_blocked(&revoked, "1")
    });
}

/// The token contract a transport plugin implements from its mirrored
/// `channel_key` secret alone: HMAC-SHA256 over
/// `request|task|epoch|expires_unix`, base32, 26 chars. Recomputed here
/// independently of core's mint (group-wise base32 over 5-byte blocks)
/// and submitted as the plugin would present it.
fn plugin_minted_token(
    key_hex: &str,
    epoch: u64,
    request: &str,
    task: &str,
    expires_unix: i64,
) -> String {
    let key: Vec<u8> = (0..key_hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&key_hex[index..index + 2], 16).unwrap())
        .collect();
    let message = format!("{request}|{task}|{epoch}|{expires_unix}");
    let mut block = [0u8; 64];
    block[..key.len()].copy_from_slice(&key);
    let inner_pad: Vec<u8> = block.iter().map(|byte| byte ^ 0x36).collect();
    let outer_pad: Vec<u8> = block.iter().map(|byte| byte ^ 0x5c).collect();
    let inner = Sha256::digest([inner_pad.as_slice(), message.as_bytes()].concat());
    let mac = Sha256::digest([outer_pad.as_slice(), inner.as_slice()].concat());
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut token = String::new();
    for group in mac.chunks(5) {
        let mut value: u64 = 0;
        for byte in group {
            value = (value << 8) | u64::from(*byte);
        }
        value <<= 8 * (5 - group.len());
        for shift in (0..8).rev() {
            token.push(ALPHABET[((value >> (shift * 5)) & 0x1f) as usize] as char);
        }
    }
    token.chars().take(26).collect()
}

#[test]
fn a_plugin_minted_token_from_the_mirrored_secret_verifies() {
    // Pinned against an independent implementation (Python
    // hmac/hashlib/base64) for key bytes 0x00..=0x1f and message
    // `dr-test|ORB-1|1|1893456000`, so the contract cannot drift in both
    // Rust copies at once.
    assert_eq!(
        plugin_minted_token(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            1,
            "dr-test",
            "ORB-1",
            1_893_456_000,
        ),
        "X4SKRKLQ5ZXQZIYYPW4VGNZQ2W"
    );

    let fx = registered_fixture();
    let secrets = fx.plugin_secrets();
    let key_hex = secrets["secrets"]["channel_key"]["value"]
        .as_str()
        .expect("registration mirrored the channel key");
    let b = fx.blocked();
    let plugin_token = plugin_minted_token(
        key_hex,
        1,
        &b.request.id,
        &b.task,
        b.request.expires_at.timestamp(),
    );
    assert_eq!(
        plugin_token, b.token,
        "core and the plugin-side contract mint the same token"
    );
    let answered = fx.submit(&b.request, "1", &plugin_token).unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(fx.status(&b.task), TaskStatus::Backlog);
}

#[test]
fn an_operator_caller_may_test_a_channel() {
    let fx = registered_fixture();
    let b = fx.blocked();
    let params = DecisionSubmitParams {
        caller: DecisionSubmitCaller::Operator,
        sender: None,
        ..plugin_submission(&b.request, "dismiss", &b.token)
    };
    let answered = fx.runtime.submit_decision_via_channel(params).unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.unwrap().answered_by, HUMAN);
    assert_eq!(fx.status(&b.task), TaskStatus::Blocked);
}

#[test]
fn an_empty_plugin_sender_is_refused_with_or_without_an_allowlist() {
    for allowed_senders in [vec![], vec![SENDER.to_string()]] {
        let fx = fixture();
        fx.register_with_senders(allowed_senders);
        let b = fx.blocked();

        for sender in ["", " \t "] {
            let mut params = plugin_submission(&b.request, "1", &b.token);
            params.sender = Some(sender.to_string());
            fx.assert_refused_untouched(&b, "sender_not_allowed", || {
                fx.runtime.submit_decision_via_channel(params)
            });
            fx.assert_state(&b.task, &b.request.id, DecisionRequestState::Open);
        }
    }
}

#[test]
fn registration_refuses_an_automation_human_and_a_duplicate() {
    let fx = fixture();
    refusal(
        fx.runtime
            .register_decision_channel("robot", "task-pilot", PLUGIN, vec![]),
        "automation actor",
    );
    fx.register();
    refusal(
        fx.runtime
            .register_decision_channel(CHANNEL, HUMAN, PLUGIN, vec![]),
        "already registered",
    );
    // The refused duplicate generated no new key: the original channel
    // still verifies at epoch 1.
    assert_eq!(fx.runtime.list_decision_channels().unwrap()[0].epoch, 1);
}

#[test]
fn submission_is_refused_while_the_decision_channel_is_disabled() {
    let fx = fixture_with_config("");
    fx.register();
    let b = fx.blocked();
    fx.assert_refused_untouched(&b, "decision_channel_disabled", || {
        fx.submit_blocked(&b, "1")
    });
}

#[test]
fn the_digest_raises_one_proposed_approval_per_proposed_task_idempotently() {
    let fx = registered_fixture();
    let one = fx.proposed_task("First proposal");
    let two = fx.proposed_task("Second proposal");
    // An unrelated open request expiring within the 24-hour digest
    // window rides along in the same call.
    let soon = chrono::Utc::now() + chrono::Duration::hours(1);
    let expiring = fx.blocked_expiring(Some(soon)).request;

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    assert_eq!(digest.raised.len(), 2, "{:?}", digest.raised);
    let mut raised: Vec<&str> = digest.raised.iter().map(|r| r.task_id.as_str()).collect();
    raised.sort_unstable();
    let mut both = vec![one.as_str(), two.as_str()];
    both.sort_unstable();
    assert_eq!(raised, both);
    for request in &digest.raised {
        assert_eq!(request.gate, DecisionGate::ProposedApproval);
        assert_eq!(request.raised_by, "digest");
        assert_eq!(request.state, DecisionRequestState::Open);
        let answers: Vec<&str> = request.answers.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(answers, ["approve", "reject"]);
    }
    assert!(
        digest.expiring.iter().any(|r| r.id == expiring.id),
        "the request expiring within the window is returned: {:?}",
        digest.expiring
    );
    for expected in digest.raised.iter().chain([&expiring]) {
        assert!(
            digest.requests.iter().any(|r| r.id == expected.id),
            "the digest carries {}",
            expected.id
        );
    }

    // A second call raises nothing new and returns the same ids.
    let again = fx.digest(Fixture::plugin_caller()).unwrap();
    assert!(again.raised.is_empty(), "{:?}", again.raised);
    let ids = |requests: &[DecisionRequest]| -> Vec<String> {
        let mut ids: Vec<String> = requests.iter().map(|r| r.id.clone()).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(ids(&again.requests), ids(&digest.requests));
    assert_eq!(fx.requested_entries(&one), 1);
    assert_eq!(fx.requested_entries(&two), 1);
}

#[test]
fn dangling_artifacts_do_not_abort_workspace_decision_and_task_listings() {
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::TaskArtifact;

    if !crate::dispatch_admission::isolated(
        "decision_channel::dangling_artifacts_do_not_abort_workspace_decision_and_task_listings",
    ) {
        return;
    }
    let fx = registered_fixture();
    let healthy = fx.proposed_task("Readable proposal");
    let damaged = fx.proposed_task("Proposal with missing evidence");
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let damaged_request = request_for(&digest.requests, &damaged);
    let healthy_request = request_for(&digest.requests, &healthy);
    // The workspace scan must also tolerate a broken task with no decisions.
    let no_decisions = fx.task();
    let registry = TaskRegistryStore::open(&task_registry_path(&fx.runtime.global_root())).unwrap();
    for id in [&damaged, &no_decisions] {
        let update = TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "checks/owner-gates-after.log",
                "passed",
            )],
            ..Default::default()
        };
        fx.runtime
            .update_task_with_identity(id, update, None, Some("codex".into()))
            .unwrap();
        let file = fx.runtime.get_task_artifact_manifest(id).unwrap().remove(0);
        let bundle = registry
            .canonical_task_bundle_path(&fx.runtime.workspace_id().unwrap(), id)
            .unwrap();
        // Reproduce the observed dangling manifest in this disposable store.
        std::fs::remove_file(bundle.join("artifacts").join(file.blob)).unwrap();
    }
    let decisions = fx.runtime.run_tool("orbit.decision.list", json!({}));
    let tasks = fx.runtime.run_tool(
        "orbit.task.list",
        json!({"fields": ["id", "title", "open_decisions"], "limit": 10}),
    );
    assert!(
        decisions.is_ok() && tasks.is_ok(),
        "decisions: {decisions:?}; tasks: {tasks:?}"
    );
    let decisions = decisions.unwrap();
    let tasks = tasks.unwrap();
    assert_eq!(decisions["count"], 1);
    assert_eq!(decisions["requests"][0]["id"], healthy_request.id);
    assert_eq!(tasks["tasks"].as_array().unwrap().len(), 3);
    for value in [&decisions, &tasks] {
        let warnings = value["warnings"]
            .as_array()
            .expect("partial listings disclose warnings");
        assert_eq!(warnings.len(), 2, "{value}");
        for id in [&damaged, &no_decisions] {
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.as_str().is_some_and(|text| text.contains(id))),
                "{value}"
            );
        }
    }
    for row in tasks["tasks"].as_array().unwrap() {
        if row["id"] == healthy {
            assert_eq!(row["open_decisions"][0]["id"], healthy_request.id);
        } else {
            assert!(
                row["open_decisions"].is_null(),
                "unreadable differs from no open decisions: {row}"
            );
        }
    }
    let digest = fx
        .digest(Fixture::plugin_caller())
        .expect("digest skips corrupt bundles too");
    assert_eq!(digest.requests.len(), 1);
    assert_eq!(digest.requests[0].id, healthy_request.id);
    assert_eq!(digest.warnings.len(), 2, "{digest:?}");
    for error in [
        fx.runtime.get_task(&damaged).unwrap_err(),
        fx.runtime
            .get_decision_request(&damaged_request.id)
            .unwrap_err(),
        fx.human()
            .answer_decision_request_as_human(
                &damaged_request.id,
                "reject",
                None,
                HUMAN.into(),
                Some("cli".into()),
            )
            .unwrap_err(),
        fx.runtime
            .run_tool("orbit.decision.list", json!({"task_id": damaged}))
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, OrbitError::TaskBundleCorrupt { .. }),
            "{error}"
        );
        assert!(
            error.to_string().contains(&damaged) || error.to_string().contains(&no_decisions),
            "strict scans identify the corrupt bundle they encounter: {error}"
        );
    }
}

#[test]
fn a_changed_proposal_supersedes_its_request_and_the_digest_raises_a_fresh_one() {
    let fx = registered_fixture();
    let task = fx.proposed_task("Mutable proposal");
    let first = fx.digest(Fixture::plugin_caller()).unwrap().raised[0].clone();

    // Changing the criteria the digest rendered supersedes the request.
    fx.update(json!({
        "id": task,
        "acceptance_criteria": ["It is decided differently."],
    }));
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);

    // The task is still proposed, so the next digest raises a fresh one.
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    assert_eq!(digest.raised.len(), 1);
    let second = digest.raised[0].clone();
    assert_ne!(second.id, first.id);
    assert_eq!(second.state, DecisionRequestState::Open);

    // Changing the description supersedes that one in turn.
    fx.update(json!({
        "id": task,
        "description": "A rewritten proposal.",
    }));
    fx.assert_state(&task, &second.id, DecisionRequestState::Superseded);
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    assert_eq!(digest.raised.len(), 1);
    assert_eq!(fx.status(&task), TaskStatus::Proposed);
}

/// A superseded request stays superseded: restoring the text it was raised
/// against, or returning to its status, never reopens it.
#[test]
fn a_superseded_proposal_request_stays_superseded_after_the_task_is_restored() {
    let fx = registered_fixture();
    let task = fx.proposed_task("Round-trip proposal");
    let first = fx.digest(Fixture::plugin_caller()).unwrap().raised[0].clone();

    // Edit the criteria and put them back.
    fx.update(json!({"id": task, "acceptance_criteria": ["It is decided differently."]}));
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);
    fx.update(json!({"id": task, "acceptance_criteria": ["It is decided."]}));
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);
    assert!(
        fx.requests(&task)
            .iter()
            .all(|request| request.state != DecisionRequestState::Open)
    );

    // Edit the description and put it back.
    fx.update(json!({"id": task, "description": "A rewritten proposal."}));
    fx.update(json!({"id": task, "description": "A proposed task awaiting approval."}));
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);

    // The next digest raises a fresh request; a fresh request stays open and
    // idempotent while nothing changes.
    let second = fx.digest(Fixture::plugin_caller()).unwrap().raised[0].clone();
    assert_ne!(second.id, first.id);
    fx.assert_state(&task, &second.id, DecisionRequestState::Open);
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);
    assert!(
        fx.digest(Fixture::plugin_caller())
            .unwrap()
            .raised
            .is_empty()
    );

    // Leave `proposed` and come back.
    fx.update(json!({"id": task, "status": "backlog"}));
    fx.assert_state(&task, &second.id, DecisionRequestState::Superseded);
    fx.update(json!({"id": task, "status": "proposed"}));
    assert_eq!(fx.status(&task), TaskStatus::Proposed);
    fx.assert_state(&task, &second.id, DecisionRequestState::Superseded);
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);
    let third = fx.digest(Fixture::plugin_caller()).unwrap().raised[0].clone();
    assert_ne!(third.id, second.id);
    fx.assert_state(&task, &third.id, DecisionRequestState::Open);
}

#[test]
fn a_review_approval_binds_to_the_candidate_and_supersedes_on_replacement() {
    let fx = registered_fixture();
    let task = fx.task();
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.update(json!({"id": task, "status": "review"}));
    assert_eq!(fx.status(&task), TaskStatus::Review);
    let description = fx.runtime.get_task(&task).unwrap().description;

    fx.hold_candidate(&task, "candidate-a");
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let first = request_for(&digest.raised, &task);
    assert_eq!(first.gate, DecisionGate::ReviewApproval);
    assert_eq!(first.state, DecisionRequestState::Open);
    let (candidate, status) = revision(&first);
    assert_eq!(status, &TaskStatus::Review);
    assert_eq!(candidate, Some("candidate-a"));

    // A new candidate replaces the reviewed one: superseded, then the
    // next digest raises a fresh request bound to the new candidate.
    fx.hold_candidate(&task, "candidate-b");
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let second = request_for(&digest.raised, &task);
    assert_ne!(second.id, first.id);
    assert_eq!(second.state, DecisionRequestState::Open);
    assert_eq!(revision(&second).0, Some("candidate-b"));

    // Holding the first candidate again does not revive its request.
    fx.hold_candidate(&task, "candidate-a");
    fx.assert_state(&task, &first.id, DecisionRequestState::Superseded);
    fx.assert_state(&task, &second.id, DecisionRequestState::Superseded);

    // A spec round trip on a review task supersedes its request for good.
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let third = request_for(&digest.raised, &task);
    assert_eq!(third.state, DecisionRequestState::Open);
    fx.update(json!({"id": task, "description": "Review text changed."}));
    fx.update(json!({"id": task, "description": description}));
    fx.assert_state(&task, &third.id, DecisionRequestState::Superseded);
}

/// A task that held a candidate, returned to `backlog` and was proposed again
/// still carries that candidate in its history. Its proposal approval is
/// raised without a candidate, so the historical candidate must not supersede
/// it: repeated digests keep answering the one open request.
#[test]
fn a_reproposed_task_with_a_historical_candidate_keeps_one_open_proposal() {
    let fx = registered_fixture();
    let task = fx.task();
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.update(json!({"id": task, "status": "review"}));
    fx.hold_candidate(&task, "candidate-a");
    fx.update(json!({"id": task, "status": "backlog"}));
    fx.update(json!({"id": task, "status": "proposed"}));
    assert_eq!(fx.status(&task), TaskStatus::Proposed);

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let raised: Vec<_> = digest
        .raised
        .iter()
        .filter(|request| request.task_id == task)
        .collect();
    assert_eq!(raised.len(), 1, "{:?}", digest.raised);
    let proposal = raised[0].clone();
    assert_eq!(proposal.gate, DecisionGate::ProposedApproval);
    let (candidate, status) = revision(&proposal);
    assert_eq!(status, &TaskStatus::Proposed);
    assert_eq!(candidate, None);
    let open_requests = || -> Vec<String> {
        fx.requests(&task)
            .into_iter()
            .filter(|request| request.state == DecisionRequestState::Open)
            .map(|request| request.id)
            .collect()
    };
    assert_eq!(open_requests(), vec![proposal.id.clone()]);

    // The unchanged proposal raises nothing new and stays the one open request.
    let again = fx.digest(Fixture::plugin_caller()).unwrap();
    assert!(
        again.raised.iter().all(|request| request.task_id != task),
        "{:?}",
        again.raised
    );
    assert_eq!(open_requests(), vec![proposal.id.clone()]);
    assert_eq!(fx.requested_entries(&task), 1);
}

#[test]
fn the_digest_writes_nothing_while_the_decision_channel_is_disabled() {
    let fx = fixture_with_config("");
    fx.register();
    let task = fx.proposed_task("Undecided while disabled");
    let history = fx.runtime.get_task_history(&task).unwrap().len();

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    assert!(digest.requests.is_empty());
    assert!(digest.raised.is_empty());
    assert!(digest.expiring.is_empty());
    assert_eq!(fx.requested_entries(&task), 0, "no request was written");
    assert_eq!(
        fx.runtime.get_task_history(&task).unwrap().len(),
        history,
        "the digest wrote no history entry"
    );
}

#[test]
fn the_digest_refuses_agents_and_plugins_the_channel_is_not_bound_to() {
    let fx = registered_fixture();
    let task = fx.proposed_task("Guarded proposal");

    for (caller, code) in [
        (DecisionSubmitCaller::Agent, "caller_not_allowed"),
        (DecisionSubmitCaller::Unknown, "caller_not_allowed"),
        (rogue_plugin(), "channel_not_registered"),
    ] {
        refusal(fx.digest(caller), code);
        assert_eq!(fx.requested_entries(&task), 0, "the refusal raised nothing");
    }

    // The bound plugin and the operator both get the digest.
    assert_eq!(fx.digest(Fixture::plugin_caller()).unwrap().raised.len(), 1);
    let operator = fx.digest(DecisionSubmitCaller::Operator).unwrap();
    assert!(operator.raised.is_empty());
    assert_eq!(operator.requests.len(), 1);
}

#[test]
fn an_approve_through_the_channel_lands_like_a_screened_approval() {
    let fx = registered_fixture();
    let decided = fx.proposed_task("Decided by channel");
    // A channel approval is screened: only a prepared footprint (or the
    // no-diff disposition) is admitted, so prepare one here.
    fx.prepare_footprint(&decided);
    let rejected = fx.proposed_task("Rejected by channel");
    let direct = fx.proposed_task("Decided directly");

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let approve_request = request_for(&digest.raised, &decided);
    let answered = fx.submit_minted(&approve_request, "approve").unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.unwrap().answered_by, HUMAN);

    // The screened approval a CLI approve runs lands the same way.
    fx.runtime
        .approve_task_screened(&direct, None, None, None, None)
        .unwrap();
    assert_eq!(fx.status(&decided), fx.status(&direct));
    assert_eq!(fx.status(&decided), TaskStatus::Backlog);

    let reject_request = request_for(&digest.raised, &rejected);
    fx.submit_minted(&reject_request, "reject").unwrap();
    assert_eq!(fx.status(&rejected), TaskStatus::Rejected);
}

#[test]
fn a_digest_approve_is_refused_while_the_pilot_holds_or_no_footprint_is_prepared() {
    let fx = registered_fixture();
    // Held by the pilot (duplicate) and without a footprint.
    let held = fx.proposed_task("Held by pilot");
    fx.seed_pilot_duplicate_hold(&held);
    // No pilot hold, but no footprint either.
    let unprepared = fx.proposed_task("No footprint yet");
    // No footprint, with the supported no-diff disposition.
    let no_diff = fx.proposed_task("No diff expected");
    fx.human()
        .update_task_as_human(
            &no_diff,
            TaskUpdateParams {
                tags: Some(vec!["no-diff-expected".to_string()]),
                ..Default::default()
            },
            HUMAN.to_string(),
        )
        .unwrap();

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();

    // The pilot-held proposal is refused twice with the hold refusal:
    // the first refusal neither answered the request nor consumed the
    // token, and the task stays proposed.
    let held_request = request_for(&digest.raised, &held);
    let held_token = fx.mint(&held_request);
    for _ in 0..2 {
        let refused = refusal(
            fx.submit(&held_request, "approve", &held_token),
            "pilot_duplicate",
        );
        assert!(!refused.contains("token_consumed"), "{refused}");
        assert_eq!(fx.status(&held), TaskStatus::Proposed);
        let request = fx.open_proposal(&held);
        assert_eq!(request.id, held_request.id);
        assert!(request.resolution.is_none(), "{request:?}");
    }

    // Without a prepared footprint the approval is refused and applies
    // nothing; once the footprint is declared, the same public path
    // approves it as the human.
    let unprepared_request = request_for(&digest.raised, &unprepared);
    refusal(
        fx.submit_minted(&unprepared_request, "approve"),
        "missing_context_files",
    );
    assert_eq!(fx.status(&unprepared), TaskStatus::Proposed);
    assert!(fx.open_proposal(&unprepared).resolution.is_none());

    fx.prepare_footprint(&unprepared);
    fx.digest(Fixture::plugin_caller()).unwrap();
    let prepared = fx.open_proposal(&unprepared);
    let answered = fx.submit_minted(&prepared, "approve").unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.unwrap().answered_by, HUMAN);
    assert_eq!(fx.status(&unprepared), TaskStatus::Backlog);
    let context_files = fx.runtime.get_task(&unprepared).unwrap().context_files;
    assert!(
        !context_files.is_empty(),
        "an admitted proposal carries its prepared footprint"
    );

    // The no-diff disposition needs no footprint.
    let no_diff_request = request_for(&digest.raised, &no_diff);
    let answered = fx.submit_minted(&no_diff_request, "approve").unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(fx.status(&no_diff), TaskStatus::Backlog);
}

/// A throwaway RSA test key (generated for these tests only; it signs
/// nothing in production). The matching public key is served by
/// [`FixtureResolver`] as the `test1` selector for `example.com`.
const DKIM_TEST_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEugIBADANBgkqhkiG9w0BAQEFAASCBKQwggSgAgEAAoIBAQCm2u0dz1peTq24\nhrMtjjX3pNRgq6HkFYgmXQeCM6v4I0saUprgYl+/nCzhjMYmDCmAMsSd05H/WQF3\n7S1s/zxip4T8n/ZK2F4d/eDbWww5zWbYNJctLvjJqbJtv5WuOzhbvoQ2Y/kVxZQn\nIOMKyjXD9hVj/1JMoPhDYMj0WDe3eBo6A7A2zGmAbDgZ1//AVNQlo42YBvMdlkoJ\nwD6y1d0cQb7gLoVNF8DHpGr39gpRqWEreO80+onas4cPFLy1a8LCq68GXNJWP4kM\nCEnLp69wOjafsToTEVw9CtYVHpLZq0fvG52nwQrlAxtisnMz4mVXuwlELiHBsNUf\n2i1EYab5AgMBAAECggEACpAvlMSMzw2iF92HOGoZZo0r2Nbkz+DBFtSq5gZJa9Mo\nLQlvvqbbsvC7MqhL3xrEAda09dzwED9dRW9hgdYCx6gdVjT2TbS2+KUa+CgidQJY\n8XUa4QRpQbMZlbMcUcR0A/LiweLAYS15/KQrW4KDtt08a3E9j5ZdQT6+WiG/nUDG\nAnMLUHwZcuXD1CV8c7xnp/r1BI1NGDZHkkzhaiKD7O8YF9Dm2aER6/RVjV1FBWbg\njSB0tFYnjWEF6MtlaoJKqeGJuJc8mOXdcjw4IjrJw2dXfY9JFJ5b5jFMNsyJ8Uoi\nVz1nYb2l7xOgxktLTnYmV5Z7N/6Xg9AXd5ESh2mHhQKBgQDerHHohl+BMs7AjQeG\nJZVcu5/7NYghWyfBqqGvdjaeai8m779g3ZASTfIimCERCLMRG984pqcrXYB3aem4\nY5+t/JYLqp643QIoH/LvBOPkfo+eJTJSQA5Oa8Ov2V+vO21ITOXulaoI3DqR2+Pm\nL5sRdUPY1ho8zPb91OMUZOLVfwKBgQC/09gia2rgGNjXQTXgI4yZon6qqe0aGpSN\n45CaCJymU5szADrm0x8QNY/iNw7jRx4NLRfnQFfxHvIST0KyglAM3YTJizGX5FNC\n7dTqVBD7XjFrkNh08Rhe68wnJycOskpcmEqfwn4xcCkcAtKl1flpIE0urEjdV3kJ\nejIELE9vhwKBgBNPGtWSCWgXsswJrrRYP/p5nqbW/oX99JWbqUeg1Z2pEYur0Imi\nzOJ5ddzAGmXyvixmoVW1RdvB0OkJ+yVtJiSdOZmrYvYe3BgEpTlK0EQ5IVUSXqm5\nOkRcnex6ygxU9+YEHRAyVDtOdZm+WqXUTPsXkxmTf1T0ZRqfwk07blwbAoGAN2xG\nmxM8CslDBVZC8aJzwgPP4xE7fvpFB4XDRq3MTMYFJkQfwIcpB27iRnKv63ylsxrf\nOCJABltxh1a9J4Q9CqqJ7g04Y/FFGSZMCPQHAAlj7/cmgwsRhoNMNde7EiTzFTOv\nA9+WxEPqrLwoDl/3MqdxhVWjq8fVIabbgpcm0EsCfwR4UYOfsGFcIJl9CAIiVRka\nu44WfM0XDgDiTU0/jwShujodtpLxSItBlePia5LVhCnUzAOwWNyqQ8N18Q0FP0ZQ\nFjOSep46iySeOWMhVNShL0N8ASafJp8HiCZe+o4C6b4dRRVHTqWC4smRLAfFfMEa\nBCHfmiJz+zMwgBzvMH4=\n-----END PRIVATE KEY-----\n";

struct FixtureResolver;

impl DkimTxtResolver for FixtureResolver {
    fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String> {
        if name != "test1._domainkey.example.com" {
            return Ok(Vec::new());
        }
        let key = RsaPrivateKey::from_pkcs8_pem(DKIM_TEST_PRIVATE_KEY_PEM)
            .map_err(|error| error.to_string())?;
        let der = key
            .to_public_key()
            .to_public_key_der()
            .map_err(|error| error.to_string())?;
        Ok(vec![format!(
            "v=DKIM1; k=rsa; p={}",
            STANDARD.encode(der.as_bytes())
        )])
    }
}

struct FailingResolver;

impl DkimTxtResolver for FailingResolver {
    fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String> {
        Err(format!("DNS TXT lookup for `{name}` timed out after 5s"))
    }
}

/// The message headers the fixture signs, in `h=` order.
const DKIM_HEADERS: [(&str, &str); 4] = [
    ("From", "Operator <operator@example.com>"),
    ("To", "orbit@example.com"),
    ("Subject", "Re: decision"),
    ("Date", "Mon, 26 Oct 2026 10:00:00 +0000"),
];

/// The bytes the fixture key signs (RFC 6376 §3.7): the signed headers in
/// `h=` order, then the DKIM-Signature with its `b=` emptied. Relaxed
/// canonicalisation lowercases header names; simple keeps them as written.
fn dkim_signing_input(canonicalization: &str, signed_value: &str) -> String {
    let mut input = String::new();
    if canonicalization.starts_with("relaxed/") {
        for (name, value) in DKIM_HEADERS {
            input.push_str(&format!("{}:{value}\r\n", name.to_ascii_lowercase()));
        }
        input.push_str(&format!("dkim-signature:{signed_value}"));
    } else {
        for (name, value) in DKIM_HEADERS {
            input.push_str(&format!("{name}: {value}\r\n"));
        }
        input.push_str(&format!("DKIM-Signature: {signed_value}"));
    }
    input
}

/// Signs `input` with the fixture key, returning the base64 signature.
fn dkim_sign(input: &str) -> String {
    let key = RsaPrivateKey::from_pkcs8_pem(DKIM_TEST_PRIVATE_KEY_PEM).unwrap();
    let signature: rsa::pkcs1v15::Signature = SigningKey::<Sha256>::new(key).sign(input.as_bytes());
    STANDARD.encode(signature.to_bytes())
}

/// The simple-body canonical form of `body` and its body hash (`bh=`).
fn dkim_canonical_body(body: &str) -> (String, String) {
    let canonical = format!("{}\r\n", body.trim_end_matches(['\r', '\n']));
    let bh = STANDARD.encode(Sha256::digest(canonical.as_bytes()));
    (canonical, bh)
}

/// A message as presented: the DKIM-Signature header carrying `signature_value`,
/// then the signed headers and `body`.
fn dkim_message(signature_value: &str, body: &str) -> String {
    let headers: String = DKIM_HEADERS
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    format!("DKIM-Signature: {signature_value}\r\n{headers}\r\n{body}")
}

/// Sign a fixed message with the fixture key under relaxed/relaxed
/// canonicalisation, implemented here directly so the test does not reuse
/// core's canonicaliser.
fn dkim_signed_message(body: &str) -> String {
    let (canonical_body, bh) = dkim_canonical_body(body);
    let unsigned = format!(
        "v=1; a=rsa-sha256; c=relaxed/relaxed; d=example.com; s=test1; h=from:to:subject:date; bh={bh}; b="
    );
    let signature = dkim_sign(&dkim_signing_input("relaxed/relaxed", &unsigned));
    dkim_message(&format!("{unsigned}{signature}"), &canonical_body)
}

/// Sign a message whose `b=` tag is followed by `bh=`, as mail in the
/// wild is normally emitted, returning the message and the base64
/// signature. `signed_b` is the `b` tag as the signer hashed it (empty
/// value, per RFC 6376 §3.7); `presented_b` is the same tag with `{sig}`
/// replaced by the signature, so a case can present the value folded or
/// spaced differently from how it was hashed. Canonicalisation is
/// implemented here directly so the test does not reuse core's.
fn dkim_signed_message_with_following_tag(
    body: &str,
    canonicalization: &str,
    signed_b: &str,
    presented_b: &str,
) -> (String, String) {
    let (canonical_body, bh) = dkim_canonical_body(body);
    let signed_value = format!(
        "v=1; a=rsa-sha256; c={canonicalization}; d=example.com; s=test1; h=from:to:subject:date; {signed_b}; bh={bh}"
    );
    let signature = dkim_sign(&dkim_signing_input(canonicalization, &signed_value));
    let presented_value = signed_value.replacen(
        &format!("{signed_b};"),
        &format!("{};", presented_b.replace("{sig}", &signature)),
        1,
    );
    (dkim_message(&presented_value, &canonical_body), signature)
}

/// A DKIM-requiring channel with one blocked task awaiting an answer.
fn dkim_required() -> (Fixture, Blocked) {
    let fx = fixture();
    fx.register_dkim(true);
    let b = fx.blocked();
    (fx, b)
}

#[test]
fn core_dkim_verification_applies_a_signed_message_and_refuses_forgeries() {
    let fx = fixture();
    fx.register_dkim(false);
    let b = fx.blocked();
    let signed = dkim_signed_message("approve");

    // One body byte changed: the body hash no longer matches.
    let tampered = signed.replacen("approve\r\n", "approv3\r\n", 1);
    fx.assert_dkim_refused(&b, tampered, "a tampered body");

    // A forged Authentication-Results claiming pass on an unsigned raw
    // message: core's own result contradicts the forwarded verdict.
    let unsigned = "From: Operator <operator@example.com>\r\nTo: orbit@example.com\r\nSubject: Re: decision\r\nDate: Mon, 26 Oct 2026 10:00:00 +0000\r\n\r\napprove\r\n".to_string();
    fx.assert_dkim_refused(&b, unsigned, "an unsigned message");

    // A DNS failure is retryable and consumes nothing.
    let refused = fx
        .submit_dkim(&b, Some(signed.clone()), &FailingResolver)
        .expect_err("a DNS failure is refused");
    assert!(
        matches!(&refused, OrbitError::DecisionRefused { code, .. }
        if code.as_str() == "dkim_verification_unavailable" && code.is_retryable()),
        "{refused}"
    );
    let refused = refused.to_string();
    assert!(
        refused.contains("dkim_verification_unavailable"),
        "{refused}"
    );
    assert!(refused.contains("retryable"), "{refused}");
    assert_eq!(fx.status(&b.task), TaskStatus::Blocked);

    // The signed message verifies through the injected resolver and the
    // answer applies — proving none of the refusals above consumed the
    // token.
    fx.assert_dkim_applies(&b, signed, "the signed message");
}

#[test]
fn core_dkim_accepts_legal_folding_around_a_b_value_with_a_following_tag() {
    // RFC 6376 §3.7: emptying `b=` deletes the value and all surrounding
    // whitespace. Each case signs the emptied form independently, then
    // presents the signature folded or spaced, with `bh=` after `b=`.
    let folded = "b=\r\n\t{sig}";
    let spaced = "b = \r\n\t{sig} \r\n\t";
    let cases: [(&str, &str, &str, &str); 6] = [
        ("unfolded relaxed", "relaxed/relaxed", "b=", "b={sig}"),
        ("folded relaxed", "relaxed/relaxed", "b=", folded),
        ("spaced relaxed", "relaxed/relaxed", "b =", spaced),
        ("folded simple", "simple/simple", "b=", folded),
        ("folded simple/relaxed", "simple/relaxed", "b=", folded),
        ("folded relaxed/simple", "relaxed/simple", "b=", folded),
    ];
    for (label, canonicalization, signed_b, presented_b) in cases {
        let fx = fixture();
        fx.register_dkim(true);
        let b = fx.blocked();
        let (signed, signature) = dkim_signed_message_with_following_tag(
            "approve",
            canonicalization,
            signed_b,
            presented_b,
        );

        // A changed body byte is refused and applies nothing.
        let tampered = signed.replacen("approve\r\n", "approv3\r\n", 1);
        fx.assert_dkim_refused(&b, tampered, label);

        // A changed signature byte is refused and applies nothing.
        let mut chars: Vec<char> = signature.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        let forged = signed.replacen(&signature, &chars.into_iter().collect::<String>(), 1);
        fx.assert_dkim_refused(&b, forged, label);

        // The valid message verifies and applies the answer, proving the
        // refusals above did not consume the token.
        fx.assert_dkim_applies(&b, signed, label);
    }
}

crate::scenario_table!(
    raw_message_ingress,
    simple_body_signature_survives_governed_submit
);

fn simple_body_signature_survives_governed_submit() {
    use orbit_core::test_support::with_decision_dkim_resolver;
    use orbit_tools::ToolContext;
    use orbit_types::policy::Role;
    use orbit_types::tool::{McpCapability, ToolSessionContext};

    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/decision-simple-body.json")).unwrap();
    let signed = fixture["raw_message"].as_str().unwrap();
    assert!(signed.ends_with("approve \r\n"));
    let tampered = signed.replacen("approve \r\n", "approv3 \r\n", 1);

    // Both entry points must refuse the changed byte, leave all task state
    // alone, and then accept the authentic message using the very same token.
    for through_tool in [false, true] {
        let (fx, b) = dkim_required();
        let comments_before = fx.comments(&b.task);
        let submit = |raw_message: &str| {
            if through_tool {
                with_decision_dkim_resolver(FixtureResolver, || {
                    fx.runtime.run_tool_with_context_and_role(
                        "orbit.decision.submit",
                        json!({
                            "id": b.request.id,
                            "channel": CHANNEL,
                            "answer": "1",
                            "token": b.token,
                            "sender": SENDER,
                            "auth_results": AUTH_PASS,
                            "raw_message": raw_message,
                        }),
                        Role::Admin,
                        ToolContext {
                            session_context: ToolSessionContext {
                                effective_capabilities: [McpCapability::Operator].into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    )
                })
            } else {
                fx.submit_dkim(&b, Some(raw_message.to_string()), &FixtureResolver)
                    .map(|request| json!({"state": request.state.as_str()}))
            }
        };

        let refused = submit(&tampered).expect_err("a changed signed body byte is refused");
        assert!(
            refused.to_string().contains("dkim_verification_failed"),
            "{refused}"
        );
        assert_eq!(fx.status(&b.task), TaskStatus::Blocked);
        assert_eq!(fx.comments(&b.task), comments_before);
        fx.assert_state(&b.task, &b.request.id, DecisionRequestState::Open);
        assert!(fx.requests(&b.task)[0].resolution.is_none());

        let accepted = submit(signed).expect("the authentic simple body survives ingress");
        assert_eq!(accepted["state"], "answered");
        assert_eq!(fx.status(&b.task), TaskStatus::Backlog);
        let replay = submit(signed).expect_err("a successful submission consumes its token");
        assert!(replay.to_string().contains("token_consumed"), "{replay}");
    }
}

#[test]
fn core_dkim_verifies_a_body_ending_in_non_wsp_unicode_whitespace() {
    // RFC 6376 §3.4.4 trims only SP and HTAB from body lines, so U+00A0 is
    // signed content: a signature computed over it must still verify.
    let (fx, b) = dkim_required();
    let signed = dkim_signed_message("approve\u{a0}");
    fx.assert_dkim_applies(&b, signed, "a U+00A0 body");
}

#[test]
fn core_dkim_refuses_non_wsp_bytes_added_to_a_signed_body() {
    let (fx, b) = dkim_required();
    let signed = dkim_signed_message("approve");

    // U+00A0 added before the CRLF, or substituted for the last byte, is
    // body content the signer did not hash.
    for (label, tampered) in [
        (
            "added",
            signed.replacen("approve\r\n", "approve\u{a0}\r\n", 1),
        ),
        (
            "changed",
            signed.replacen("approve\r\n", "approv\u{a0}\r\n", 1),
        ),
    ] {
        fx.assert_dkim_refused(&b, tampered, label);
        fx.assert_state(&b.task, &b.request.id, DecisionRequestState::Open);
    }

    // SP and HTAB at line end are WSP under relaxed canonicalisation, so the
    // unchanged signature still verifies and applies the answer.
    let trailing_wsp = signed.replacen("approve\r\n", "approve \t\r\n", 1);
    fx.assert_dkim_applies(&b, trailing_wsp, "trailing WSP");
}

#[test]
fn core_dkim_refuses_non_wsp_bytes_added_to_a_signed_header() {
    // Subject is in h=, so U+00A0 added to its name or its value changes
    // bytes the signer hashed. Relaxed canonicalisation trims only SP and
    // HTAB from both.
    let (fx, b) = dkim_required();
    let signed = dkim_signed_message("approve");
    let subject = "Subject: Re: decision\r\n";
    for (label, tampered) in [
        (
            "value",
            signed.replacen(subject, "Subject: Re: decision\u{a0}\r\n", 1),
        ),
        (
            "name",
            signed.replacen(subject, "Subject\u{a0}: Re: decision\r\n", 1),
        ),
    ] {
        fx.assert_dkim_refused(&b, tampered, label);
    }
    fx.assert_dkim_applies(&b, signed, "the signed header");
}

#[test]
fn a_channel_requiring_dkim_refuses_a_submission_without_a_raw_message() {
    let fx = fixture();
    let info = fx
        .runtime
        .register_decision_channel_with_dkim_requirement(
            CHANNEL,
            HUMAN,
            PLUGIN,
            vec![SENDER.to_string()],
            true,
        )
        .unwrap();
    assert!(info.require_dkim_verification);
    let b = fx.blocked();

    refusal(
        fx.submit_dkim(&b, None, &FixtureResolver),
        "dkim_verification_required",
    );
    assert_eq!(fx.status(&b.task), TaskStatus::Blocked);

    let signed = dkim_signed_message("approve");
    fx.assert_dkim_applies(&b, signed, "the signed message");
}

/// The decision-channel store holds every channel's key, so an agent sandbox
/// masks it; on Linux a sentinel stands in for the directory. A nested
/// `orbit` must refuse every channel read and write there, not read the
/// sentinel as "no channel registered" or register into it [ORB-19338].
#[test]
fn a_masked_decision_channel_store_refuses_instead_of_reading_as_empty() {
    const MASKED: &str = "decision channel store is not visible from an agent sandbox";

    let fx = registered_fixture();
    let b = fx.blocked();
    let store = fx.global.join("state/decision-channels");
    let sentinel = store.join(orbit_core::runtime::plugin::sandbox_mask::PLUGIN_MASK_SENTINEL_FILE);
    std::fs::write(&sentinel, b"masked").unwrap();
    let before = std::fs::read(store.join("channels.json")).unwrap();

    refusal(fx.runtime.list_decision_channels(), MASKED);
    refusal(
        fx.runtime
            .register_decision_channel("sms", HUMAN, PLUGIN, vec![SENDER.to_string()]),
        MASKED,
    );
    refusal(fx.runtime.rotate_decision_channel_key(CHANNEL), MASKED);
    refusal(
        fx.runtime.mint_decision_token(CHANNEL, &b.request.id),
        MASKED,
    );
    refusal(fx.digest(Fixture::plugin_caller()), MASKED);
    refusal(fx.submit_blocked(&b, "1"), MASKED);

    assert_eq!(std::fs::read(store.join("channels.json")).unwrap(), before);
    assert_eq!(
        fx.status(&b.task),
        TaskStatus::Blocked,
        "nothing was applied"
    );

    std::fs::remove_file(&sentinel).unwrap();
    assert_eq!(fx.runtime.list_decision_channels().unwrap().len(), 1);
}
