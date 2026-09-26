//! What both dispatch surfaces tell a backend about the call, and what they
//! must never tell anything else.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_types::plugin::{PluginGrant, PluginPermissions};
use serde_json::{Value, json};

use super::super::backend::{
    DeliveredPluginSecret, PluginBackendSpec, PluginConfigSection, PluginSecretDelivery,
    PluginSecretSource,
};
use super::super::envelope::{
    CallSecrets, MAX_PLUGIN_ERROR_DETAIL_BYTES, call_context, exec_envelope, parse_response,
    take_delivered_plugin_secret_names,
};
use super::super::mcp::tools_call_params;
use super::support::{context, spec};
use crate::{ActivityBinding, ToolContext};

/// A backend spec whose plugin is configured: `[plugins.demo]` over the
/// manifest's defaults, already validated by the host.
fn configured_spec(root: &Path) -> PluginBackendSpec {
    let mut spec = (*spec(
        root.join("bin/backend.sh"),
        root,
        PluginPermissions::default(),
        &[PluginGrant::Fs],
    ))
    .clone();
    spec.config = PluginConfigSection::new(json!({
        "index_dir": "/srv/graph",
        "max_nodes": 500,
        "incremental": true,
        "api_token": "s3cret",
    }));
    spec
}

fn no_secrets() -> CallSecrets {
    CallSecrets::default()
}

/// A source holding values for more names than any one plugin declares, and
/// recording which names it was asked for.
#[derive(Default)]
struct RecordingSource {
    values: BTreeMap<String, DeliveredPluginSecret>,
    asked: Mutex<Vec<Vec<String>>>,
}

impl RecordingSource {
    fn holding(values: &[(&str, &str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            values: values
                .iter()
                .map(|(name, value, version)| {
                    (
                        (*name).to_string(),
                        DeliveredPluginSecret {
                            value: (*value).to_string(),
                            version: (*version).to_string(),
                        },
                    )
                })
                .collect(),
            asked: Mutex::default(),
        })
    }
}

impl PluginSecretSource for RecordingSource {
    fn read(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, DeliveredPluginSecret>, OrbitError> {
        self.asked.lock().expect("asked").push(names.to_vec());
        // Deliberately answers with everything it holds, asked for or not:
        // the delivery must bound the result itself.
        Ok(self.values.clone())
    }
}

/// A spec declaring `api_token` and `refresh_token`, over a source that also
/// holds a value for a name the plugin never declared.
fn secret_spec(root: &Path, source: Arc<RecordingSource>) -> PluginBackendSpec {
    let mut spec = configured_spec(root);
    spec.secrets = PluginSecretDelivery::new(
        vec!["api_token".to_string(), "refresh_token".to_string()],
        source,
    );
    spec
}

fn call_ctx(root: &Path, workspace: &Path) -> ToolContext {
    ToolContext {
        workspace_root: Some(workspace.to_path_buf()),
        agent_name: Some("claude".to_string()),
        model_name: Some("opus-5".to_string()),
        ..context(root)
    }
}

/// An `exec` backend has had no view of its own `[plugins.<ns>]` section: it
/// could only read what the manifest interpolated into its arguments through
/// `{{config.<key>}}`. The envelope now carries the section itself, typed.
#[test]
fn the_exec_envelope_carries_the_effective_config_section() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let spec = configured_spec(temp.path());
    let ctx = call_ctx(temp.path(), &workspace);

    let envelope = exec_envelope(
        &spec,
        &ctx,
        "demo.hello",
        json!({ "name": "world" }),
        &no_secrets(),
    );

    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["tool"], "demo.hello");
    assert_eq!(envelope["input"]["name"], "world");
    assert_eq!(
        envelope["context"],
        json!({
            "workspace_root": workspace.to_string_lossy(),
            "agent": "claude",
            "model": "opus-5",
            "config": {
                "index_dir": "/srv/graph",
                "max_nodes": 500,
                "incremental": true,
                "api_token": "s3cret",
            },
            "task_id": null,
            "job_run_id": null,
        }),
        "the context is the caller's facts plus the plugin's effective section; \
         an interactive call serves no task or run"
    );
    // Typed, not stringified: a backend reading `max_nodes` gets a number,
    // which is what `{{config.<key>}}` substitution cannot give it.
    assert!(envelope["context"]["config"]["max_nodes"].is_u64());
    assert!(envelope["context"]["config"]["incremental"].is_boolean());
}

