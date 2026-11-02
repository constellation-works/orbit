//! Public-entry proof for the shadow precedent-decision consumer.
//! Synthetic answers check plumbing only. No real home, workspace, or network.
#![allow(
    clippy::unwrap_used,
    reason = "isolated manual fixture assertions fail immediately on missing synthetic state"
)]
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use orbit_config::assistance::{AssistancePolicy, AssistanceSettings, PrecedentProviderPolicy};
use orbit_core::OrbitRuntime;
use orbit_core::application::assistance::{AssistanceAdapter, AssistanceService};
use orbit_core::application::plugin::{PluginAddOptions, install_plugin};
use orbit_core::application::precedent_decision::{self, PrecedentDecisionReceipt};
use orbit_core::application::task::{AgentDecisionRaise, TaskAddParams, TaskUpdateParams};
use orbit_store::contracts::TaskHistoryUpdateParams;
use orbit_store::contracts::assistance::AssistanceClaim;
use orbit_types::assistance::{
    self, AssistanceAnswer, AssistanceDisposition, AssistanceMode, AssistanceProbability,
    AssistanceQuestion, AssistanceReason, AssistanceRequest, AssistanceResult, AssistanceValue,
};
use orbit_types::task::{
    AgentDecisionOptionInput, DECISION_ANSWERED_EVENT, DecisionRequest, DecisionResolution,
    PRECEDENT_ACTOR, TaskHistoryEntry,
};
use serde_json::{Value, json};

type FixtureResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct SyntheticAdapter {
    calls: AtomicUsize,
}

impl AssistanceAdapter for SyntheticAdapter {
    fn assistance_adapter_identity(&self) -> &str {
        "synthetic-precedent-decision/1"
    }
    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        _: &[String],
        _: &orbit_config::assistance::AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason> {
        Ok(request)
    }
    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        _: &orbit_config::assistance::AssistancePolicy,
    ) -> AssistanceResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
                        .unwrap_or(&allowed[0])
                        .clone();
                    AssistanceAnswer {
                        probability: Some(if allowed.len() == 1 { 1.0 } else { 0.5 }),
                        value: AssistanceValue::Choice(chosen.clone()),
                        probabilities: choice_probabilities(allowed, &chosen),
                        criterion_refs: Vec::new(),
                    }
                }
                AssistanceQuestion::Predicate { .. } => AssistanceAnswer {
                    value: AssistanceValue::Predicate(false),
                    probability: Some(0.1),
                    probabilities: Vec::new(),
                    criterion_refs: Vec::new(),
                },
            };
            answers.insert(name.clone(), answer);
        }
        AssistanceResult {
            disposition: AssistanceDisposition::Uncertain,
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

fn choice_probabilities(allowed: &[String], chosen: &str) -> Vec<AssistanceProbability> {
    if allowed.len() == 1 {
        return vec![AssistanceProbability {
            value: chosen.into(),
            probability: 1.0,
        }];
    }
    let others = allowed.len() - 1;
    let mut remaining = 0.5;
    let mut seen_others = 0;
    allowed
        .iter()
        .map(|id| {
            let probability = if id == chosen {
                0.5
            } else {
                seen_others += 1;
                if seen_others == others {
                    remaining
                } else {
                    let share = 0.5 / others as f64;
                    remaining -= share;
                    share
                }
            };
            AssistanceProbability {
                value: id.clone(),
                probability,
            }
        })
        .collect()
}

fn save(global: &Path, settings: &AssistanceSettings) -> FixtureResult {
    std::fs::write(
        global.join("config.toml"),
        format!("assistance = {}\n", toml::Value::try_from(settings)?),
    )?;
    Ok(())
}

fn provider_calls(runtime: &OrbitRuntime) -> FixtureResult<usize> {
    let events =
        runtime
            .sqlite_store()?
            .list_audit_events(&orbit_store::contracts::AuditEventFilter {
                tool_name: Some("precedentfixture.retrieve".into()),
                status: Some(orbit_types::telemetry::AuditEventStatus::Success),
                limit: 100,
                ..Default::default()
            })?;
    Ok(events.len())
}

fn forbidden(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys().any(|key| {
                matches!(
                    key.as_str(),
                    "sealed_at"
                        | "work_id"
                        | "decision_choice"
                        | "insufficient_precedent"
                        | "precedents_conflict"
                        | "precedent_decision"
                )
            }) || map.values().any(forbidden)
        }
        Value::Array(items) => items.iter().any(forbidden),
        Value::String(text) => {
            text.contains("sealed_at")
                || text.contains("insufficient_precedent")
                || text.contains("precedent-decision")
        }
        _ => false,
    }
}

