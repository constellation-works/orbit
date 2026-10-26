//! The delegated-human decision channel (design
//! `docs/design/human-decision-channel/2_design.md` §3–§5): a registered
//! test channel submits a human's answer through core with a per-request
//! token, and every refusal in the §5 table applies nothing.

use std::path::PathBuf;

use orbit_core::application::task::{
    DecisionSubmitCaller, DecisionSubmitParams, DkimTxtResolver, FinalRecoveryCompletion,
    FinalRecoveryRequest, TaskUpdateParams,
};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState,
    TaskStatus,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const CHANNEL: &str = "email";
const PLUGIN: &str = "decision-mail";
const HUMAN: &str = "human:operator";
const SENDER: &str = "operator@example.com";
const AUTH_PASS: &str = "mx.example.com; dkim=pass header.d=example.com; spf=pass smtp.mailfrom=example.com; dmarc=pass";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    runs: std::cell::Cell<u32>,
}

fn fixture() -> Fixture {
    fixture_with_config("[decision_channel]\nenabled = true\n")
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

impl Fixture {
    fn human(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_actor(ActorIdentity::human(HUMAN))
    }

    fn task(&self) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Decide an escalated block",
                    "description": "Decision channel fixture task.",
                    "acceptance_criteria": ["Recovered."],
                    "complexity": "low",
                    "type": "chore",
                    "workspace": self.repo.to_string_lossy(),
                    "model": "codex"
                }),
            )
            .unwrap();
        let id = task["id"].as_str().unwrap().to_string();
        self.update(json!({"id": id, "plan": "1. Do it."}));
        self.update(json!({"id": id, "status": "backlog"}));
        id
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

    /// A blocked task carrying one open block-triage decision request.
    fn blocked_task_with_request(&self) -> (String, DecisionRequest) {
        self.blocked_task_with_request_expiring(None)
    }

    fn blocked_task_with_request_expiring(
        &self,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> (String, DecisionRequest) {
        let task = self.task();
        self.escalate(&task);
        let history = self.runtime.get_task_history(&task).unwrap();
        let blocked_at = history
            .iter()
            .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .expect("seeded blocked task records its block")
            .at;
        let mut request = DecisionRequest::new(
            task.clone(),
            DecisionGate::BlockTriage,
            vec![
                DecisionAnswer {
                    id: "1".to_string(),
                    label: "Drop criterion 2".to_string(),
                    consequence: "the old API stays retired".to_string(),
                },
                DecisionAnswer {
                    id: "dismiss".to_string(),
                    label: "Dismiss".to_string(),
                    consequence: "Leave blocked, stop asking for this episode.".to_string(),
                },
            ],
            DecisionBinding::BlockEpisode {
                episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            },
            "block_triage".to_string(),
            chrono::Utc::now(),
        );
        if let Some(expires_at) = expires_at {
            request.expires_at = expires_at;
        }
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
            .runtime
            .get_task_decision_requests(&task)
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == request.id)
            .unwrap();
        (task, stored)
    }

    fn register(&self) {
        self.register_with_senders(vec![SENDER.to_string()]);
    }

    fn register_with_senders(&self, allowed_senders: Vec<String>) {
        self.runtime
            .register_decision_channel(CHANNEL, HUMAN, PLUGIN, allowed_senders)
            .unwrap();
    }

    fn submit(
        &self,
        request: &DecisionRequest,
        answer: &str,
        token: &str,
        caller: DecisionSubmitCaller,
    ) -> Result<DecisionRequest, orbit_core::OrbitError> {
        self.submit_with_sender(request, answer, token, caller, Some(SENDER))
    }

    fn submit_with_sender(
        &self,
        request: &DecisionRequest,
        answer: &str,
        token: &str,
        caller: DecisionSubmitCaller,
        sender: Option<&str>,
    ) -> Result<DecisionRequest, orbit_core::OrbitError> {
        self.runtime
            .submit_decision_via_channel(DecisionSubmitParams {
                channel: CHANNEL.to_string(),
                request_id: request.id.clone(),
                answer_id: answer.to_string(),
                note: None,
                token: token.to_string(),
                sender: sender.map(str::to_string),
                auth_results: Some(AUTH_PASS.to_string()),
                raw_message: None,
                caller,
            })
    }

    fn plugin_caller() -> DecisionSubmitCaller {
        DecisionSubmitCaller::PluginCallback {
            plugin: PLUGIN.to_string(),
        }
    }

    /// A task left in `proposed`, as `orbit.task.add` files it.
    fn proposed_task(&self, title: &str) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": title,
                    "description": "A proposed task awaiting approval.",
                    "acceptance_criteria": ["It is decided."],
                    "complexity": "low",
                    "type": "chore",
                    "workspace": self.repo.to_string_lossy(),
                    "model": "codex"
                }),
            )
            .unwrap();
        let id = task["id"].as_str().unwrap().to_string();
        assert_eq!(self.status(&id), TaskStatus::Proposed);
        id
    }

    /// Declares an existing workspace file as the task's footprint.
    fn prepare_footprint(&self, task: &str) {
        std::fs::write(self.repo.join("README.md"), "# fixture\n").unwrap();
        self.update(json!({"id": task, "context_files": ["file:README.md"]}));
    }

    /// Records an applied task-pilot assessment holding the task as a
    /// duplicate, in the receipt-comment form pilot admission reads.
    fn seed_pilot_duplicate_hold(&self, task: &str) {
        use orbit_types::task::TaskComment;

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

    fn digest(
        &self,
        caller: DecisionSubmitCaller,
    ) -> Result<orbit_core::application::task::DecisionDigest, orbit_core::OrbitError> {
        self.runtime.decision_digest(CHANNEL, caller)
    }

    fn requests(&self, task: &str) -> Vec<DecisionRequest> {
        self.runtime.get_task_decision_requests(task).unwrap()
    }

    fn requested_entries(&self, task: &str) -> usize {
        self.runtime
            .get_task_history(task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == orbit_types::task::DECISION_REQUESTED_EVENT)
            .count()
    }

    /// Records a candidate with `head` held against `task`, as an owner-local
    /// run does when its candidate waits for review.
    fn hold_candidate(&self, task: &str, head: &str) {
        use orbit_types::task::{TaskDecision, TaskHistoryEntry};
        use orbit_types::workflow::{CANDIDATE_HELD_EVENT, HeldCandidate};

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
        self.runtime
            .apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    append_history: vec![TaskHistoryEntry {
                        at: chrono::Utc::now(),
                        by: "system".to_string(),
                        event: CANDIDATE_HELD_EVENT.to_string(),
                        note: Some(held.text()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: Some(TaskDecision::CandidateHeld(held)),
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
    }
}

#[test]
fn a_registered_channel_applies_an_answer_as_the_human_with_provenance() {
    let fx = fixture();
    fx.register();
    let channels = fx.runtime.list_decision_channels().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].epoch, 1);
    assert!(!channels[0].revoked);
    assert_eq!(channels[0].human, HUMAN);
    assert_eq!(channels[0].plugin, PLUGIN);
    // The key is mirrored to the bound plugin's secret store at
    // registration, without the store file ever naming it in listings.
    let secrets = std::fs::read_to_string(
        fx.global
            .join("state/plugin-secrets")
            .join(format!("{PLUGIN}.json")),
    )
    .unwrap();
    assert!(secrets.contains("channel_key"), "{secrets}");

    let (task, request) = fx.blocked_task_with_request();
    assert_eq!(request.state, DecisionRequestState::Open);
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();
    assert_eq!(token.len(), 26, "{token}");

    let answered = fx
        .submit(&request, "1", &token, Fixture::plugin_caller())
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    let resolution = answered.resolution.clone().unwrap();
    assert_eq!(resolution.answered_by, HUMAN);
    assert_eq!(resolution.answer_id, "1");
    let provenance = resolution.provenance.unwrap();
    assert!(
        provenance.starts_with("decision-channel email (request ")
            && provenance.contains(", token sha256:"),
        "{provenance}"
    );
    assert_eq!(fx.status(&task), TaskStatus::Backlog, "the answer applied");
    let history = fx.runtime.get_task_history(&task).unwrap();
    let answered_entry = history
        .iter()
        .find(|entry| entry.event == orbit_types::task::DECISION_ANSWERED_EVENT)
        .expect("the applied answer is on the task history");
    assert_eq!(answered_entry.by, HUMAN, "written as the human");

    // The token is single-use: presenting it again is refused and the
    // original answer stands.
    let comments = fx.comments(&task);
    let reused = fx
        .submit(&request, "dismiss", &token, Fixture::plugin_caller())
        .expect_err("a reused token is refused");
    assert!(reused.to_string().contains("token_consumed"), "{reused}");
    assert_eq!(fx.status(&task), TaskStatus::Backlog);
    assert_eq!(fx.comments(&task), comments, "the replay wrote no comment");
}

#[test]
fn every_refusal_applies_nothing() {
    let fx = fixture();
    fx.register();

    // One fresh request is refused several unauthenticated and
    // parse-level ways, then still accepts the real answer — proving
    // none of those refusals consumed the token or moved the task.
    let (task, request) = fx.blocked_task_with_request();
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();
    type SubmitResult = Result<DecisionRequest, orbit_core::OrbitError>;
    let cases: Vec<(String, Box<dyn Fn() -> SubmitResult>)> = vec![
        (
            "caller_not_allowed".into(),
            Box::new(|| fx.submit(&request, "1", &token, DecisionSubmitCaller::Agent)),
        ),
        (
            "caller_not_allowed".into(),
            Box::new(|| fx.submit(&request, "1", &token, DecisionSubmitCaller::Unknown)),
        ),
        (
            "channel_not_registered".into(),
            Box::new(|| {
                fx.submit(
                    &request,
                    "1",
                    &token,
                    DecisionSubmitCaller::PluginCallback {
                        plugin: "other-plugin".into(),
                    },
                )
            }),
        ),
        (
            "sender_not_allowed".into(),
            Box::new(|| {
                fx.runtime
                    .submit_decision_via_channel(DecisionSubmitParams {
                        channel: CHANNEL.into(),
                        request_id: request.id.clone(),
                        answer_id: "1".into(),
                        note: None,
                        token: token.clone(),
                        sender: Some("forged@example.net".into()),
                        auth_results: Some(AUTH_PASS.into()),
                        raw_message: None,
                        caller: Fixture::plugin_caller(),
                    })
            }),
        ),
        (
            "sender_not_allowed".into(),
            Box::new(|| {
                fx.submit_with_sender(
                    &request,
                    "1",
                    &token,
                    DecisionSubmitCaller::Operator,
                    Some("forged@example.net"),
                )
            }),
        ),
        (
            "authentication_failed".into(),
            Box::new(|| {
                fx.runtime
                    .submit_decision_via_channel(DecisionSubmitParams {
                        channel: CHANNEL.into(),
                        request_id: request.id.clone(),
                        answer_id: "1".into(),
                        note: None,
                        token: token.clone(),
                        sender: Some(SENDER.into()),
                        auth_results: Some(
                            "mx.example.com; dkim=pass; spf=pass; dmarc=fail".into(),
                        ),
                        raw_message: None,
                        caller: Fixture::plugin_caller(),
                    })
            }),
        ),
        (
            "authentication_failed".into(),
            Box::new(|| {
                fx.runtime
                    .submit_decision_via_channel(DecisionSubmitParams {
                        channel: CHANNEL.into(),
                        request_id: request.id.clone(),
                        answer_id: "1".into(),
                        note: None,
                        token: token.clone(),
                        sender: Some(SENDER.into()),
                        auth_results: None,
                        raw_message: None,
                        caller: Fixture::plugin_caller(),
                    })
            }),
        ),
        (
            "sender_not_allowed".into(),
            Box::new(|| {
                fx.runtime
                    .submit_decision_via_channel(DecisionSubmitParams {
                        channel: CHANNEL.into(),
                        request_id: request.id.clone(),
                        answer_id: "1".into(),
                        note: None,
                        token: token.clone(),
                        sender: None,
                        auth_results: Some(AUTH_PASS.into()),
                        raw_message: None,
                        caller: Fixture::plugin_caller(),
                    })
            }),
        ),
        (
            "token_invalid".into(),
            Box::new(|| fx.submit(&request, "1", &"A".repeat(26), Fixture::plugin_caller())),
        ),
        (
            "token_invalid".into(),
            Box::new(|| fx.submit(&request, "1", "not-a-token", Fixture::plugin_caller())),
        ),
        (
            "answer_not_allowed".into(),
            Box::new(|| fx.submit(&request, "bogus", &token, Fixture::plugin_caller())),
        ),
        (
            "answer_not_allowed".into(),
            Box::new(|| fx.submit(&request, "", &token, Fixture::plugin_caller())),
        ),
    ];
    let comments = fx.comments(&task);
    for (index, (want, submit)) in cases.iter().enumerate() {
        let error = submit().expect_err("each case is refused");
        assert!(
            error.to_string().contains(want.as_str()),
            "case {index}: want {want}, got {error}"
        );
        assert_eq!(fx.status(&task), TaskStatus::Blocked, "case {index}");
        assert_eq!(fx.comments(&task), comments, "case {index} wrote a comment");
        assert_eq!(
            fx.runtime.get_task_decision_requests(&task).unwrap()[0].state,
            DecisionRequestState::Open,
            "case {index} closed the request"
        );
    }
    // An unknown channel and an unknown request are refused by name too.
    let unknown_channel = fx
        .runtime
        .submit_decision_via_channel(DecisionSubmitParams {
            channel: "pager".into(),
            request_id: request.id.clone(),
            answer_id: "1".into(),
            note: None,
            token: token.clone(),
            sender: Some(SENDER.into()),
            auth_results: Some(AUTH_PASS.into()),
            raw_message: None,
            caller: Fixture::plugin_caller(),
        })
        .expect_err("an unregistered channel is refused");
    assert!(
        unknown_channel
            .to_string()
            .contains("channel_not_registered"),
        "{unknown_channel}"
    );
    let unknown_request = fx
        .runtime
        .submit_decision_via_channel(DecisionSubmitParams {
            channel: CHANNEL.into(),
            request_id: "dr-does-not-exist".into(),
            answer_id: "1".into(),
            note: None,
            token: token.clone(),
            sender: Some(SENDER.into()),
            auth_results: Some(AUTH_PASS.into()),
            raw_message: None,
            caller: Fixture::plugin_caller(),
        })
        .expect_err("an unknown request is refused");
    assert!(
        unknown_request.to_string().contains("request_not_found"),
        "{unknown_request}"
    );
    // None of it burned the credential (lowercase presentation included).
    let answered = fx
        .submit(
            &request,
            "dismiss",
            &token.to_ascii_lowercase(),
            Fixture::plugin_caller(),
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(
        fx.status(&task),
        TaskStatus::Blocked,
        "dismiss stays blocked"
    );

    // Expired: the token dies with its request, and the authenticated
    // refusal consumes it.
    let (expired_task, expired) = fx
        .blocked_task_with_request_expiring(Some(chrono::Utc::now() - chrono::Duration::hours(1)));
    assert_eq!(expired.state, DecisionRequestState::Expired);
    let expired_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &expired.id)
        .unwrap();
    let expired_error = fx
        .submit(&expired, "1", &expired_token, Fixture::plugin_caller())
        .expect_err("an expired token is refused");
    assert!(
        expired_error.to_string().contains("request_expired"),
        "{expired_error}"
    );
    assert_eq!(fx.status(&expired_task), TaskStatus::Blocked);
    let replay = fx
        .submit(&expired, "1", &expired_token, Fixture::plugin_caller())
        .expect_err("a consumed token stays consumed");
    assert!(replay.to_string().contains("token_consumed"), "{replay}");

    // Superseded: a human answered on another surface first.
    let (superseded_task, superseded) = fx.blocked_task_with_request();
    let superseded_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &superseded.id)
        .unwrap();
    fx.human()
        .answer_decision_request_as_human(
            &superseded.id,
            "dismiss",
            None,
            HUMAN.to_string(),
            Some("dashboard".into()),
        )
        .unwrap();
    let comments = fx.comments(&superseded_task);
    let superseded_error = fx
        .submit(
            &superseded,
            "1",
            &superseded_token,
            Fixture::plugin_caller(),
        )
        .expect_err("a superseded request is refused");
    assert!(
        superseded_error.to_string().contains("request_superseded"),
        "{superseded_error}"
    );
    assert_eq!(fx.comments(&superseded_task), comments);

    // Rotation invalidates every outstanding token (§4): one minted
    // under the previous epoch is refused even though unexpired, and a
    // fresh token under the new epoch applies.
    let (rotated_task, rotated) = fx.blocked_task_with_request();
    let old_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &rotated.id)
        .unwrap();
    let info = fx.runtime.rotate_decision_channel_key(CHANNEL).unwrap();
    assert_eq!(info.epoch, 2);
    let rotated_error = fx
        .submit(&rotated, "1", &old_token, Fixture::plugin_caller())
        .expect_err("a previous-epoch token is refused");
    assert!(
        rotated_error.to_string().contains("token_consumed"),
        "{rotated_error}"
    );
    assert_eq!(fx.status(&rotated_task), TaskStatus::Blocked);
    let fresh_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &rotated.id)
        .unwrap();
    assert_ne!(fresh_token, old_token);
    fx.submit(&rotated, "1", &fresh_token, Fixture::plugin_caller())
        .unwrap();
    assert_eq!(fx.status(&rotated_task), TaskStatus::Backlog);

    // Revocation is immediate.
    let (revoked_task, revoked) = fx.blocked_task_with_request();
    let revoked_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &revoked.id)
        .unwrap();
    fx.runtime.revoke_decision_channel(CHANNEL).unwrap();
    let comments = fx.comments(&revoked_task);
    let revoked_error = fx
        .submit(&revoked, "1", &revoked_token, Fixture::plugin_caller())
        .expect_err("a revoked channel is refused");
    assert!(
        revoked_error.to_string().contains("channel_revoked"),
        "{revoked_error}"
    );
    assert_eq!(fx.status(&revoked_task), TaskStatus::Blocked);
    assert_eq!(fx.comments(&revoked_task), comments);
}

