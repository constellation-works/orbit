//! Script grant requests (script steps phase 4): a script step refused for a
//! missing, stale or insufficient grant raises one open `script_grant`
//! decision request carrying the script, both digests, the diff and the
//! requested against granted capabilities; a repeat refusal reuses it. Only
//! a human answer (the CLI funnel or a registered decision channel) writes
//! the grant, bound to the reviewed digest; an agent session or plugin
//! callback can neither answer nor grant.

use std::path::PathBuf;

use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{DecisionSubmitCaller, DecisionSubmitParams};
use orbit_core::runtime::scripts::{self, ScriptGrantOptions, ScriptGrantState};
use orbit_engine::{RuntimeHost, ScriptStepRequest, ScriptStepRun};
use orbit_types::task::{DecisionGate, DecisionRequest};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::activity_job::{ScriptGrantRequest, ScriptNetwork, script_refusal};
use serde_json::json;
use tempfile::TempDir;

const SCRIPT: &str = "sweep_report";
const HUMAN: &str = "human:operator";
const CHANNEL: &str = "email";
const PLUGIN: &str = "decision-mail";
const SENDER: &str = "operator@example.com";
const AUTH_PASS: &str = "mx.example.com; dkim=pass header.d=example.com; spf=pass smtp.mailfrom=example.com; dmarc=pass";

struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    runtime: OrbitRuntime,
}

fn fixture() -> Fixture {
    crate::scenarios::require_isolated_mutable_fixture("script_grant_request::fixture");
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit/resources/scripts")).unwrap();
    std::fs::create_dir_all(repo.join("tools")).unwrap();
    std::fs::write(
        repo.join(".orbit/config.toml"),
        "[decision_channel]\nenabled = true\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("sweep.yaml"),
        format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: sweep\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: run\n      target: script:{SCRIPT}\n"
        ),
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let fx = Fixture {
        _root: root,
        repo,
        runtime,
    };
    fx.write_manifest("");
    fx.write_entry("echo '{}'\n");
    fx
}

impl Fixture {
    fn write_manifest(&self, requests: &str) {
        std::fs::write(
            self.repo
                .join(format!(".orbit/resources/scripts/{SCRIPT}.yaml")),
            format!(
                "schemaVersion: 1\nkind: Script\nmetadata:\n  name: {SCRIPT}\nspec:\n  description: Fixture report.\n  entry: tools/report.sh\n{requests}"
            ),
        )
        .unwrap();
    }

    fn write_entry(&self, body: &str) {
        std::fs::write(
            self.repo.join("tools/report.sh"),
            format!("#!/bin/sh\n{body}"),
        )
        .unwrap();
    }

    /// Runs the one-step job; the rendered outcome (refusal or result).
    fn run(&self) -> String {
        format!(
            "{:?}",
            self.runtime
                .run_job_v2_from_yaml(&self.repo.join("sweep.yaml"), json!({}))
        )
    }

    fn open(&self) -> Vec<DecisionRequest> {
        self.runtime.open_script_grant_requests().unwrap()
    }

    fn only_open(&self) -> (DecisionRequest, ScriptGrantRequest) {
        let open = self.open();
        assert_eq!(open.len(), 1, "one open script_grant request: {open:?}");
        let request = open[0].clone();
        assert_eq!(request.gate, DecisionGate::ScriptGrant);
        let payload = request
            .script_grant
            .clone()
            .expect("a script_grant request carries its payload");
        (request, payload)
    }

    fn current_digest(&self) -> String {
        self.runtime
            .script_grant_overview()
            .unwrap()
            .into_iter()
            .find(|script| script.name == SCRIPT)
            .and_then(|script| script.current_digest)
            .expect("the script has a current digest")
    }

    fn answer(&self, request: &DecisionRequest, answer: &str) -> Result<DecisionRequest, String> {
        self.runtime
            .answer_decision_request_as_human(
                &request.id,
                answer,
                None,
                HUMAN.to_string(),
                Some("cli".to_string()),
            )
            .map_err(|error| error.to_string())
    }