fn raise(runtime: &OrbitRuntime, task_id: &str, question: &str) -> FixtureResult<DecisionRequest> {
    Ok(runtime
        .raise_agent_decision_request(AgentDecisionRaise {
            task_id: task_id.into(),
            question: question.into(),
            options: vec![
                AgentDecisionOptionInput {
                    label: "Ship the change".into(),
                    consequence: "The task is marked done.".into(),
                    risk_tier: Some("reversible".into()),
                    recommended: true,
                },
                AgentDecisionOptionInput {
                    label: "Hold the change".into(),
                    consequence: "The task stays with the human.".into(),
                    risk_tier: Some("routine".into()),
                    recommended: false,
                },
            ],
            topic: Some("pricing/sign-off".into()),
            filer: "grok".into(),
        })?
        .request)
}

fn policy(workspace_id: &str, mode: AssistanceMode) -> FixtureResult<AssistancePolicy> {
    let template = precedent_decision::precedent_decision_question_template();
    Ok(AssistancePolicy {
        workspace_id: workspace_id.into(),
        use_case: precedent_decision::PRECEDENT_DECISION_USE_CASE.into(),
        mode,
        model: "gpt-6-luna".into(),
        model_revision: "fixture/1".into(),
        policy_version: "synthetic-only/1".into(),
        contract_digest: assistance::assistance_digest(&serde_json::to_vec(&template)?),
        evidence_fields: vec![
            "option_ids".into(),
            "request".into(),
            "task_context".into(),
            "precedents".into(),
            "provenance".into(),
        ],
        evidence_ref_prefixes: vec!["task:".into(), "decision:".into()],
        credential_env: "ORBIT_ASSISTANCE_SYNTHETIC".into(),
    })
}

fn receipt(runtime: &OrbitRuntime, id: &str) -> FixtureResult<Option<PrecedentDecisionReceipt>> {
    Ok(runtime.precedent_decision_receipt(id)?)
}

fn surface(runtime: &OrbitRuntime, request: &DecisionRequest) -> FixtureResult<Value> {
    Ok(json!({
        "show": runtime.get_decision_request(&request.id)?,
        "open": runtime.open_decisions_json(&request.task_id)?,
        "list": runtime.list_decision_requests(None, Some(&request.task_id), true)?,
    }))
}

fn install_calls_plugin(runtime: &OrbitRuntime, root: &Path) -> FixtureResult {
    let plugin_root = root.join("plugin-src/.orbit-plugin");
    std::fs::create_dir_all(plugin_root.join("bin"))?;
    let body = json!({"ok":true,"output":{"contract":"orbit.precedent-provider/1","precedents":[],"omissions":[],"truncated":false}})
        .to_string()
        .replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\nset -eu\nread -r envelope || test -n \"$envelope\"\ntest -n \"$ORBIT_PLUGIN_CALLBACK_FD\"\nprintf '%s\\n' '{body}'\n"
    );
    std::fs::write(plugin_root.join("bin/backend.sh"), script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            plugin_root.join("bin/backend.sh"),
            std::fs::Permissions::from_mode(0o755),
        )?;
    }
    std::fs::write(
        plugin_root.join("plugin.yaml"),
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: precedentfixture\n  version: 1.0.0\n  description: Synthetic precedent provider.\nspec:\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: retrieve\n      description: Retrieve synthetic precedents.\n      execution_kind: read_only\n      input_schema:\n        type: object\n        additionalProperties: true\n",
    )?;
    install_plugin(
        runtime,
        plugin_root.to_str().ok_or("plugin path")?,
        &PluginAddOptions {
            enable: true,
            ..Default::default()
        },
    )?;
    Ok(())
}

fn mark_precedent(runtime: &OrbitRuntime, request: &DecisionRequest) -> FixtureResult {
    let resolution = DecisionResolution {
        request_id: request.id.clone(),
        answer_id: "1".into(),
        answered_by: PRECEDENT_ACTOR.into(),
        provenance: Some("precedent".into()),
        note: None,
        answered_at: Utc::now(),
        precedent: None,
    };
    let entry = TaskHistoryEntry {
        at: Utc::now(),
        by: PRECEDENT_ACTOR.into(),
        event: DECISION_ANSWERED_EVENT.into(),
        note: Some(serde_json::to_string(&resolution)?),
        from_status: None,
        to_status: None,
        operation_id: Some(request.id.clone()),
        decision: None,
    };
    runtime.stores().task_history().update_task_history(
        &request.task_id,
        TaskHistoryUpdateParams {
            actor: PRECEDENT_ACTOR.into(),
            append_history: vec![entry],
            ..Default::default()
        },
    )?;
    Ok(())
}