/// The token contract a transport plugin implements from its mirrored
/// `channel_key` secret alone: HMAC-SHA256 over
/// `request|task|epoch|expires_unix`, base32, 26 chars. Recomputed here
/// independently of core's mint (group-wise base32 over 5-byte blocks)
/// and submitted as the plugin would present it.
fn plugin_minted_token(key_hex: &str, epoch: u64, request: &DecisionRequest) -> String {
    use sha2::{Digest, Sha256};
    let key: Vec<u8> = (0..key_hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&key_hex[index..index + 2], 16).unwrap())
        .collect();
    let message = format!(
        "{}|{}|{}|{}",
        request.id,
        request.task_id,
        epoch,
        request.expires_at.timestamp()
    );
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
    let vector_request = DecisionRequest {
        id: "dr-test".into(),
        task_id: "ORB-1".into(),
        gate: DecisionGate::BlockTriage,
        answers: vec![],
        binding: DecisionBinding::BlockEpisode {
            episode: "vector".into(),
        },
        raised_by: "vector".into(),
        raised_at: chrono::DateTime::from_timestamp(1_893_456_000, 0).unwrap(),
        expires_at: chrono::DateTime::from_timestamp(1_893_456_000, 0).unwrap(),
        state: DecisionRequestState::Open,
        resolution: None,
    };
    assert_eq!(
        plugin_minted_token(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            1,
            &vector_request
        ),
        "X4SKRKLQ5ZXQZIYYPW4VGNZQ2W"
    );

    let fx = fixture();
    fx.register();
    let secrets: Value = serde_json::from_str(
        &std::fs::read_to_string(
            fx.global
                .join("state/plugin-secrets")
                .join(format!("{PLUGIN}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    let key_hex = secrets["secrets"]["channel_key"]["value"]
        .as_str()
        .expect("registration mirrored the channel key");
    let (task, request) = fx.blocked_task_with_request();
    let plugin_token = plugin_minted_token(key_hex, 1, &request);
    assert_eq!(
        plugin_token,
        fx.runtime
            .mint_decision_token(CHANNEL, &request.id)
            .unwrap(),
        "core and the plugin-side contract mint the same token"
    );
    let answered = fx
        .submit(&request, "1", &plugin_token, Fixture::plugin_caller())
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(fx.status(&task), TaskStatus::Backlog);
}

#[test]
fn an_operator_caller_may_test_a_channel() {
    let fx = fixture();
    fx.register();
    let (task, request) = fx.blocked_task_with_request();
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();
    let answered = fx
        .submit_with_sender(
            &request,
            "dismiss",
            &token,
            DecisionSubmitCaller::Operator,
            None,
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.unwrap().answered_by, HUMAN);
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
}

#[test]
fn an_empty_plugin_sender_is_refused_with_or_without_an_allowlist() {
    for allowed_senders in [vec![], vec![SENDER.to_string()]] {
        let fx = fixture();
        fx.register_with_senders(allowed_senders);
        let (task, request) = fx.blocked_task_with_request();
        let token = fx
            .runtime
            .mint_decision_token(CHANNEL, &request.id)
            .unwrap();
        let comments = fx.comments(&task);

        for sender in ["", " \t "] {
            let error = fx
                .submit_with_sender(
                    &request,
                    "1",
                    &token,
                    Fixture::plugin_caller(),
                    Some(sender),
                )
                .expect_err("an empty sender is absent transport evidence");
            assert!(error.to_string().contains("sender_not_allowed"), "{error}");
            assert_eq!(fx.status(&task), TaskStatus::Blocked);
            assert_eq!(fx.comments(&task), comments, "the refusal wrote a comment");
            assert_eq!(
                fx.runtime.get_task_decision_requests(&task).unwrap()[0].state,
                DecisionRequestState::Open,
                "the refusal closed the request"
            );
        }
    }
}

#[test]
fn registration_refuses_an_automation_human_and_a_duplicate() {
    let fx = fixture();
    let automation = fx
        .runtime
        .register_decision_channel("robot", "task-pilot", PLUGIN, vec![])
        .expect_err("a channel may not speak for automation");
    assert!(
        automation.to_string().contains("automation actor"),
        "{automation}"
    );
    fx.register();
    let duplicate = fx
        .runtime
        .register_decision_channel(CHANNEL, HUMAN, PLUGIN, vec![])
        .expect_err("a live channel cannot be re-registered");
    assert!(
        duplicate.to_string().contains("already registered"),
        "{duplicate}"
    );
    // The refused duplicate generated no new key: the original channel
    // still verifies at epoch 1.
    assert_eq!(fx.runtime.list_decision_channels().unwrap()[0].epoch, 1);
}

#[test]
fn submission_is_refused_while_the_decision_channel_is_disabled() {
    let fx = fixture_with_config("");
    fx.register();
    let (task, request) = fx.blocked_task_with_request();
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();
    let disabled = fx
        .submit(&request, "1", &token, Fixture::plugin_caller())
        .expect_err("a disabled channel refuses submission");
    assert!(
        disabled.to_string().contains("decision_channel_disabled"),
        "{disabled}"
    );
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
}

#[test]
fn the_digest_raises_one_proposed_approval_per_proposed_task_idempotently() {
    let fx = fixture();
    fx.register();
    let one = fx.proposed_task("First proposal");
    let two = fx.proposed_task("Second proposal");
    // An unrelated open request expiring within the 24-hour digest
    // window rides along in the same call.
    let (_blocked, expiring) = fx
        .blocked_task_with_request_expiring(Some(chrono::Utc::now() + chrono::Duration::hours(1)));

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    assert_eq!(digest.raised.len(), 2, "{:?}", digest.raised);
    let mut raised_tasks: Vec<&str> = digest
        .raised
        .iter()
        .map(|request| request.task_id.as_str())
        .collect();
    raised_tasks.sort_unstable();
    assert_eq!(raised_tasks, {
        let mut both = vec![one.as_str(), two.as_str()];
        both.sort_unstable();
        both
    });
    for request in &digest.raised {
        assert_eq!(request.gate, DecisionGate::ProposedApproval);
        assert_eq!(request.raised_by, "digest");
        assert_eq!(request.state, DecisionRequestState::Open);
        assert_eq!(
            request
                .answers
                .iter()
                .map(|answer| answer.id.as_str())
                .collect::<Vec<_>>(),
            ["approve", "reject"]
        );
    }
    assert!(
        digest
            .expiring
            .iter()
            .any(|request| request.id == expiring.id),
        "the request expiring within the window is returned: {:?}",
        digest.expiring
    );
    for expected in digest.raised.iter().chain([&expiring]) {
        assert!(
            digest
                .requests
                .iter()
                .any(|request| request.id == expected.id),
            "the digest carries {}",
            expected.id
        );
    }

    // A second call raises nothing new and returns the same ids.
    let again = fx.digest(Fixture::plugin_caller()).unwrap();
    assert!(again.raised.is_empty(), "{:?}", again.raised);
    let ids = |requests: &[DecisionRequest]| -> Vec<String> {
        let mut ids: Vec<String> = requests.iter().map(|request| request.id.clone()).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(ids(&again.requests), ids(&digest.requests));
    assert_eq!(fx.requested_entries(&one), 1);
    assert_eq!(fx.requested_entries(&two), 1);
}

#[test]
fn a_changed_proposal_supersedes_its_request_and_the_digest_raises_a_fresh_one() {
    let fx = fixture();
    fx.register();
    let task = fx.proposed_task("Mutable proposal");
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let first = digest.raised[0].clone();

    // Changing the criteria the digest rendered supersedes the request.
    fx.update(json!({
        "id": task,
        "acceptance_criteria": ["It is decided differently."],
    }));
    let requests = fx.requests(&task);
    assert_eq!(
        requests
            .iter()
            .find(|request| request.id == first.id)
            .unwrap()
            .state,
        DecisionRequestState::Superseded
    );

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
    assert_eq!(
        fx.requests(&task)
            .into_iter()
            .find(|request| request.id == second.id)
            .unwrap()
            .state,
        DecisionRequestState::Superseded
    );
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    assert_eq!(digest.raised.len(), 1);
    assert_eq!(fx.status(&task), TaskStatus::Proposed);
}

/// A superseded request stays superseded: restoring the text it was raised
/// against, or returning to its status, never reopens it.
#[test]
fn a_superseded_proposal_request_stays_superseded_after_the_task_is_restored() {
    let fx = fixture();
    fx.register();
    let task = fx.proposed_task("Round-trip proposal");
    let first = fx.digest(Fixture::plugin_caller()).unwrap().raised[0].clone();
    let state_of = |id: &str| {
        fx.requests(&task)
            .into_iter()
            .find(|request| request.id == id)
            .unwrap()
            .state
    };

    // Edit the criteria and put them back.
    fx.update(json!({"id": task, "acceptance_criteria": ["It is decided differently."]}));
    assert_eq!(state_of(&first.id), DecisionRequestState::Superseded);
    fx.update(json!({"id": task, "acceptance_criteria": ["It is decided."]}));
    assert_eq!(state_of(&first.id), DecisionRequestState::Superseded);
    assert!(
        fx.requests(&task)
            .iter()
            .all(|request| request.state != DecisionRequestState::Open)
    );

    // Edit the description and put it back.
    fx.update(json!({"id": task, "description": "A rewritten proposal."}));
    fx.update(json!({"id": task, "description": "A proposed task awaiting approval."}));
    assert_eq!(state_of(&first.id), DecisionRequestState::Superseded);

    // The next digest raises a fresh request; a fresh request stays open and
    // idempotent while nothing changes.
    let second = fx.digest(Fixture::plugin_caller()).unwrap().raised[0].clone();
    assert_ne!(second.id, first.id);
    assert_eq!(state_of(&second.id), DecisionRequestState::Open);
    assert_eq!(state_of(&first.id), DecisionRequestState::Superseded);
    assert!(
        fx.digest(Fixture::plugin_caller())
            .unwrap()
            .raised
            .is_empty()
    );

    // Leave `proposed` and come back.
    fx.update(json!({"id": task, "status": "backlog"}));
    assert_eq!(state_of(&second.id), DecisionRequestState::Superseded);
    fx.update(json!({"id": task, "status": "proposed"}));
    assert_eq!(fx.status(&task), TaskStatus::Proposed);
    assert_eq!(state_of(&second.id), DecisionRequestState::Superseded);
    assert_eq!(state_of(&first.id), DecisionRequestState::Superseded);
    let third = fx.digest(Fixture::plugin_caller()).unwrap().raised[0].clone();
    assert_ne!(third.id, second.id);
    assert_eq!(state_of(&third.id), DecisionRequestState::Open);
}

#[test]
fn a_review_approval_binds_to_the_candidate_and_supersedes_on_replacement() {
    let fx = fixture();
    fx.register();
    let task = fx.task();
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.update(json!({"id": task, "status": "review"}));
    assert_eq!(fx.status(&task), TaskStatus::Review);
    let fx_description = fx.runtime.get_task(&task).unwrap().description;

    fx.hold_candidate(&task, "candidate-a");

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let first = digest
        .raised
        .iter()
        .find(|request| request.task_id == task)
        .expect("the digest raises a review approval")
        .clone();
    assert_eq!(first.gate, DecisionGate::ReviewApproval);
    assert_eq!(first.state, DecisionRequestState::Open);
    let DecisionBinding::TaskRevision {
        candidate, status, ..
    } = &first.binding
    else {
        panic!(
            "a review approval binds to a task revision: {:?}",
            first.binding
        );
    };
    assert_eq!(status, &TaskStatus::Review);
    assert_eq!(candidate.as_deref(), Some("candidate-a"));

    // A new candidate replaces the reviewed one: superseded, then the
    // next digest raises a fresh request bound to the new candidate.
    fx.hold_candidate(&task, "candidate-b");
    assert_eq!(
        fx.requests(&task)
            .into_iter()
            .find(|request| request.id == first.id)
            .unwrap()
            .state,
        DecisionRequestState::Superseded
    );
    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let second = digest
        .raised
        .iter()
        .find(|request| request.task_id == task)
        .expect("the digest re-raises for the new candidate")
        .clone();
    assert_ne!(second.id, first.id);
    assert_eq!(second.state, DecisionRequestState::Open);
    let DecisionBinding::TaskRevision { candidate, .. } = &second.binding else {
        panic!(
            "a review approval binds to a task revision: {:?}",
            second.binding
        );
    };
    assert_eq!(candidate.as_deref(), Some("candidate-b"));

    // Holding the first candidate again does not revive its request.
    fx.hold_candidate(&task, "candidate-a");
    let states: Vec<_> = fx
        .requests(&task)
        .into_iter()
        .map(|request| (request.id, request.state))
        .collect();
    assert!(states.contains(&(first.id.clone(), DecisionRequestState::Superseded)));
    assert!(states.contains(&(second.id.clone(), DecisionRequestState::Superseded)));

    // A spec round trip on a review task supersedes its request for good.
    let third = fx
        .digest(Fixture::plugin_caller())
        .unwrap()
        .raised
        .into_iter()
        .find(|request| request.task_id == task)
        .expect("the digest raises a fresh request for the restored candidate");
    assert_eq!(third.state, DecisionRequestState::Open);
    fx.update(json!({"id": task, "description": "Review text changed."}));
    fx.update(json!({"id": task, "description": fx_description}));
    assert_eq!(
        fx.requests(&task)
            .into_iter()
            .find(|request| request.id == third.id)
            .unwrap()
            .state,
        DecisionRequestState::Superseded
    );
}

/// A task that held a candidate, returned to `backlog` and was proposed again
/// still carries that candidate in its history. Its proposal approval is
/// raised without a candidate, so the historical candidate must not supersede
/// it: repeated digests keep answering the one open request.
#[test]
fn a_reproposed_task_with_a_historical_candidate_keeps_one_open_proposal() {
    let fx = fixture();
    fx.register();
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
    let DecisionBinding::TaskRevision {
        candidate, status, ..
    } = &proposal.binding
    else {
        panic!(
            "a proposal approval binds to a task revision: {:?}",
            proposal.binding
        );
    };
    assert_eq!(status, &TaskStatus::Proposed);
    assert_eq!(candidate, &None);
    let open_requests = |fx: &Fixture| -> Vec<String> {
        fx.requests(&task)
            .into_iter()
            .filter(|request| request.state == DecisionRequestState::Open)
            .map(|request| request.id)
            .collect()
    };
    assert_eq!(open_requests(&fx), vec![proposal.id.clone()]);

    // The unchanged proposal raises nothing new and stays the one open request.
    let again = fx.digest(Fixture::plugin_caller()).unwrap();
    assert!(
        again.raised.iter().all(|request| request.task_id != task),
        "{:?}",
        again.raised
    );
    assert_eq!(open_requests(&fx), vec![proposal.id.clone()]);
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
    let fx = fixture();
    fx.register();
    let task = fx.proposed_task("Guarded proposal");

    for (caller, code) in [
        (DecisionSubmitCaller::Agent, "caller_not_allowed"),
        (DecisionSubmitCaller::Unknown, "caller_not_allowed"),
        (
            DecisionSubmitCaller::PluginCallback {
                plugin: "other-plugin".to_string(),
            },
            "channel_not_registered",
        ),
    ] {
        let error = fx
            .digest(caller)
            .expect_err("the digest refuses this caller");
        assert!(error.to_string().contains(code), "{error}");
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
    let fx = fixture();
    fx.register();
    let decided = fx.proposed_task("Decided by channel");
    // A channel approval is screened: only a prepared footprint (or the
    // no-diff disposition) is admitted, so prepare one here.
    fx.prepare_footprint(&decided);
    let rejected = fx.proposed_task("Rejected by channel");
    let direct = fx.proposed_task("Decided directly");

    let digest = fx.digest(Fixture::plugin_caller()).unwrap();
    let request_for = |task: &str| {
        digest
            .raised
            .iter()
            .find(|request| request.task_id == task)
            .unwrap()
            .clone()
    };
    let approve_request = request_for(&decided);
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &approve_request.id)
        .unwrap();
    let answered = fx
        .submit(
            &approve_request,
            "approve",
            &token,
            Fixture::plugin_caller(),
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.clone().unwrap().answered_by, HUMAN);

    // The screened approval a CLI approve runs lands the same way.
    fx.runtime
        .approve_task_screened(&direct, None, None, None, None)
        .unwrap();
    assert_eq!(fx.status(&decided), fx.status(&direct));
    assert_eq!(fx.status(&decided), TaskStatus::Backlog);

    let reject_request = request_for(&rejected);
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &reject_request.id)
        .unwrap();
    fx.submit(&reject_request, "reject", &token, Fixture::plugin_caller())
        .unwrap();
    assert_eq!(fx.status(&rejected), TaskStatus::Rejected);
}

#[test]
fn a_digest_approve_is_refused_while_the_pilot_holds_or_no_footprint_is_prepared() {
    let fx = fixture();
    fx.register();
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
    let request_for = |task: &str| {
        digest
            .raised
            .iter()
            .find(|request| request.task_id == task)
            .unwrap()
            .clone()
    };
    let open_request_for = |task: &str| {
        fx.requests(task)
            .into_iter()
            .find(|request| {
                request.gate == DecisionGate::ProposedApproval
                    && request.state == DecisionRequestState::Open
            })
            .unwrap()
    };

    // The pilot-held proposal is refused twice with the hold refusal:
    // the first refusal neither answered the request nor consumed the
    // token, and the task stays proposed.
    let held_request = request_for(&held);
    let held_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &held_request.id)
        .unwrap();
    for _ in 0..2 {
        let refused = fx
            .submit(
                &held_request,
                "approve",
                &held_token,
                Fixture::plugin_caller(),
            )
            .expect_err("a pilot-held proposal is not approved through the channel");
        let refused = refused.to_string();
        assert!(refused.contains("pilot_duplicate"), "{refused}");
        assert!(!refused.contains("token_consumed"), "{refused}");
        assert_eq!(fx.status(&held), TaskStatus::Proposed);
        let request = open_request_for(&held);
        assert_eq!(request.id, held_request.id);
        assert!(request.resolution.is_none(), "{request:?}");
    }

    // Without a prepared footprint the approval is refused and applies
    // nothing; once the footprint is declared, the same public path
    // approves it as the human.
    let unprepared_request = request_for(&unprepared);
    let unprepared_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &unprepared_request.id)
        .unwrap();
    let refused = fx
        .submit(
            &unprepared_request,
            "approve",
            &unprepared_token,
            Fixture::plugin_caller(),
        )
        .expect_err("an empty-footprint proposal is not approved through the channel");
    assert!(
        refused.to_string().contains("missing_context_files"),
        "{refused}"
    );
    assert_eq!(fx.status(&unprepared), TaskStatus::Proposed);
    assert!(open_request_for(&unprepared).resolution.is_none());

    fx.prepare_footprint(&unprepared);
    fx.digest(Fixture::plugin_caller()).unwrap();
    let prepared_request = open_request_for(&unprepared);
    let prepared_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &prepared_request.id)
        .unwrap();
    let answered = fx
        .submit(
            &prepared_request,
            "approve",
            &prepared_token,
            Fixture::plugin_caller(),
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.clone().unwrap().answered_by, HUMAN);
    assert_eq!(fx.status(&unprepared), TaskStatus::Backlog);
    assert!(
        !fx.runtime
            .get_task(&unprepared)
            .unwrap()
            .context_files
            .is_empty(),
        "an admitted proposal carries its prepared footprint"
    );

    // The no-diff disposition needs no footprint.
    let no_diff_request = request_for(&no_diff);
    let no_diff_token = fx
        .runtime
        .mint_decision_token(CHANNEL, &no_diff_request.id)
        .unwrap();
    let answered = fx
        .submit(
            &no_diff_request,
            "approve",
            &no_diff_token,
            Fixture::plugin_caller(),
        )
        .unwrap();
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
        use base64::Engine as _;
        use rsa::RsaPrivateKey;
        use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
        let key = RsaPrivateKey::from_pkcs8_pem(DKIM_TEST_PRIVATE_KEY_PEM)
            .map_err(|error| error.to_string())?;
        let der = key
            .to_public_key()
            .to_public_key_der()
            .map_err(|error| error.to_string())?;
        Ok(vec![format!(
            "v=DKIM1; k=rsa; p={}",
            base64::engine::general_purpose::STANDARD.encode(der.as_bytes())
        )])
    }
}

