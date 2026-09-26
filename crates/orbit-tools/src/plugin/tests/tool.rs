use orbit_types::plugin::{PluginGrant, PluginPermissions, PluginSecretUpdateStatus};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::backend::{
    DeliveredPluginSecret, PluginConfigSection, PluginSecretDelivery, PluginSecretRotation,
    PluginSecretSource,
};
use super::super::envelope::take_plugin_secret_updates;
use super::super::tool::PluginTool;
use super::support::{CasSource, context, require_sandbox, spec, stub_backend, tool};
use crate::{Tool, ToolContext, ToolExecutionKind};

const ECHO_BACKEND: &str = "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"arg\":\"%s\",\"plugin\":\"%s\",\"allowed\":\"%s\",\"programs\":\"%s\",\"envelope\":%s}}\\n' \"$1\" \"$ORBIT_PLUGIN\" \"$ORBIT_ALLOWED_TOOLS\" \"$ORBIT_PROC_ALLOWED_PROGRAMS\" \"$input\"\n";

fn orbit_tools_permissions() -> PluginPermissions {
    PluginPermissions {
        orbit_tools: vec!["orbit.task.show".into(), "orbit.search".into()],
        ..PluginPermissions::default()
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn exec_backend_receives_the_envelope_and_returns_output() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(temp.path(), ECHO_BACKEND);
    let mut backend = (*spec(
        command,
        temp.path(),
        orbit_tools_permissions(),
        &[PluginGrant::OrbitTools],
    ))
    .clone();
    backend.config = PluginConfigSection::new(json!({
        "index_dir": "/srv/graph",
        "max_nodes": 500,
    }));
    let tool = tool(std::sync::Arc::new(backend), None);
    assert_eq!(tool.execution_kind(), ToolExecutionKind::ReadOnly);
    let ctx = ToolContext {
        allowed_tools: vec!["orbit.task.show".into(), "demo.hello".into()],
        ..context(temp.path())
    };
    let output = tool
        .execute(&ctx, json!({ "name": "world" }))
        .expect("backend succeeds");
    assert_eq!(output["arg"], "--serve");
    assert_eq!(output["plugin"], "demo");
    // Requested ∩ granted ∩ the caller's own allowlist.
    assert_eq!(output["allowed"], "orbit.task.show");
    assert_eq!(output["envelope"]["schema_version"], 1);
    assert_eq!(output["envelope"]["tool"], "demo.hello");
    assert_eq!(output["envelope"]["input"]["name"], "world");
    // What the plugin is configured with reaches the process itself, typed,
    // rather than only the `{{config.<key>}}` slots the manifest declared.
    assert_eq!(
        output["envelope"]["context"]["config"],
        json!({ "index_dir": "/srv/graph", "max_nodes": 500 })
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn the_callback_allowlist_is_exactly_the_granted_orbit_tools() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(temp.path(), ECHO_BACKEND);

    // Granted, no caller allowlist: every requested tool.
    let granted = tool(
        spec(
            command.clone(),
            temp.path(),
            orbit_tools_permissions(),
            &[PluginGrant::OrbitTools],
        ),
        None,
    );
    let output = granted
        .execute(&context(temp.path()), json!({}))
        .expect("backend succeeds");
    assert_eq!(output["allowed"], "orbit.task.show,orbit.search");

    // Requested but not granted: the variable is present and empty, so the
    // child's `orbit tool run` refuses everything.
    let ungranted = tool(
        spec(command, temp.path(), orbit_tools_permissions(), &[]),
        None,
    );
    let output = ungranted
        .execute(&context(temp.path()), json!({}))
        .expect("backend succeeds");
    assert_eq!(output["allowed"], "");
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn exec_backend_failures_are_tool_errors_with_no_partial_output() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let permissions = PluginPermissions::default();

    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":false,\"error\":{\"code\":\"nope\",\"message\":\"declined\"}}\\n'\n",
    );
    let error = tool(spec(command, temp.path(), permissions.clone(), &[]), None)
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("nope") && error.contains("declined"),
        "{error}"
    );

    let command = stub_backend(temp.path(), "#!/bin/sh\ncat >/dev/null\necho not-json\n");
    let error = tool(spec(command, temp.path(), permissions.clone(), &[]), None)
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid JSON output"), "{error}");

    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\necho boom >&2\nexit 3\n",
    );
    let error = tool(spec(command, temp.path(), permissions.clone(), &[]), None)
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("exited with 3") && error.contains("boom"),
        "{error}"
    );

    // A response that parses but violates `output_schema` never reaches the
    // caller either.
    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"count\":\"three\"}}\\n'\n",
    );
    let schema = json!({
        "type": "object",
        "required": ["count"],
        "properties": { "count": { "type": "integer" } }
    });
    let error = tool(
        spec(command.clone(), temp.path(), permissions.clone(), &[]),
        Some(schema.clone()),
    )
    .execute(&context(temp.path()), json!({}))
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("violates its output_schema") && error.contains("count"),
        "{error}"
    );
    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"count\":3}}\\n'\n",
    );
    let output = tool(spec(command, temp.path(), permissions, &[]), Some(schema))
        .execute(&context(temp.path()), json!({}))
        .expect("valid output passes the schema");
    assert_eq!(output["count"], 3);
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn an_exec_backend_that_does_not_answer_is_killed_at_its_timeout() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(temp.path(), "#!/bin/sh\ncat >/dev/null\nsleep 30\n");
    let mut backend = (*spec(command, temp.path(), PluginPermissions::default(), &[])).clone();
    backend.timeout_ms = Some(100);
    let started = Instant::now();
    let error = tool(std::sync::Arc::new(backend), None)
        .execute(&context(temp.path()), json!({}))
        .expect_err("the backend must time out")
        .to_string();
    let elapsed = started.elapsed();

    assert!(error.contains("timed out after 100 ms"), "{error}");
    assert!(
        elapsed >= Duration::from_millis(100) && elapsed < Duration::from_secs(3),
        "the configured timeout is the execution bound: {elapsed:?}"
    );
}