/// One plugin configured one way must look the same whichever transport the
/// host chose for it: an operator reading `[plugins.<ns>]` is not told which
/// backend type their plugin declares [ORB-12826].
#[test]
fn both_dispatch_surfaces_send_the_same_config_for_one_plugin_and_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let spec = configured_spec(temp.path());
    let ctx = call_ctx(temp.path(), &workspace);

    let exec = exec_envelope(
        &spec,
        &ctx,
        "demo.hello",
        json!({ "name": "world" }),
        &no_secrets(),
    );
    let mcp = tools_call_params(
        &spec,
        &ctx,
        "demo.hello",
        "hello",
        json!({ "name": "world" }),
        &no_secrets(),
    );

    assert_eq!(
        exec["context"]["config"], mcp["_meta"]["orbit"]["config"],
        "one resolution feeds both surfaces"
    );
    assert_eq!(exec["context"]["config"], *spec.config.as_value());

    // And the rest of the context agrees too: `mcp` adds the tool name its
    // shared child has no `ORBIT_TOOL_NAME` for, and nothing else.
    let mut expected = exec["context"].clone();
    expected["tool"] = json!("demo.hello");
    assert_eq!(mcp["_meta"]["orbit"], expected);
    assert_eq!(mcp["name"], "hello");
    assert_eq!(mcp["arguments"]["name"], "world");
}

/// A backend that authorizes its own writes must match them against who is
/// calling, and the agent writes the tool input. So the task and run a managed
/// call serves come from the host's binding on both surfaces, and an input
/// naming another task or run changes nothing but the input [ORB-13115].
#[test]
fn a_managed_call_names_its_host_attested_task_and_run_on_both_surfaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let spec = configured_spec(temp.path());
    let ctx = ToolContext {
        activity_binding: Some(ActivityBinding {
            job_run_id: "jrun-host".to_string(),
            task_id: Some("ORB-7".to_string()),
        }),
        ..call_ctx(temp.path(), &workspace)
    };
    let forged = json!({
        "task_id": "ORB-999",
        "job_run_id": "jrun-forged",
        "context": { "task_id": "ORB-999" },
        "_meta": { "orbit": { "task_id": "ORB-999" } },
    });

    let exec = exec_envelope(&spec, &ctx, "demo.hello", forged.clone(), &no_secrets());
    let mcp = tools_call_params(
        &spec,
        &ctx,
        "demo.hello",
        "hello",
        forged.clone(),
        &no_secrets(),
    );

    for (surface, context) in [("exec", &exec["context"]), ("mcp", &mcp["_meta"]["orbit"])] {
        assert_eq!(context["task_id"], "ORB-7", "{surface}: {context}");
        assert_eq!(context["job_run_id"], "jrun-host", "{surface}: {context}");
    }
    assert_eq!(
        exec["input"], forged,
        "the input still reaches the backend as written"
    );
    assert_eq!(mcp["arguments"], forged);

    // A run step that serves no task still names its run.
    let ctx = ToolContext {
        activity_binding: Some(ActivityBinding {
            job_run_id: "jrun-host".to_string(),
            task_id: None,
        }),
        ..call_ctx(temp.path(), &workspace)
    };
    let context = call_context(&spec, &ctx, None, &no_secrets());
    assert_eq!(context["task_id"], Value::Null);
    assert_eq!(context["job_run_id"], "jrun-host");
}

/// A plugin is configured with its credentials like any other key, so the
/// section goes to the backend process and nowhere else. `Debug` is the one
/// way a value could reach a log line without a caller meaning it to: it
/// prints the key names only.
#[test]
fn the_config_section_is_not_printed_by_debug() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec = configured_spec(temp.path());

    let rendered = format!("{spec:?}");
    assert!(
        !rendered.contains("s3cret"),
        "a formatted spec must not carry configured values: {rendered}"
    );
    assert!(
        rendered.contains("api_token"),
        "the key names stay, so a diagnostic can still say what was set: {rendered}"
    );
    assert!(!format!("{:?}", spec.config).contains("s3cret"));
}

/// The section a plugin declares nothing for is an empty object rather than
/// an absent key, so a backend can read `context.config` unconditionally.
#[test]
fn an_unconfigured_plugin_still_receives_a_config_object() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec = (*spec(
        temp.path().join("bin/backend.sh"),
        temp.path(),
        PluginPermissions::default(),
        &[],
    ))
    .clone();
    let ctx = context(temp.path());

    let context = call_context(&spec, &ctx, None, &no_secrets());
    assert_eq!(context["config"], json!({}));
    assert!(spec.config_values().is_empty());
}

#[test]
fn exec_error_preserves_structured_fields_and_defaults_retryable() {
    let error = parse_response(
        "demo.hello",
        r#"{"ok":false,"error":{"code":"bad_plan","message":"invalid step","retryable":true,"detail":{"at":"posts[0]"}}}"#,
    )
    .expect_err("the backend reported an error");
    match error {
        OrbitError::RemoteTool { code, payload, .. } => {
            assert_eq!(code, "bad_plan");
            assert_eq!(
                payload,
                json!({"code":"bad_plan","message":"invalid step","retryable":true,"detail":{"at":"posts[0]"}})
            );
        }
        other => panic!("expected structured plugin error, got {other}"),
    }

    let error = parse_response(
        "demo.hello",
        r#"{"ok":false,"error":{"code":"refused","message":"no"}}"#,
    )
    .expect_err("the backend reported an error");
    assert!(
        matches!(error, OrbitError::RemoteTool { payload, .. } if payload["retryable"] == false && payload.get("detail").is_none())
    );
}