    fn tool(&self, tool: &str, input: serde_json::Value, session: ToolSessionContext) -> String {
        match self
            .runtime
            .execute_tool_command_dispatch_with_session_context(
                tool,
                input,
                None,
                None,
                ToolEntryPoint::Mcp,
                session,
            ) {
            Ok(outcome) => panic!("{tool} must be refused, got {:?}", outcome.value),
            Err(error) => error.to_string(),
        }
    }
}

fn agent_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Agent].into(),
        origin_session_id: Some("mcp-session-1".into()),
        ..Default::default()
    }
}

crate::scenario_table!(
    stored_script_grant_timeout_is_rechecked_before_step_supervision,
    stored_script_grant_timeout_checks,
);

fn stored_script_grant_timeout_checks() {
    let bound = u32::MAX as u64;
    for seconds in [bound + 1, u64::MAX, 0, bound] {
        let fx = fixture();
        // A network mismatch gives the valid boundary case a deterministic
        // later gate, without requiring a sandbox on the test host.
        fx.write_manifest(&format!(
            "  requests:\n    timeout_seconds: {}\n    network: https\n",
            seconds.max(1)
        ));
        let mut grant = fx
            .runtime
            .grant_script(
                SCRIPT,
                &ScriptGrantOptions {
                    timeout_seconds: Some(bound),
                    network: Some(ScriptNetwork::None),
                    ..Default::default()
                },
            )
            .unwrap();
        let bytes = fx.runtime.granted_script_bytes(SCRIPT).unwrap().unwrap();
        // Reproduce a legacy grant, including a valid authorization witness:
        // admission now rejects these ceilings, but persisted rows survive.
        grant.limits.timeout_max_seconds = seconds;
        fx.runtime
            .sqlite_store()
            .unwrap()
            .upsert_script_grant(&grant)
            .unwrap();
        scripts::record_script_grant_witness(
            &fx.runtime.global_root(),
            &grant,
            &bytes.manifest,
            &bytes.entry,
        )
        .unwrap();
        scripts::verify_script_grant_witness(&fx.runtime.global_root(), &grant).unwrap();

        let status = fx.runtime.script_status(SCRIPT).unwrap().unwrap();
        if seconds == bound {
            assert_eq!(status.state, ScriptGrantState::Granted, "{}", status.detail);
        } else {
            assert_eq!(
                status.state,
                ScriptGrantState::WitnessRefused,
                "{}",
                status.detail
            );
            assert!(
                status
                    .detail
                    .contains(&format!("timeout ceiling {seconds}")),
                "{}",
                status.detail
            );
            assert!(
                status.detail.contains(&format!("1..={bound}")),
                "{}",
                status.detail
            );
            assert!(
                status
                    .detail
                    .contains(&format!("orbit script grant {SCRIPT}")),
                "{}",
                status.detail
            );
        }

        let expected_code = if seconds == bound {
            assert_eq!(
                fx.runtime.verified_script_grant(SCRIPT).unwrap(),
                Some(grant.clone())
            );
            script_refusal::GRANT_INSUFFICIENT
        } else {
            let error = fx
                .runtime
                .verified_script_grant(SCRIPT)
                .expect_err("a witnessed legacy grant must still satisfy the timeout bound")
                .to_string();
            assert!(error.contains(&grant.grant_id), "{error}");
            assert!(
                error.contains(&format!("timeout ceiling {seconds}")),
                "{error}"
            );
            assert!(error.contains(&format!("1..={bound}")), "{error}");
            assert!(
                error.contains(&format!("orbit script grant {SCRIPT}")),
                "{error}"
            );
            script_refusal::GRANT_MISSING
        };
        let ScriptStepRun::Refused {
            code,
            message,
            record,
        } = RuntimeHost::run_script_step(
            &fx.runtime,
            &ScriptStepRequest {
                run_id: "stored-grant-timeout".to_string(),
                step_id: "run".to_string(),
                script: SCRIPT.to_string(),
                input: json!({}),
                workspace_path: None,
                timeout_seconds: None,
            },
        )
        else {
            panic!("the grant gate must refuse before sandbox supervision");
        };
        assert_eq!(code, expected_code, "{message}");
        assert!(
            record.argv.is_empty() && record.profile.is_none(),
            "{record:?}"
        );
        let outcome = fx.run();
        assert!(outcome.contains(expected_code), "{outcome}");
        if seconds == bound {
            assert!(message.contains("network `https`"), "{message}");
        } else {
            for refusal in [&message, &outcome] {
                assert!(refusal.contains(&grant.grant_id), "{refusal}");
                assert!(
                    refusal.contains(&format!("orbit script grant {SCRIPT}")),
                    "{refusal}"
                );
            }
        }
    }
}