struct FailingResolver;

impl DkimTxtResolver for FailingResolver {
    fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String> {
        Err(format!("DNS TXT lookup for `{name}` timed out after 5s"))
    }
}

/// Sign a fixed message with the fixture key under relaxed/relaxed
/// canonicalisation, implemented here directly so the test does not reuse
/// core's canonicaliser.
fn dkim_signed_message(body: &str) -> String {
    use base64::Engine as _;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use sha2::{Digest, Sha256};

    let b64 = base64::engine::general_purpose::STANDARD;
    let canonical_body = format!("{}\r\n", body.trim_end_matches(['\r', '\n']));
    let bh = b64.encode(Sha256::digest(canonical_body.as_bytes()));
    let dkim_value = format!(
        "v=1; a=rsa-sha256; c=relaxed/relaxed; d=example.com; s=test1; h=from:to:subject:date; bh={bh}; b="
    );
    let mut header_data = String::new();
    for (name, value) in [
        ("from", "Operator <operator@example.com>"),
        ("to", "orbit@example.com"),
        ("subject", "Re: decision"),
        ("date", "Mon, 26 Oct 2026 10:00:00 +0000"),
    ] {
        header_data.push_str(&format!("{name}:{value}\r\n"));
    }
    header_data.push_str(&format!("dkim-signature:{dkim_value}"));
    let key = RsaPrivateKey::from_pkcs8_pem(DKIM_TEST_PRIVATE_KEY_PEM).unwrap();
    let signature: rsa::pkcs1v15::Signature =
        SigningKey::<Sha256>::new(key).sign(header_data.as_bytes());
    format!(
        "DKIM-Signature: {dkim_value}{}\r\nFrom: Operator <operator@example.com>\r\nTo: orbit@example.com\r\nSubject: Re: decision\r\nDate: Mon, 26 Oct 2026 10:00:00 +0000\r\n\r\n{canonical_body}",
        b64.encode(signature.to_bytes())
    )
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
    use base64::Engine as _;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use sha2::{Digest, Sha256};

    let b64 = base64::engine::general_purpose::STANDARD;
    let canonical_body = format!("{}\r\n", body.trim_end_matches(['\r', '\n']));
    let bh = b64.encode(Sha256::digest(canonical_body.as_bytes()));
    let signed_value = format!(
        "v=1; a=rsa-sha256; c={canonicalization}; d=example.com; s=test1; h=from:to:subject:date; {signed_b}; bh={bh}"
    );
    let mut header_data = String::new();
    if canonicalization.starts_with("relaxed/") {
        for (name, value) in [
            ("from", "Operator <operator@example.com>"),
            ("to", "orbit@example.com"),
            ("subject", "Re: decision"),
            ("date", "Mon, 26 Oct 2026 10:00:00 +0000"),
        ] {
            header_data.push_str(&format!("{name}:{value}\r\n"));
        }
        header_data.push_str(&format!("dkim-signature:{signed_value}"));
    } else {
        for (name, value) in [
            ("From", "Operator <operator@example.com>"),
            ("To", "orbit@example.com"),
            ("Subject", "Re: decision"),
            ("Date", "Mon, 26 Oct 2026 10:00:00 +0000"),
        ] {
            header_data.push_str(&format!("{name}: {value}\r\n"));
        }
        header_data.push_str(&format!("DKIM-Signature: {signed_value}"));
    }
    let key = RsaPrivateKey::from_pkcs8_pem(DKIM_TEST_PRIVATE_KEY_PEM).unwrap();
    let signature: rsa::pkcs1v15::Signature =
        SigningKey::<Sha256>::new(key).sign(header_data.as_bytes());
    let signature_b64 = b64.encode(signature.to_bytes());
    let presented_value = signed_value.replacen(
        &format!("{signed_b};"),
        &format!("{};", presented_b.replace("{sig}", &signature_b64)),
        1,
    );
    let message = format!(
        "DKIM-Signature: {presented_value}\r\nFrom: Operator <operator@example.com>\r\nTo: orbit@example.com\r\nSubject: Re: decision\r\nDate: Mon, 26 Oct 2026 10:00:00 +0000\r\n\r\n{canonical_body}"
    );
    (message, signature_b64)
}

