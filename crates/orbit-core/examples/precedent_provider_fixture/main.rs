//! Manual public-API verification with synthetic histories and a local plugin.
//! No model, external source, real workspace or real home is accessed.
use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Duration, Utc};
use orbit_config::assistance::{AssistanceSettings, PrecedentProviderPolicy};
use orbit_core::OrbitRuntime;
use orbit_core::application::plugin::{PluginAddOptions, install_plugin};
use orbit_core::application::task::{TaskAddParams, precedent_provider};
use orbit_store::contracts::TaskHistoryUpdateParams;
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, TaskHistoryEntry,
};
use serde_json::{Value, json};

type FixtureResult<T = ()> = Result<T, Box<dyn Error>>;

fn history_event(at: DateTime<Utc>, by: &str, event: &str, note: Value) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at,
        by: by.into(),
        event: event.into(),
        note: Some(note.to_string()),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }
}

fn fixture(root: &Path) -> FixtureResult {
    let global = root.join("global");
    let workspace = root.join("repo/.orbit");
    let runtime = OrbitRuntime::from_roots(&global, &workspace)?;
    std::env::set_current_dir(root.join("repo"))?;
    let workspace_id = runtime.workspace_id()?;
    let cutoff: DateTime<Utc> = "2026-11-01T09:00:00Z".parse()?;
    let mut query: precedent_provider::PrecedentProviderRequest = serde_json::from_value(json!({
        "contract": "orbit.precedent-provider/1", "workspace_id": workspace_id,
        "request": {"id":"dr-query", "gate":"agent_request", "category":format!("{workspace_id}/agent_request/guide.md"),
            "question":"Which cache?", "options":[{"id":"a", "label":"Cache"}], "topic":"guide.md#cache", "raised_at":cutoff},
        "as_of":cutoff, "k":8
    }))?;
    let row = json!({"id":"external-1", "question":"Which cache?", "options":[{"id":"a", "label":"Cache"}],
        "chosen_option":"a", "context":"synthetic", "scope":{"workspace_id":workspace_id, "category":"cache"},
        "decided_at":cutoff-Duration::days(1), "decided_by":"human:fixture", "decided_by_kind":"human",
        "reversed":true, "citation":{"source":"fixture", "ref":"https://example.invalid/never-fetch", "digest":format!("sha256:{}", "1".repeat(64))}});
    let response = json!({"contract":"orbit.precedent-provider/1", "precedents":[row], "omissions":[], "truncated":false});
    let validate = |value: &Value| {
        precedent_provider::validate_precedent_response(
            &query,
            &serde_json::to_vec(value).unwrap_or_default(),
        )
    };
    assert!(
        validate(&response)
            .map_err(|reason| format!("{reason:?}"))?
            .precedents[0]
            .reversed
    );
    let invalid: Vec<Value> = vec![
        {
            let mut v = response.clone();
            v["contract"] = json!("other/1");
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["decided_at"] = json!(cutoff);
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["decided_at"] = json!(cutoff + Duration::seconds(1));
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["chosen_option"] = json!("absent");
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["decided_by_kind"] = json!("automation");
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["decided_by_kind"] = json!("model");
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["decided_by"] = json!(" model ");
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["decided_by"] = json!("gpt-6.1-sol");
            v
        },
        {
            let mut v = response.clone();
            v["precedents"] = json!([row.clone(), row.clone()]);
            v
        },
        {
            let mut v = response.clone();
            v["precedents"] = json!(vec![row.clone(); 9]);
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["context"] = json!("é".repeat(4097));
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["citation"]["digest"] = json!("bad");
            v
        },
        {
            let mut v = response.clone();
            v["precedents"][0]["scope"]["workspace_id"] = json!("other");
            v
        },
    ];
    for value in &invalid {
        assert!(validate(value).is_err());
    }
    let mut oversized = serde_json::to_vec(&response)?;
    oversized.extend(vec![b' '; precedent_provider::MAX_PRECEDENT_RESPONSE_BYTES]);
    assert!(precedent_provider::validate_precedent_response(&query, &oversized).is_err());
    // A valid prefix never survives an invalid suffix.
    let mut mixed = response.clone();
    mixed["precedents"] = json!([row.clone(), invalid[3]["precedents"][0].clone()]);
    mixed["precedents"][1]["id"] = json!("invalid-suffix");
    assert!(validate(&mixed).is_err());
    assert!(!runtime.retrieve_decision_precedents(&query)?.enabled);

    let task = runtime.add_task(TaskAddParams {
        title: "Synthetic history".into(),
        ..Default::default()
    })?;
    let answer = DecisionAnswer {
        id: "a".into(),
        label: "Cache".into(),
        consequence: "Keep cache".into(),
        option: None,
    };
    let mut request = DecisionRequest::new(
        task.id.to_string(),
        DecisionGate::AgentRequest,
        vec![answer],
        DecisionBinding::for_task(&task, None),
        "fixture".into(),
        cutoff - Duration::days(10),
    )
    .with_summary("Which cache?".into(), None);
    request.topic = Some("guide.md#cache".into());
    let resolution = |actor: &str, at: DateTime<Utc>| json!({"request_id": request.id, "answer_id":"a", "answered_by":actor, "answered_at":at});
    let human_at = cutoff - Duration::days(5);
    let model_at = cutoff - Duration::days(4);
    let mut history = vec![
        request.history_entry("fixture"),
        history_event(
            human_at,
            "human:fixture",
            "decision_answered",
            resolution("human:fixture", human_at),
        ),
        history_event(
            human_at + Duration::hours(1),
            "human:fixture",
            "decision_reopened",
            json!({"request_id":request.id}),
        ),
        history_event(
            model_at,
            "model",
            "decision_answered",
            resolution("model", model_at),
        ),
        history_event(
            model_at + Duration::hours(1),
            "precedent",
            "decision_answered",
            resolution("precedent", model_at + Duration::hours(1)),
        ),
        history_event(
            cutoff,
            "human:fixture",
            "decision_answered",
            resolution("human:fixture", cutoff),
        ),
    ];
    let records = precedent_provider::decision_precedents_from_history(&query, &history);
    assert_eq!(records.len(), 1);
    assert!(records[0].reversed);
    history.push(history_event(
        model_at + Duration::hours(2),
        "human:fixture",
        precedent_provider::PRECEDENT_HUMAN_CONFIRMED_EVENT,
        json!({"request_id":request.id,"answered_at":model_at,"confirmed_by":"human:fixture"}),
    ));
    let records = precedent_provider::decision_precedents_from_history(&query, &history);
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|record| matches!(
        record.decided_by_kind,
        precedent_provider::PrecedentDeciderKind::HumanConfirmed
    )));
    let mut earlier = query.clone();
    earlier.as_of = model_at + Duration::hours(1);
    earlier.request.raised_at = earlier.as_of;
    assert_eq!(
        precedent_provider::decision_precedents_from_history(&earlier, &history).len(),
        1
    );
    runtime.stores().task_history().update_task_history(
        &task.id,
        TaskHistoryUpdateParams {
            actor: "human:fixture".into(),
            append_history: history,
            ..Default::default()
        },
    )?;
    let mut settings = AssistanceSettings::default();
    settings.precedent_providers.push(PrecedentProviderPolicy {
        workspace_id: workspace_id.clone(),
        built_in: true,
        plugin_tools: vec![],
    });
    let save = |settings: &AssistanceSettings| -> FixtureResult {
        std::fs::write(
            global.join("config.toml"),
            format!("assistance = {}\n", toml::Value::try_from(settings)?),
        )?;
        Ok(())
    };
    save(&settings)?;
    let built_in = runtime.retrieve_decision_precedents(&query)?;
    assert_eq!(built_in.precedents.len(), 2);
    assert!(built_in.provider_gaps.is_empty());

    // Real install, grants, audited dispatch, callback session, exec envelope,
    // sandbox and output parsing; the fixture backend checks its callback fd.
    let plugin_root = root.join("source/.orbit-plugin");
    std::fs::create_dir_all(plugin_root.join("bin"))?;
    let mut duplicate = serde_json::to_value(&built_in.precedents[0])?;
    duplicate["id"] = json!("external-duplicate");
    let plugin_response = json!({"contract":"orbit.precedent-provider/1","precedents":[row.clone(), duplicate],"omissions":[],"truncated":false});
    let script = format!(
        "#!/bin/sh\nset -eu\nread -r envelope || test -n \"$envelope\"\ntest -n \"$ORBIT_PLUGIN_CALLBACK_FD\"\ncase \"$envelope\" in *orbit.precedent-provider/1*) ;; *) exit 8;; esac\ncase \"$envelope\" in *invalid-query*) printf '%s\\n' '{}';; *) printf '%s\\n' '{}';; esac\n",
        json!({"ok":true,"output":mixed})
            .to_string()
            .replace('\'', "'\\''"),
        json!({"ok":true,"output":plugin_response})
            .to_string()
            .replace('\'', "'\\''")
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
        &runtime,
        plugin_root.to_str().ok_or("plugin path")?,
        &PluginAddOptions {
            enable: true,
            ..Default::default()
        },
    )?;
    settings.precedent_providers[0]
        .plugin_tools
        .push("precedentfixture.retrieve".into());
    save(&settings)?;
    let runtime = OrbitRuntime::from_roots(&global, &workspace)?;
    let merged = runtime.retrieve_decision_precedents(&query)?;
    assert!(
        merged.provider_gaps.is_empty(),
        "{:?}",
        merged.provider_gaps
    );
    assert_eq!(merged.precedents.len(), 3);
    assert_eq!(merged.precedents[0].id, "external-1");
    assert_eq!(
        serde_json::to_value(&merged)?,
        serde_json::to_value(runtime.retrieve_decision_precedents(&query)?)?
    );
    query.request.id = "invalid-query".into();
    let refused = runtime.retrieve_decision_precedents(&query)?;
    assert_eq!(refused.precedents.len(), 2);
    assert_eq!(refused.provider_gaps.len(), 1);
    let audit =
        runtime
            .sqlite_store()?
            .list_audit_events(&orbit_store::contracts::AuditEventFilter {
                tool_name: Some("precedentfixture.retrieve".into()),
                limit: 100,
                ..Default::default()
            })?;
    assert!(
        audit
            .iter()
            .any(|event| event.subcommand.as_deref() == Some("precedent_provider_gap"))
    );
    // Ranking is exercised through the runtime API with k=1, including a
    // newer record losing to category, topic or lexical relevance.
    settings.precedent_providers[0].plugin_tools.clear();
    save(&settings)?;
    let mut ranked_history = Vec::new();
    for (id, category, topic, question, hours) in [
        (
            "rank-a",
            query.request.category.as_str(),
            "guide.md#cache",
            "alpha beta",
            120,
        ),
        (
            "rank-b",
            query.request.category.as_str(),
            "guide.md#cache",
            "alpha",
            24,
        ),
        (
            "rank-c",
            query.request.category.as_str(),
            "other.md#cache",
            "alpha beta",
            48,
        ),
        (
            "rank-d",
            "other-category",
            "guide.md#cache",
            "alpha beta",
            12,
        ),
        (
            "rank-e",
            query.request.category.as_str(),
            "guide.md#cache",
            "alpha beta",
            96,
        ),
    ] {
        let mut candidate = request.clone();
        candidate.id = id.into();
        candidate.topic = Some(topic.into());
        candidate.summary = Some(question.into());
        let mut raised = candidate.history_entry("fixture");
        let mut note: Value = serde_json::from_str(raised.note.as_deref().ok_or("request note")?)?;
        note["category"] = json!(category);
        raised.note = Some(note.to_string());
        ranked_history.push(raised);
        let at = cutoff - Duration::hours(hours);
        ranked_history.push(history_event(
            at,
            "human:fixture",
            "decision_answered",
            json!({
                "request_id":id,"answer_id":"a","answered_by":"human:fixture","answered_at":at
            }),
        ));
    }
    runtime.stores().task_history().update_task_history(
        &task.id,
        TaskHistoryUpdateParams {
            actor: "human:fixture".into(),
            append_history: ranked_history,
            ..Default::default()
        },
    )?;
    query.k = 1;
    query.request.question = "alpha beta".into();
    assert!(
        runtime.retrieve_decision_precedents(&query)?.precedents[0]
            .id
            .starts_with("decision:rank-e:")
    );
    query.request.topic = Some("other.md#cache".into());
    assert!(
        runtime.retrieve_decision_precedents(&query)?.precedents[0]
            .id
            .starts_with("decision:rank-c:")
    );
    query.request.topic = Some("guide.md#cache".into());
    query.request.category = "other-category".into();
    assert!(
        runtime.retrieve_decision_precedents(&query)?.precedents[0]
            .id
            .starts_with("decision:rank-d:")
    );
    query.request.category = format!("{workspace_id}/agent_request/guide.md");
    query.request.question = "alpha".into();
    assert!(
        runtime.retrieve_decision_precedents(&query)?.precedents[0]
            .id
            .starts_with("decision:rank-b:")
    );
    settings.precedent_providers.clear();
    save(&settings)?;
    assert!(!runtime.retrieve_decision_precedents(&query)?.enabled);
    std::io::stdout().write_all(b"{\"fixture\":\"precedent-provider/1\",\"status\":\"passed\",\"external_network_calls\":0,\"labels\":\"synthetic\"}\n")?;
    Ok(())
}

fn main() -> FixtureResult {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "--fixture-child") {
        return fixture(Path::new(args.get(2).ok_or("fixture root required")?));
    }
    let scratch = std::fs::canonicalize(args.get(1).ok_or("pass .orbit/tmp as scratch root")?)?;
    let root = tempfile::Builder::new()
        .prefix("precedent-fixture-")
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
        return Err("isolated precedent fixture failed".into());
    }
    Ok(())
}