#[test]
fn script_grant_request_scenarios() {
    if !super::dispatch_admission::isolated("script_grant_request::script_grant_request_scenarios")
    {
        return;
    }
    a_missing_grant_raises_one_request_and_a_repeat_refusal_reuses_it();
    a_human_answer_grants_the_reviewed_digest_and_the_step_passes_the_grant_gate();
    a_stale_grant_raises_a_request_with_both_digests_and_the_diff();
    an_answer_after_the_script_changed_is_refused_and_grants_nothing();
    an_insufficient_grant_raises_a_request_naming_requested_and_granted();
    a_requested_timeout_above_the_default_ceiling_is_granted_by_answer_and_dashboard();
    an_unrepresentable_timeout_is_refused_by_answer_and_dashboard_and_grants_nothing();
    a_registered_channel_answer_writes_the_grant_with_channel_provenance();
    agents_and_plugin_callbacks_cannot_answer_or_grant();
}

fn a_missing_grant_raises_one_request_and_a_repeat_refusal_reuses_it() {
    let fx = fixture();
    let refused = fx.run();
    assert!(refused.contains("grant_missing"), "{refused}");
    let (request, payload) = fx.only_open();
    assert!(
        refused.contains(&request.id),
        "the refusal names the request: {refused}"
    );
    assert_eq!(payload.name, SCRIPT);
    assert_eq!(payload.digest, fx.current_digest());
    assert_eq!(payload.refusal, "grant_missing");
    assert_eq!(payload.granted_digest, None);
    assert_eq!(payload.granted, None);
    assert_eq!(payload.requested.network, ScriptNetwork::None);
    assert!(
        payload.diff.contains("+++ tools/report.sh") && payload.diff.contains("+echo '{}'"),
        "a first grant shows every line as added: {}",
        payload.diff
    );

    // The decision tool surface filters by the new gate.
    let listed = fx
        .runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.decision",
            json!({"action": "list", "gate": "script_grant"}),
            None,
            None,
            ToolEntryPoint::Mcp,
            agent_session(),
        )
        .expect("`script_grant` is a known gate filter")
        .value;
    assert!(listed.to_string().contains(&request.id), "{listed}");

    let again = fx.run();
    assert!(again.contains("existing grant request"), "{again}");
    let (reused, _) = fx.only_open();
    assert_eq!(reused.id, request.id, "a repeat refusal reuses the request");
}

fn a_human_answer_grants_the_reviewed_digest_and_the_step_passes_the_grant_gate() {
    let fx = fixture();
    fx.run();
    let (request, payload) = fx.only_open();
    fx.answer(&request, "grant").expect("the human grants");
    let grant = fx
        .runtime
        .verified_script_grant(SCRIPT)
        .unwrap()
        .expect("the answer wrote a verified grant");
    assert_eq!(grant.digest, payload.digest);
    assert_eq!(grant.grantor, HUMAN);
    assert_eq!(grant.provenance, "cli");
    assert!(
        fx.open().is_empty(),
        "the answered request is no longer open"
    );
    let rerun = fx.run();
    assert!(
        !rerun.contains("grant_missing") && !rerun.contains("grant_stale"),
        "the granted step passes the grant gate: {rerun}"
    );
    assert!(
        fx.open().is_empty(),
        "a granted run raises nothing: {rerun}"
    );
}