fn fixture(root: &Path) -> FixtureResult {
    let global = root.join("global");
    let repo = root.join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global)?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(repo.join("src"))?;
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[crews.dk]\nkind = \"human\"\ndescription = \"Fixture human\"\n",
    )?;
    std::fs::write(repo.join("src/lib.rs"), "pub fn price() {}\n")?;
    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        ["config", "user.email", "fixture@example.com"].as_slice(),
        ["config", "user.name", "Fixture"].as_slice(),
        ["config", "commit.gpgsign", "false"].as_slice(),
        ["add", "-A"].as_slice(),
        ["commit", "-q", "-m", "fixture"].as_slice(),
    ] {
        let status = Command::new("git").args(args).current_dir(&repo).status()?;
        if !status.success() {
            return Err(format!("git {args:?} failed").into());
        }
    }
    let runtime = OrbitRuntime::from_roots(&global, &workspace)?;
    let workspace_id = runtime.workspace_id()?;
    install_calls_plugin(&runtime, root)?;
    let mut settings = AssistanceSettings::default();
    settings.budget.epoch = "synthetic-only/1".into();
    settings.budget.token_ceiling = 10_000_000;
    settings.budget.spend_ceiling_microusd = 10_000_000;
    settings.budget.input_microusd_per_million_tokens = Some(1);
    settings.budget.pricing_version = Some("synthetic-price/1".into());
    settings.limits.max_attempts = 1;
    settings.precedent_providers.push(PrecedentProviderPolicy {
        workspace_id: workspace_id.clone(),
        built_in: true,
        plugin_tools: vec!["precedentfixture.retrieve".into()],
    });
    save(&global, &settings)?;
    let runtime = OrbitRuntime::from_roots(&global, &workspace)?;
    let workspace_id = runtime.workspace_id()?;
    let task = runtime.add_task(TaskAddParams {
        title: "Price the plan".into(),
        description: "Decide the price.".into(),
        acceptance_criteria: vec!["A human answer is recorded.".into()],
        crew: Some("dk".into()),
        ..Default::default()
    })?;
    let open = raise(&runtime, &task.id, "Which price should ship?")?;
    assert_eq!(
        provider_calls(&runtime)?,
        0,
        "off mode must not call a provider"
    );
    assert!(
        receipt(&runtime, &open.id)?.is_none(),
        "off mode must not enqueue"
    );
    assert!(
        runtime
            .get_task_history(&task.id)?
            .iter()
            .any(|entry| entry.event == "decision_requested" && entry.at == open.raised_at),
        "notification is durable before any assessment"
    );
    assert!(!forbidden(&surface(&runtime, &open)?));

    settings
        .policies
        .push(policy(&workspace_id, AssistanceMode::Advisory)?);
    save(&global, &settings)?;
    runtime.consider_precedent_decision(&open)?;
    assert_eq!(
        provider_calls(&runtime)?,
        0,
        "advisory must not call a provider"
    );
    assert!(receipt(&runtime, &open.id)?.is_none());

    settings.policies[0].mode = AssistanceMode::Shadow;
    save(&global, &settings)?;
    runtime.consider_precedent_decision(&open)?;
    runtime.consider_precedent_decision(&open)?;
    assert_eq!(
        provider_calls(&runtime)?,
        1,
        "shadow enqueues one provider call"
    );
    let queued = receipt(&runtime, &open.id)?.ok_or("missing shadow receipt")?;
    assert!(queued.work_id.is_some() && queued.sealed_at.is_none());
    let before = surface(&runtime, &open)?;
    assert!(!forbidden(&before));

    let adapter = Arc::new(SyntheticAdapter {
        calls: AtomicUsize::new(0),
    });
    let config_root = global.clone();
    let service = AssistanceService::new(
        orbit_store::compose::assistance_store_from_store(runtime.sqlite_store()?),
        adapter.clone(),
        Arc::new(move || {
            Ok(orbit_config::ConfigStore::open(
                orbit_config::ConfigScope::Global,
                config_root.join("config.toml"),
            )?
            .snapshot()?
            .assistance_settings)
        }),
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    runtime.run_precedent_decision_with_service(&service)?;
    assert_eq!(
        adapter.calls.load(Ordering::SeqCst),
        1,
        "one sealed assessment"
    );
    let sealed = receipt(&runtime, &open.id)?.ok_or("unsealed")?;
    assert!(sealed.sealed_at.is_some());
    assert_eq!(surface(&runtime, &open)?, before);
    assert!(
        service
            .assistance_advisory(sealed.work_id.as_deref().unwrap(), &sealed.source_digest)?
            .is_none()
    );
    thread::sleep(Duration::from_millis(20));
    runtime.answer_decision_request_as_human(
        &open.id,
        "1",
        None,
        "human:fixture".into(),
        Some("cli".into()),
    )?;

    let leaked_task = runtime.add_task(TaskAddParams {
        title: "Price the wait".into(),
        description: "Decide whether to wait.".into(),
        acceptance_criteria: vec!["The late prediction is excluded.".into()],
        crew: Some("dk".into()),
        ..Default::default()
    })?;
    let leaked = raise(&runtime, &leaked_task.id, "Which price should wait?")?;
    assert!(
        runtime
            .get_task_history(&leaked_task.id)?
            .iter()
            .any(|entry| entry.event == "decision_requested" && entry.at == leaked.raised_at)
    );
    let leaked_queued = receipt(&runtime, &leaked.id)?.ok_or("leak not queued")?;
    assert!(leaked_queued.work_id.is_some() && leaked_queued.sealed_at.is_none());
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    runtime.answer_decision_request_as_human(
        &leaked.id,
        "2",
        None,
        "human:fixture".into(),
        Some("cli".into()),
    )?;
    runtime.run_precedent_decision_with_service(&service)?;
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);

    let curated_task = runtime.add_task(TaskAddParams {
        title: "Price the add-on".into(),
        description: "Decide the add-on.".into(),
        acceptance_criteria: vec!["A rule answer is excluded.".into()],
        crew: Some("dk".into()),
        ..Default::default()
    })?;
    let curated = raise(
        &runtime,
        &curated_task.id,
        "Which add-on price should ship?",
    )?;
    let superseded_task = runtime.add_task(TaskAddParams {
        title: "Price the renewal".into(),
        description: "Decide the renewal.".into(),
        acceptance_criteria: vec!["An edit supersedes the request.".into()],
        crew: Some("dk".into()),
        ..Default::default()
    })?;
    let superseded = raise(
        &runtime,
        &superseded_task.id,
        "Which renewal price should ship?",
    )?;
    runtime.run_precedent_decision_with_service(&service)?;
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        provider_calls(&runtime)?,
        4,
        "each shadow request retrieves once"
    );
    mark_precedent(&runtime, &curated)?;
    runtime.update_task_as_human(
        &superseded_task.id,
        TaskUpdateParams {
            description: Some("Renewal terms changed after the request.".into()),
            ..Default::default()
        },
        "human:fixture".into(),
    )?;
    assert!(!forbidden(&surface(&runtime, &open)?));
    assert!(!forbidden(&surface(&runtime, &leaked)?));

    let join = runtime.precedent_decision_join_with_service(&service)?;
    assert_eq!(join["format"], "precedent-decision-join/1");
    let category = format!("{workspace_id}/agent_request/pricing");
    let bucket = &join["categories"][&category];
    assert_eq!(bucket["scored"].as_array().map(Vec::len), Some(1));
    assert_eq!(bucket["scored"][0]["request_id"], open.id);
    assert_eq!(bucket["scored"][0]["human_answer"], "1");
    assert_eq!(bucket["scored"][0]["choice"], "1");
    assert_eq!(bucket["scored"][0]["disposition"], "uncertain");
    assert_eq!(bucket["scored"][0]["precedents_conflict"], false);
    assert_eq!(bucket["counted"]["leaked"], 1);
    assert_eq!(bucket["counted"]["curated_rule"], 1);
    assert_eq!(bucket["counted"]["superseded"], 1);
    assert_eq!(bucket["counted"]["expired"], 0);
    assert_eq!(bucket["counted"]["automation"], 0);
    let scored = bucket["scored"].to_string();
    assert!(!scored.contains(&leaked.id));
    assert!(!scored.contains(&curated.id));
    assert!(!scored.contains(&superseded.id));
    std::io::stdout().write_all(
        b"{\"fixture\":\"precedent-decision/1\",\"status\":\"passed\",\"external_network_calls\":0,\"labels\":\"synthetic\"}\n",
    )?;
    Ok(())
}

fn main() -> FixtureResult {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "--fixture-child") {
        return fixture(Path::new(args.get(2).ok_or("fixture root required")?));
    }
    let scratch = std::fs::canonicalize(args.get(1).ok_or("pass .orbit/tmp as scratch root")?)?;
    let root = tempfile::Builder::new()
        .prefix("precedent-decision-")
        .tempdir_in(scratch)?;
    let output = Command::new(std::env::current_exe()?)
        .arg("--fixture-child")
        .arg(root.path())
        .env_clear()
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root.path())
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        return Err("isolated precedent decision fixture failed".into());
    }
    Ok(())
}
