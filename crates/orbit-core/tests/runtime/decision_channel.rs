//! The delegated-human decision channel (design
//! `docs/design/human-decision-channel/2_design.md` §3–§5): a registered
//! test channel submits a human's answer through core with a per-request
//! token, and every refusal in the §5 table applies nothing.

use std::path::PathBuf;

use orbit_core::application::task::{
    DecisionSubmitCaller, DecisionSubmitParams, FinalRecoveryCompletion, FinalRecoveryRequest,
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
        self.runtime
            .register_decision_channel(CHANNEL, HUMAN, PLUGIN, vec![SENDER.to_string()])
            .unwrap();
    }

    fn submit(
        &self,
        request: &DecisionRequest,
        answer: &str,
        token: &str,
        caller: DecisionSubmitCaller,
    ) -> Result<DecisionRequest, orbit_core::OrbitError> {
        self.runtime
            .submit_decision_via_channel(DecisionSubmitParams {
                channel: CHANNEL.to_string(),
                request_id: request.id.clone(),
                answer_id: answer.to_string(),
                note: None,
                token: token.to_string(),
                sender: Some(SENDER.to_string()),
                auth_results: Some(AUTH_PASS.to_string()),
                caller,
            })
    }

    fn plugin_caller() -> DecisionSubmitCaller {
        DecisionSubmitCaller::PluginCallback {
            plugin: PLUGIN.to_string(),
        }
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
                        auth_results: Some(
                            "mx.example.com; dkim=pass; spf=pass; dmarc=fail".into(),
                        ),
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
        .submit(&request, "dismiss", &token, DecisionSubmitCaller::Operator)
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.unwrap().answered_by, HUMAN);
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
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
