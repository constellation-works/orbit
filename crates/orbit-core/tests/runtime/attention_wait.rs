//! A shell may wait on attention only through a grant a capability-holding
//! session minted, and only for the workspaces that session read.

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::attention::AttentionWaitStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

fn session(capabilities: &[McpCapability]) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: capabilities.iter().copied().collect(),
        ..Default::default()
    }
}

fn attention(
    runtime: &OrbitRuntime,
    input: Value,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.attention",
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            session,
        )
        .map(|outcome| outcome.value)
}

fn runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    (root, runtime)
}

fn is_denied(result: Result<impl std::fmt::Debug, OrbitError>) -> bool {
    matches!(result, Err(OrbitError::CapabilityDenied(_)))
}

fn shell_without_a_session_grant_is_refused() {
    let (_root, runtime) = runtime();
    let since = "atc1.unused";
    let input = || json!({"since": since});
    assert!(is_denied(runtime.attention_wait("", input(), "1s", None)));
    assert!(is_denied(runtime.attention_wait(
        "oatw_00",
        input(),
        "1s",
        None
    )));
    assert!(is_denied(runtime.attention_wait(
        &format!("oatw_{}", "0".repeat(64)),
        input(),
        "1s",
        None
    )));
    // A caller that asserts no capability cannot mint one for itself.
    assert!(
        attention(
            &runtime,
            json!({"wait_grant": true}),
            ToolSessionContext::default()
        )
        .is_err()
    );
}

fn minted_grant_waits_for_a_delta_and_stays_inside_its_scope() {
    let (_root, runtime) = runtime();
    let minted = attention(
        &runtime,
        json!({"wait_grant": true}),
        session(&[McpCapability::Agent]),
    )
    .unwrap();
    let token = minted["wait_grant"]["token"].as_str().unwrap().to_owned();
    assert!(
        minted["filters"].get("wait_grant").is_none(),
        "the grant is not echoed as a filter"
    );
    let cursor = minted["cursor"]["next"].as_str().unwrap().to_owned();

    // Nothing changed: the wait times out, and says what it could not read.
    let outcome = runtime
        .attention_wait(&token, json!({"since": cursor}), "1s", None)
        .unwrap();
    assert_eq!(outcome.status, AttentionWaitStatus::TimedOut);
    let response = outcome.response.unwrap();
    assert!(
        response["degraded"]
            .as_array()
            .is_some_and(|degraded| !degraded.is_empty()),
        "an agent-capability read still reports the kinds it cannot read: {response}"
    );
    let cursor = response["cursor"]["next"].as_str().unwrap().to_owned();

    // A new open friction record is a delta.
    use orbit_store::contracts::FrictionAddParams;
    orbit_core::runtime::friction::store_for(&runtime)
        .unwrap()
        .add(FrictionAddParams {
            model: "test".into(),
            title: Some("Wait delta".into()),
            body: "Appears after the cursor.".into(),
            tags: vec!["other".into()],
            status: orbit_types::record::FrictionStatus::Open,
            during_task: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    let outcome = runtime
        .attention_wait(&token, json!({"since": cursor}), "30s", None)
        .unwrap();
    assert_eq!(outcome.status, AttentionWaitStatus::Changed);
    assert_eq!(
        outcome.response.unwrap()["items"][0]["kind"],
        "friction_untriaged"
    );

    // The grant covers what was read, and nothing wider.
    for widening in [
        json!({"since": "x", "all": true}),
        json!({"since": "x", "host": "elsewhere"}),
        json!({"since": "x", "workspace": "not-granted"}),
        json!({"since": "x", "wait_grant": true}),
    ] {
        assert!(
            is_denied(runtime.attention_wait(&token, widening.clone(), "1s", None)),
            "{widening} must be refused"
        );
    }
}

crate::scenario_table!(
    attention_wait,
    shell_without_a_session_grant_is_refused,
    minted_grant_waits_for_a_delta_and_stays_inside_its_scope,
);