fn a_stale_grant_raises_a_request_with_both_digests_and_the_diff() {
    let fx = fixture();
    let git = |args: &[&str]| {
        let output = crate::git_test_support::run(&fx.repo, args).unwrap();
        assert!(output.success, "git {args:?}: {}", output.stderr);
    };
    git(&["init", "-q"]);
    git(&["add", "tools"]);
    git(&[
        "-c",
        "user.name=qa",
        "-c",
        "user.email=qa@example.com",
        "commit",
        "-qm",
        "Add the report",
    ]);
    let old = fx
        .runtime
        .grant_script(SCRIPT, &ScriptGrantOptions::default())
        .unwrap();
    fx.write_entry("echo '{\"changed\": true}'\n");
    git(&[
        "-c",
        "user.name=qa",
        "-c",
        "user.email=qa@example.com",
        "commit",
        "-qam",
        "Report a change",
    ]);
    let overview = fx
        .runtime
        .script_grant_overview()
        .unwrap()
        .into_iter()
        .find(|script| script.name == SCRIPT)
        .unwrap();
    assert_eq!(overview.state, "stale");
    assert!(
        overview
            .stale_commit
            .as_deref()
            .is_some_and(|commit| commit.ends_with(" Report a change")),
        "the overview names the commit that made the grant stale: {:?}",
        overview.stale_commit
    );
    let refused = fx.run();
    assert!(refused.contains("grant_stale"), "{refused}");
    let (_, payload) = fx.only_open();
    assert_eq!(payload.refusal, "grant_stale");
    assert_eq!(payload.granted_digest.as_deref(), Some(old.digest.as_str()));
    assert_eq!(payload.digest, fx.current_digest());
    assert_ne!(payload.digest, old.digest);
    assert!(
        payload.diff.contains("-echo '{}'") && payload.diff.contains("+echo '{\"changed\": true}'"),
        "the diff is against the granted bytes: {}",
        payload.diff
    );
    assert!(
        !payload
            .diff
            .contains(&format!("+++ .orbit/resources/scripts/{SCRIPT}.yaml")),
        "an unchanged manifest adds no lines: {}",
        payload.diff
    );
}

fn an_answer_after_the_script_changed_is_refused_and_grants_nothing() {
    let fx = fixture();
    fx.run();
    let (request, _) = fx.only_open();
    fx.write_entry("echo '{\"edited\": \"after review\"}'\n");
    let refused = fx
        .answer(&request, "grant")
        .expect_err("the bytes moved on since the request was raised");
    assert!(
        refused.contains("superseded") || refused.contains("changed"),
        "{refused}"
    );
    assert!(
        fx.runtime.verified_script_grant(SCRIPT).unwrap().is_none(),
        "a refused answer writes no grant"
    );
}

fn an_insufficient_grant_raises_a_request_naming_requested_and_granted() {
    let fx = fixture();
    fx.write_manifest("  requests:\n    network: https\n");
    fx.runtime
        .grant_script(
            SCRIPT,
            &ScriptGrantOptions {
                network: Some(ScriptNetwork::None),
                ..Default::default()
            },
        )
        .unwrap();
    let refused = fx.run();
    assert!(refused.contains("grant_insufficient"), "{refused}");
    let (request, payload) = fx.only_open();
    assert_eq!(payload.refusal, "grant_insufficient");
    assert_eq!(payload.requested.network, ScriptNetwork::Https);
    assert_eq!(
        payload.granted.as_ref().map(|granted| granted.network),
        Some(ScriptNetwork::None)
    );
    assert_eq!(
        payload.granted_digest.as_deref(),
        Some(payload.digest.as_str())
    );
    fx.answer(&request, "grant").expect("the human widens");
    let grant = fx.runtime.verified_script_grant(SCRIPT).unwrap().unwrap();
    assert_eq!(
        grant.network,
        ScriptNetwork::Https,
        "the answer grants the requested set"
    );
}