/// The value an `exec` backend is handed, and the one string its script
/// searches its own environment and argv for.
const EXEC_SECRET: &str = "exec-secret-value-9c2f";

/// A plugin's declared secret reaches its `exec` backend on stdin, under
/// `context.secrets`, and nowhere the process could leak it by being
/// inspected: not its environment, not its argv.
#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn an_exec_backend_receives_its_secret_on_stdin_and_not_in_env_or_argv() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(
        temp.path(),
        &format!(
            "#!/bin/sh\ninput=$(cat)\nenv_hits=$(env | grep -c '{EXEC_SECRET}' || true)\n\
             argv_hits=$(printf '%s\\n' \"$0\" \"$@\" | grep -c '{EXEC_SECRET}' || true)\n\
             printf '{{\"ok\":true,\"output\":{{\"env_hits\":%s,\"argv_hits\":%s,\"envelope\":%s}}}}\\n' \
             \"$env_hits\" \"$argv_hits\" \"$input\"\n"
        ),
    );
    let mut backend = (*spec(command, temp.path(), PluginPermissions::default(), &[])).clone();
    backend.secrets = PluginSecretDelivery::new(
        vec!["api_token".to_string(), "unset_token".to_string()],
        Arc::new(FixedSource),
    );

    let output = tool(Arc::new(backend), None)
        .execute(&context(temp.path()), json!({}))
        .expect("backend succeeds");

    assert_eq!(
        output["envelope"]["context"]["secrets"],
        json!({ "api_token": { "value": EXEC_SECRET, "version": "v7" } }),
        "the declared, set secret rides stdin with its version; the unset one is omitted"
    );
    assert_eq!(output["env_hits"], 0, "the value is not in the environment");
    assert_eq!(output["argv_hits"], 0, "the value is not in argv");
}

struct FixedSource;

impl PluginSecretSource for FixedSource {
    fn read(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, DeliveredPluginSecret>, orbit_common::OrbitError> {
        Ok(names
            .iter()
            .filter(|name| *name == "api_token")
            .map(|name| {
                (
                    name.clone(),
                    DeliveredPluginSecret {
                        value: EXEC_SECRET.to_string(),
                        version: "v7".to_string(),
                    },
                )
            })
            .collect())
    }
}

/// An `exec` backend that rotates `refresh_token` from the version it was
/// delivered, to a value unique to its process. Input `{"fail": true}` makes
/// it report a failure after rotating — the refresh that succeeded before
/// the call it was for did not.
const ROTATING_BACKEND: &str = r##"#!/bin/sh
input=$(cat)
version=$(printf '%s' "$input" | sed -n 's/.*"refresh_token":{"value":"[^"]*","version":"\([^"]*\)".*/\1/p')
updates="{\"refresh_token\":{\"value\":\"rotated-token-$$\",\"expected_version\":\"$version\"}}"
case "$input" in
  *'"fail":true'*) printf '{"ok":false,"error":{"code":"upstream","message":"post failed"},"secret_updates":%s}\n' "$updates" ;;
  *) printf '{"ok":true,"output":{"delivered":"%s"},"secret_updates":%s}\n' "$version" "$updates" ;;