impl Fixture {
    fn submit_dkim(
        &self,
        request: &DecisionRequest,
        token: &str,
        raw_message: Option<String>,
        resolver: &dyn DkimTxtResolver,
    ) -> Result<DecisionRequest, orbit_core::OrbitError> {
        self.runtime.submit_decision_via_channel_with_resolver(
            DecisionSubmitParams {
                channel: CHANNEL.to_string(),
                request_id: request.id.clone(),
                answer_id: "1".to_string(),
                note: None,
                token: token.to_string(),
                sender: Some(SENDER.to_string()),
                auth_results: Some(AUTH_PASS.to_string()),
                raw_message,
                caller: Fixture::plugin_caller(),
            },
            resolver,
        )
    }
}

#[test]
fn core_dkim_verification_applies_a_signed_message_and_refuses_forgeries() {
    let fx = fixture();
    fx.runtime
        .register_decision_channel_with_dkim_requirement(
            CHANNEL,
            HUMAN,
            PLUGIN,
            vec![SENDER.to_string()],
            false,
        )
        .unwrap();
    let (task, request) = fx.blocked_task_with_request();
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();
    let signed = dkim_signed_message("approve");

    // One body byte changed: the body hash no longer matches.
    let tampered = signed.replacen("approve\r\n", "approv3\r\n", 1);
    let refused = fx
        .submit_dkim(&request, &token, Some(tampered), &FixtureResolver)
        .expect_err("a tampered body is refused");
    assert!(
        refused.to_string().contains("dkim_verification_failed"),
        "{refused}"
    );
    assert_eq!(fx.status(&task), TaskStatus::Blocked);

    // A forged Authentication-Results claiming pass on an unsigned raw
    // message: core's own result contradicts the forwarded verdict.
    let unsigned = "From: Operator <operator@example.com>\r\nTo: orbit@example.com\r\nSubject: Re: decision\r\nDate: Mon, 26 Oct 2026 10:00:00 +0000\r\n\r\napprove\r\n".to_string();
    let refused = fx
        .submit_dkim(&request, &token, Some(unsigned), &FixtureResolver)
        .expect_err("an unsigned raw message is refused");
    assert!(
        refused.to_string().contains("dkim_verification_failed"),
        "{refused}"
    );
    assert_eq!(fx.status(&task), TaskStatus::Blocked);

    // A DNS failure is retryable and consumes nothing.
    let refused = fx
        .submit_dkim(&request, &token, Some(signed.clone()), &FailingResolver)
        .expect_err("a DNS failure is refused");
    let refused = refused.to_string();
    assert!(
        refused.contains("dkim_verification_unavailable"),
        "{refused}"
    );
    assert!(refused.contains("retryable"), "{refused}");
    assert_eq!(fx.status(&task), TaskStatus::Blocked);

    // The signed message verifies through the injected resolver and the
    // answer applies — proving none of the refusals above consumed the
    // token.
    let answered = fx
        .submit_dkim(&request, &token, Some(signed), &FixtureResolver)
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(fx.status(&task), TaskStatus::Backlog);
}