#[test]
fn malformed_error_falls_back_to_execution_and_oversized_detail_is_omitted() {
    for error in [
        json!(null),
        json!({"code": 42, "message": "bad"}),
        json!({"code": "bad", "message": ""}),
        json!({"code": "bad", "message": "bad", "retryable": "yes"}),
    ] {
        let response = json!({"ok": false, "error": error}).to_string();
        assert!(
            matches!(
                parse_response("demo.hello", &response),
                Err(OrbitError::Execution(_))
            ),
            "malformed error must retain the execution failure: {response}"
        );
    }
    let detail = Value::String("x".repeat(MAX_PLUGIN_ERROR_DETAIL_BYTES));
    let response = json!({"ok": false, "error": {
        "code": "too_large", "message": "detail exceeds limit", "detail": detail,
    }});
    let error = parse_response("demo.hello", &response.to_string())
        .expect_err("the backend reported an error");
    assert!(
        matches!(error, OrbitError::RemoteTool { payload, .. } if payload.get("detail").is_none() && payload["code"] == "too_large")
    );
}

/// Both dispatch surfaces carry the plugin's declared secrets in the request:
/// `context.secrets` on the `exec` envelope, `_meta.orbit.secrets` on `mcp`,
/// each entry `{value, version}`. A declared secret with no value is omitted,
/// and a name the manifest does not declare is never delivered, whatever the
/// source holds.
#[test]
fn declared_secrets_ride_the_request_on_both_surfaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let source = RecordingSource::holding(&[
        ("api_token", "tok-value", "v1"),
        ("other_plugin_token", "not-yours", "v9"),
    ]);
    let spec = secret_spec(temp.path(), Arc::clone(&source));
    let ctx = call_ctx(temp.path(), &workspace);

    let secrets = CallSecrets::resolve(&spec).expect("resolve");
    let exec = exec_envelope(&spec, &ctx, "demo.hello", json!({}), &secrets);
    let mcp = tools_call_params(&spec, &ctx, "demo.hello", "hello", json!({}), &secrets);

    let expected = json!({ "api_token": { "value": "tok-value", "version": "v1" } });
    assert_eq!(exec["context"]["secrets"], expected, "{exec}");
    assert_eq!(mcp["_meta"]["orbit"]["secrets"], expected, "{mcp}");
    assert_eq!(
        *source.asked.lock().expect("asked"),
        vec![vec!["api_token".to_string(), "refresh_token".to_string()]],
        "the source is asked for the declared names only"
    );
    for request in [&exec, &mcp] {
        let text = request.to_string();
        assert!(
            !text.contains("not-yours") && !text.contains("other_plugin_token"),
            "an undeclared name never reaches the request: {text}"
        );
        assert!(
            !text.contains("refresh_token"),
            "an unset declared secret is omitted, not sent empty: {text}"
        );
    }
}

/// A plugin that declares no secrets has no `secrets` key at all, and one that
/// declares some but has none set gets an empty object.
#[test]
fn the_secrets_object_is_present_exactly_when_the_plugin_declares_secrets() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let ctx = call_ctx(temp.path(), &workspace);

    let undeclared = configured_spec(temp.path());
    let secrets = CallSecrets::resolve(&undeclared).expect("resolve");
    let context = call_context(&undeclared, &ctx, None, &secrets);
    assert!(context.get("secrets").is_none(), "{context}");

    let unset = secret_spec(temp.path(), RecordingSource::holding(&[]));
    let secrets = CallSecrets::resolve(&unset).expect("resolve");
    let context = call_context(&unset, &ctx, None, &secrets);
    assert_eq!(context["secrets"], json!({}));
}

/// The audit row names what a call delivered, and a formatted spec or
/// resolution names no value.
#[test]
fn delivery_records_names_and_debug_prints_no_value() {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec = secret_spec(
        temp.path(),
        RecordingSource::holding(&[("api_token", "tok-value", "v1")]),
    );
    let secrets = CallSecrets::resolve(&spec).expect("resolve");

    let _ = take_delivered_plugin_secret_names();
    secrets.record_delivery();
    assert_eq!(take_delivered_plugin_secret_names(), vec!["api_token"]);
    assert!(
        take_delivered_plugin_secret_names().is_empty(),
        "taking the record clears it"
    );

    for rendered in [format!("{spec:?}"), format!("{secrets:?}")] {
        assert!(!rendered.contains("tok-value"), "{rendered}");
    }
}