esac
"##;

fn rotating_tool(temp: &Path, source: Arc<dyn PluginSecretSource>) -> PluginTool {
    let command = stub_backend(temp, ROTATING_BACKEND);
    let mut backend = (*spec(command, temp, PluginPermissions::default(), &[])).clone();
    backend.secrets = PluginSecretDelivery::new(vec!["refresh_token".to_string()], source)
        .with_rotatable(vec!["refresh_token".to_string()]);
    tool(Arc::new(backend), None)
}

/// A rotation an `exec` backend returns beside `ok`/`output` is stored, the
/// call's output is returned as-is, and the next call is delivered the new
/// value at its new version. A rotation beside a reported failure is stored
/// too, and the failure is still the call's result.
#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn an_exec_backend_rotates_a_secret_beside_its_answer() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let source = CasSource::holding(&[("refresh_token", "first-token-a1", "v1")]);
    let tool = rotating_tool(
        temp.path(),
        Arc::clone(&source) as Arc<dyn PluginSecretSource>,
    );
    let ctx = context(temp.path());

    let _ = take_plugin_secret_updates();
    let output = tool.execute(&ctx, json!({})).expect("call succeeds");
    assert_eq!(
        output,
        json!({ "delivered": "v1" }),
        "the output is unchanged"
    );
    assert_eq!(
        take_plugin_secret_updates(),
        BTreeMap::from([(
            "refresh_token".to_string(),
            PluginSecretUpdateStatus::Applied
        )])
    );
    let (value, version) = source.stored("refresh_token").expect("stored");
    assert!(value.starts_with("rotated-token-"), "{value}");

    let next = tool.execute(&ctx, json!({})).expect("next call");
    assert_eq!(
        next,
        json!({ "delivered": version }),
        "the next call sees it"
    );

    let (before, _) = source.stored("refresh_token").expect("stored");
    let error = tool
        .execute(&ctx, json!({ "fail": true }))
        .expect_err("the backend reported a failure");
    assert!(error.to_string().contains("post failed"), "{error}");
    assert_eq!(
        take_plugin_secret_updates()["refresh_token"],
        PluginSecretUpdateStatus::Applied
    );
    assert_ne!(
        source.stored("refresh_token").expect("stored").0,
        before,
        "a token refreshed before the failure is not lost with it"
    );
}

/// Holds every read until both racing calls have one, so both are delivered
/// the same version before either backend answers.
struct Gate {
    inner: Arc<CasSource>,
    barrier: std::sync::Barrier,
}

impl PluginSecretSource for Gate {
    fn read(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, DeliveredPluginSecret>, orbit_common::OrbitError> {
        let delivered = self.inner.read(names);
        self.barrier.wait();
        delivered
    }

    fn compare_and_swap(
        &self,
        name: &str,
        value: &str,
        expected_version: Option<&str>,
    ) -> Result<PluginSecretRotation, orbit_common::OrbitError> {
        self.inner.compare_and_swap(name, value, expected_version)
    }
}

/// Two concurrent calls rotating from the same version: exactly one update
/// is applied, the other is refused, and both calls still return their
/// output.
#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn concurrent_exec_rotations_from_one_version_apply_exactly_one() {
    require_sandbox();
    let temp = tempfile::tempdir().expect("tempdir");
    let store = CasSource::holding(&[("refresh_token", "first-token-a1", "v1")]);
    let gate = Arc::new(Gate {
        inner: Arc::clone(&store),
        barrier: std::sync::Barrier::new(2),
    });
    let tool = Arc::new(rotating_tool(temp.path(), gate));

    let racers: Vec<_> = (0..2)
        .map(|_| {
            let tool = Arc::clone(&tool);
            let ctx = context(temp.path());
            std::thread::spawn(move || {
                let output = tool.execute(&ctx, json!({}));
                (output, take_plugin_secret_updates())
            })
        })
        .collect();
    let results: Vec<_> = racers
        .into_iter()
        .map(|racer| racer.join().expect("racer"))
        .collect();

    let mut statuses = Vec::new();
    for (output, updates) in &results {
        let output = output.as_ref().expect("both calls return their output");
        assert_eq!(output, &json!({ "delivered": "v1" }));
        statuses.push(updates["refresh_token"]);
    }
    statuses.sort_by_key(|status| *status == PluginSecretUpdateStatus::Refused);
    assert_eq!(
        statuses,
        vec![
            PluginSecretUpdateStatus::Applied,
            PluginSecretUpdateStatus::Refused
        ]
    );
    assert_ne!(store.stored("refresh_token").expect("stored").1, "v1");
}