#[test]
fn core_dkim_accepts_legal_folding_around_a_b_value_with_a_following_tag() {
    // RFC 6376 §3.7: emptying `b=` deletes the value and all surrounding
    // whitespace. Each case signs the emptied form independently, then
    // presents the signature folded or spaced, with `bh=` after `b=`.
    let cases: [(&str, &str, &str, &str); 6] = [
        ("unfolded relaxed", "relaxed/relaxed", "b=", "b={sig}"),
        (
            "folded after b= relaxed",
            "relaxed/relaxed",
            "b=",
            "b=\r\n\t{sig}",
        ),
        (
            "whitespace around tag syntax relaxed",
            "relaxed/relaxed",
            "b =",
            "b = \r\n\t{sig} \r\n\t",
        ),
        (
            "folded after b= simple",
            "simple/simple",
            "b=",
            "b=\r\n\t{sig}",
        ),
        (
            "folded after b= simple/relaxed",
            "simple/relaxed",
            "b=",
            "b=\r\n\t{sig}",
        ),
        (
            "folded after b= relaxed/simple",
            "relaxed/simple",
            "b=",
            "b=\r\n\t{sig}",
        ),
    ];
    for (label, canonicalization, signed_b, presented_b) in cases {
        let fx = fixture();
        fx.runtime
            .register_decision_channel_with_dkim_requirement(
                CHANNEL,
                HUMAN,
                PLUGIN,
                vec![SENDER.to_string()],
                true,
            )
            .unwrap();
        let (task, request) = fx.blocked_task_with_request();
        let token = fx
            .runtime
            .mint_decision_token(CHANNEL, &request.id)
            .unwrap();
        let (signed, signature) = dkim_signed_message_with_following_tag(
            "approve",
            canonicalization,
            signed_b,
            presented_b,
        );

        // A changed body byte is refused and applies nothing.
        let tampered = signed.replacen("approve\r\n", "approv3\r\n", 1);
        let refused = fx
            .submit_dkim(&request, &token, Some(tampered), &FixtureResolver)
            .expect_err("a tampered body is refused");
        assert!(
            refused.to_string().contains("dkim_verification_failed"),
            "{label}: {refused}"
        );
        assert_eq!(fx.status(&task), TaskStatus::Blocked, "{label}");

        // A changed signature byte is refused and applies nothing.
        let forged_signature = {
            let mut chars: Vec<char> = signature.chars().collect();
            chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
            chars.into_iter().collect::<String>()
        };
        let forged = signed.replacen(&signature, &forged_signature, 1);
        let refused = fx
            .submit_dkim(&request, &token, Some(forged), &FixtureResolver)
            .expect_err("a forged signature is refused");
        assert!(
            refused.to_string().contains("dkim_verification_failed"),
            "{label}: {refused}"
        );
        assert_eq!(fx.status(&task), TaskStatus::Blocked, "{label}");

        // The valid message verifies and applies the answer, proving the
        // refusals above did not consume the token.
        let answered = fx
            .submit_dkim(&request, &token, Some(signed), &FixtureResolver)
            .unwrap_or_else(|error| panic!("{label}: valid message refused: {error}"));
        assert_eq!(answered.state, DecisionRequestState::Answered, "{label}");
        assert_eq!(fx.status(&task), TaskStatus::Backlog, "{label}");
    }
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
    let (task, request) = fx.blocked_task_with_request();
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();

    let refused = fx
        .submit_dkim(&request, &token, None, &FixtureResolver)
        .expect_err("no raw message is refused when DKIM is required");
    assert!(
        refused.to_string().contains("dkim_verification_required"),
        "{refused}"
    );
    assert_eq!(fx.status(&task), TaskStatus::Blocked);

    let answered = fx
        .submit_dkim(
            &request,
            &token,
            Some(dkim_signed_message("approve")),
            &FixtureResolver,
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(fx.status(&task), TaskStatus::Backlog);
}