fn a_requested_timeout_above_the_default_ceiling_is_granted_by_answer_and_dashboard() {
    // Answer path: a default grant (3600s ceiling) refuses a 7200s request
    // as insufficient; the human answer must grant exactly that timeout.
    let fx = fixture();
    fx.write_manifest("  requests:\n    timeout_seconds: 7200\n");
    fx.runtime
        .grant_script(SCRIPT, &ScriptGrantOptions::default())
        .unwrap();
    let refused = fx.run();
    assert!(refused.contains("grant_insufficient"), "{refused}");
    let (request, payload) = fx.only_open();
    assert_eq!(payload.requested.timeout_seconds, Some(7200));
    assert_eq!(
        payload
            .granted
            .as_ref()
            .and_then(|granted| granted.timeout_seconds),
        Some(3600)
    );
    fx.answer(&request, "grant")
        .expect("the human grants the timeout");
    let grant = fx.runtime.verified_script_grant(SCRIPT).unwrap().unwrap();
    assert_eq!(grant.limits.timeout_max_seconds, 7200);
    let rerun = fx.run();
    assert!(
        !rerun.contains("grant_missing")
            && !rerun.contains("grant_stale")
            && !rerun.contains("grant_insufficient"),
        "the granted timeout passes the grant gate: {rerun}"
    );
    assert!(
        fx.open().is_empty(),
        "a granted run raises nothing: {rerun}"
    );

    // Dashboard direct grant (no open request) grants the same timeout.
    let fx = fixture();
    fx.write_manifest("  requests:\n    timeout_seconds: 7200\n");
    let digest = fx.current_digest();
    let grant = fx
        .runtime
        .grant_script_from_dashboard(SCRIPT, &digest, HUMAN.to_string())
        .unwrap();
    assert_eq!(grant.limits.timeout_max_seconds, 7200);
    let rerun = fx.run();
    assert!(
        !rerun.contains("grant_missing")
            && !rerun.contains("grant_stale")
            && !rerun.contains("grant_insufficient"),
        "the dashboard-granted timeout passes the grant gate: {rerun}"
    );
}

fn an_unrepresentable_timeout_is_refused_by_answer_and_dashboard_and_grants_nothing() {
    // ORB-21454: a timeout no deadline can hold must be refused at grant time.
    // Recording it lets the step start a deadline that panics on overflow.
    let unrepresentable = u64::MAX;
    let manifest = format!("  requests:\n    timeout_seconds: {unrepresentable}\n");

    // Answer path: the default 3600s grant refuses the request as insufficient.
    // The human answer must be refused and leave that grant in place.
    let fx = fixture();
    fx.write_manifest(&manifest);
    fx.runtime
        .grant_script(SCRIPT, &ScriptGrantOptions::default())
        .unwrap();
    let before = fx.runtime.verified_script_grant(SCRIPT).unwrap().unwrap();
    let refused = fx.run();
    assert!(refused.contains("grant_insufficient"), "{refused}");
    let (request, payload) = fx.only_open();
    assert_eq!(payload.requested.timeout_seconds, Some(unrepresentable));
    let error = fx
        .answer(&request, "grant")
        .expect_err("an unrepresentable timeout must not be granted by answer");
    assert!(error.contains("nothing was granted"), "{error}");
    let after = fx.runtime.verified_script_grant(SCRIPT).unwrap().unwrap();
    assert_eq!(
        after.grant_id, before.grant_id,
        "the refused answer keeps the existing grant"
    );
    assert_eq!(after.limits.timeout_max_seconds, 3600);

    // Dashboard path with no grant yet: the refusal records nothing.
    let fx = fixture();
    fx.write_manifest(&manifest);
    let digest = fx.current_digest();
    let error = fx
        .runtime
        .grant_script_from_dashboard(SCRIPT, &digest, HUMAN.to_string())
        .expect_err("the dashboard must refuse an unrepresentable timeout");
    assert!(error.to_string().contains("nothing was granted"), "{error}");
    assert!(
        fx.runtime.verified_script_grant(SCRIPT).unwrap().is_none(),
        "the refused dashboard grant records no grant"
    );

    // Dashboard path over an existing grant: the refusal keeps that grant.
    let fx = fixture();
    fx.write_manifest(&manifest);
    fx.runtime
        .grant_script(SCRIPT, &ScriptGrantOptions::default())
        .unwrap();
    let before = fx.runtime.verified_script_grant(SCRIPT).unwrap().unwrap();
    let digest = fx.current_digest();
    let error = fx
        .runtime
        .grant_script_from_dashboard(SCRIPT, &digest, HUMAN.to_string())
        .expect_err("the dashboard must refuse an unrepresentable timeout");
    assert!(error.to_string().contains("nothing was granted"), "{error}");
    let after = fx.runtime.verified_script_grant(SCRIPT).unwrap().unwrap();
    assert_eq!(
        after.grant_id, before.grant_id,
        "the refused dashboard grant keeps the existing grant"
    );
    assert_eq!(after.limits.timeout_max_seconds, 3600);
}

fn a_registered_channel_answer_writes_the_grant_with_channel_provenance() {
    let fx = fixture();
    fx.runtime
        .register_decision_channel(CHANNEL, HUMAN, PLUGIN, vec![SENDER.to_string()])
        .unwrap();
    fx.run();
    let (request, payload) = fx.only_open();
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();
    fx.runtime
        .submit_decision_via_channel(DecisionSubmitParams {
            channel: CHANNEL.to_string(),
            request_id: request.id.clone(),
            answer_id: "grant".to_string(),
            note: None,
            token,
            sender: Some(SENDER.to_string()),
            auth_results: Some(AUTH_PASS.to_string()),
            raw_message: None,
            caller: DecisionSubmitCaller::PluginCallback {
                plugin: PLUGIN.to_string(),
            },
        })
        .expect("the bound channel relays the human's answer");
    let grant = fx.runtime.verified_script_grant(SCRIPT).unwrap().unwrap();
    assert_eq!(grant.digest, payload.digest);
    assert_eq!(grant.provenance, format!("decision-channel {CHANNEL}"));
}

fn agents_and_plugin_callbacks_cannot_answer_or_grant() {
    let fx = fixture();
    fx.runtime
        .register_decision_channel(CHANNEL, HUMAN, PLUGIN, vec![SENDER.to_string()])
        .unwrap();
    fx.run();
    let (request, _) = fx.only_open();
    let token = fx
        .runtime
        .mint_decision_token(CHANNEL, &request.id)
        .unwrap();

    // An agent session answering through the tool surface.
    let answer = fx.tool(
        "orbit.decision.answer",
        json!({"id": request.id, "answer_id": "grant"}),
        agent_session(),
    );
    assert!(answer.contains("policy denied"), "{answer}");
    // An agent session granting the script directly: the grant tool is not
    // on any agent surface (it is the operator's `orbit script grant`).
    let grant = fx.tool(
        "orbit.script.grant",
        json!({"name": SCRIPT}),
        agent_session(),
    );
    assert!(
        grant.contains("policy denied") || grant.contains("tool not found"),
        "{grant}"
    );
    // An agent-attributed human answer.
    let attributed = fx
        .runtime
        .answer_decision_request_as_human(
            &request.id,
            "grant",
            None,
            "codex".to_string(),
            Some("cli".to_string()),
        )
        .expect_err("an agent label is not a human");
    assert!(!attributed.to_string().is_empty());
    // An agent, and a plugin callback the channel is not bound to, submitting
    // through the channel with a valid token.
    for caller in [
        DecisionSubmitCaller::Agent,
        DecisionSubmitCaller::PluginCallback {
            plugin: "rogue-plugin".to_string(),
        },
    ] {
        fx.runtime
            .submit_decision_via_channel(DecisionSubmitParams {
                channel: CHANNEL.to_string(),
                request_id: request.id.clone(),
                answer_id: "grant".to_string(),
                note: None,
                token: token.clone(),
                sender: Some(SENDER.to_string()),
                auth_results: Some(AUTH_PASS.to_string()),
                raw_message: None,
                caller: caller.clone(),
            })
            .expect_err("only the bound channel relays a human answer");
    }
    assert!(
        fx.runtime.verified_script_grant(SCRIPT).unwrap().is_none(),
        "no refused path wrote a grant"
    );
    let (still_open, _) = fx.only_open();
    assert_eq!(
        still_open.id, request.id,
        "the request stays open for the human"
    );
}
