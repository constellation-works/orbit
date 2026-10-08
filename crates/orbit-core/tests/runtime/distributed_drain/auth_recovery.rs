//! Auth recovery through the real pull action, owner claims and CLI runner.
//! The fixture advances worker bookkeeping because this test binary cannot
//! re-exec Orbit workers; provider invocations themselves are real subprocesses.

use super::*;
use orbit_engine::{DispatchOutcome, V2AuditWriter};
use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};
use orbit_types::workflow::{AuthProbe, AuthProbeSuccess};

const CONFIG: &str = r#"
[workflow]
default_crew = "opus"
[crews.opus]
provider = "claude"
model = "claude-opus"
[crews.sonnet]
provider = "anthropic"
model = "claude-sonnet"
[execution.env]
pass = ["HOME", "PATH", "AUTH_FIXTURE_ALLOWED", "CLAUDE_CODE_OAUTH_TOKEN"]
"#;

fn provider(pair: &Pair, probe: bool, timeout: u64) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = pair.follower_repo.join("claude");
    std::fs::write(&bin, r#"#!/bin/sh
[ "$AUTH_FIXTURE_ALLOWED" = permitted ] || exit 21
[ -z "$AUTH_FIXTURE_NOT_ALLOWED" ] || exit 22
[ "$CLAUDE_CODE_DISABLE_BACKGROUND_TASKS" = 1 ] || exit 23
if [ "$1" = auth-recovery-probe ]; then
    [ -z "$ORBIT_ACTIVITY_TOOLS" ] || exit 24
    [ -z "$ORBIT_TASK_ID" ] || exit 25
    printf 'probe\n' >> probe-calls
    cat > probe-stdin
    [ ! -f probe-hang ] || sleep 30
    [ -f auth-ok ] || exit 1
    printf 'ORBIT_AUTH_OK\n'
else
    printf 'leaf\n' >> leaf-calls
    cat > leaf-stdin
    if [ ! -f auth-ok ]; then
        printf 'Authentication Error: HTTP 401 OAuth token revoked\n' >&2
        exit 1
    fi
    printf '%s\n' '{"type":"result","is_error":false,"result":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"ran\":true},\"error\":null}"}'
fi
"#).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    pair.follower_cli("claude", bin.to_str().unwrap());
    let mut def = pair.follower.get_executor_def("claude").unwrap().unwrap();
    def.auth_probe = probe.then(|| AuthProbe {
        args: vec!["auth-recovery-probe".into()],
        stdin: "Reply with exactly ORBIT_AUTH_OK.".into(),
        timeout_seconds: timeout,
        success: AuthProbeSuccess::StdoutContains {
            text: "ORBIT_AUTH_OK".into(),
        },
        relogin_hint: "Run `claude login`.".into(),
    });
    pair.follower.upsert_executor_def(&def).unwrap();
    bin
}

fn run_leaf(pair: &Pair, leaf: &str) -> Result<DispatchOutcome, orbit_engine::DispatchError> {
    let audit_dir = TempDir::new().unwrap();
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_test",
        leaf,
        "claude:fixture",
        None,
    )
    .unwrap();
    let spec = AgentLoopSpec {
        instruction: "Return a success envelope.".into(),
        tools: vec![],
        tool_disallow_list: None,
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Claude,
        wall_clock_timeout_seconds: 10,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
    };
    orbit_engine::activity_job::cli_runner::run_cli_backend(
        &pair.follower,
        &spec,
        "auth_fixture_leaf",
        leaf,
        audit,
        &json!({"workspace_path": pair.follower_repo}),
        None,
    )
}

fn due(pair: &Pair, drain: &str) {
    let mut state = pair.follower.read_run_state(drain).unwrap().unwrap();
    assert!(!state.pull_auth_recovery.is_empty());
    for incident in state.pull_auth_recovery.values_mut() {
        if incident.recovered_at.is_none() {
            incident.exclusion.next_probe_at = Some(Utc::now() - chrono::Duration::seconds(1));
        }
    }
    pair.follower.write_run_state(drain, &state).unwrap();
}

#[test]
fn auth_probe_backoff_recovery_and_later_leaf_run_in_the_same_drain() {
    if !isolated(
        module_path!(),
        "auth_probe_backoff_recovery_and_later_leaf_run_in_the_same_drain",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        ("AUTH_FIXTURE_ALLOWED", Some("permitted")),
        ("AUTH_FIXTURE_NOT_ALLOWED", Some("private")),
        ("CLAUDE_CODE_OAUTH_TOKEN", Some("fixture-oauth-credential")),
        ("ORBIT_ACTIVITY_TOOLS", Some("orbit.task.update")),
        ("ORBIT_TASK_ID", Some("parent-task")),
    ]);
    let pair = Pair::with_configs(
        CONFIG,
        CONFIG,
        &[Some("opus"), Some("sonnet"), Some("opus")],
    );
    provider(&pair, true, 5);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert!(
        !pair.follower_repo.join("probe-calls").exists(),
        "healthy crews never probe"
    );
    let failure = match run_leaf(&pair, &leaf) {
        Err(error) => error.to_string(),
        Ok(outcome) => {
            assert!(!outcome.success);
            outcome.message.unwrap()
        }
    };
    assert!(
        failure.contains("provider_unavailable"),
        "actual provider classification: {failure}"
    );
    pair.leaf_fails_with(&leaf, &failure);
    let pass = pair.pass(&drain);
    for crew in ["opus", "sonnet"] {
        assert!(
            pass["crews"]["excluded"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["crew"] == crew)
        );
    }
    let first = &pass["crews"]["auth_exclusions"][0];
    assert_eq!(first["provider"], "claude");
    assert_eq!(first["host"], FOLLOWER);
    assert!(
        first["credential_source"]
            .as_str()
            .unwrap()
            .contains("CLAUDE_CODE_OAUTH_TOKEN")
    );
    assert!(first["next_probe_at"].is_string());
    assert!(
        !pair.follower_repo.join("probe-calls").exists(),
        "first delay is respected"
    );

    // Persist initial incidents without waiting in real time. Exercise due
    // timestamps in the same state document the production refill reads.
    pair.pass(&drain);
    due(&pair, &drain);
    let failed = pair.pass(&drain);
    assert_eq!(failed["admitted"], 0);
    let incident = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_auth_recovery
        .into_values()
        .next()
        .unwrap();
    assert!(incident.recovered_at.is_none());
    assert_eq!(incident.exclusion.attempts, 1);
    assert!(incident.exclusion.next_probe_at.unwrap() > Utc::now());
    let first_wait = incident.exclusion.next_probe_at.unwrap() - Utc::now();
    pair.pass(&drain);
    assert_eq!(
        std::fs::read_to_string(pair.follower_repo.join("probe-calls"))
            .unwrap()
            .lines()
            .count(),
        1,
        "backoff prevents per-refill calls"
    );
    due(&pair, &drain);
    pair.pass(&drain);
    let second = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_auth_recovery
        .into_values()
        .next()
        .unwrap();
    assert!(
        second.exclusion.next_probe_at.unwrap() - Utc::now() > first_wait,
        "failed probes back off"
    );
    assert_eq!(second.exclusion.attempts, 2);

    std::fs::write(pair.follower_repo.join("auth-ok"), "ready").unwrap();
    due(&pair, &drain);
    // Stop after real owner claim/bind; this fixture then owns the worker and
    // runs the recovered crew's provider through the actual CLI runner.
    pair.wire.lose_next_reply("orbit.drain.claim.bind");
    let before = pair.leaf_runs();
    let recovered = pair.pass(&drain);
    assert!(
        recovered["crews"]["auth_exclusions"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{recovered}"
    );
    let next = pair
        .leaf_runs()
        .into_iter()
        .find(|id| !before.contains(id))
        .unwrap();
    assert_eq!(pair.claimed_task(&next), pair.tasks[1]);
    pair.advance(&next, LocalPullMutation::Bound);
    pair.advance(&next, LocalPullMutation::LaunchIntent);
    pair.follower_jobs
        .mark_job_run_running(&next, Utc::now(), std::process::id())
        .unwrap();
    pair.advance(&next, LocalPullMutation::Launched);
    assert!(
        run_leaf(&pair, &next).unwrap().success,
        "later claimed leaf runs after re-login"
    );
    assert_eq!(
        std::fs::read_to_string(pair.follower_repo.join("leaf-calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(pair.follower_repo.join("probe-stdin")).unwrap(),
        "Reply with exactly ORBIT_AUTH_OK."
    );
    let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    let recovery = state.pull_auth_recovery.values().next().unwrap();
    assert!(recovery.recovered_at.is_some());
    assert_eq!(recovery.recovery_source.as_deref(), Some("auth_probe"));
    assert!(
        pair.follower
            .pull_drain_crew_window(&drain)
            .unwrap()
            .unwrap()
            .auth_exclusions
            .is_empty(),
        "historical released incidents stay acknowledged"
    );
    // A new failure is a distinct incident and must re-exclude the provider.
    pair.leaf_fails_with(
        &next,
        "[provider_unavailable] claude provider authentication failure (HTTP 401)",
    );
    let again = pair.pass(&drain);
    assert_eq!(again["crews"]["auth_exclusions"][0]["attempts"], 0);
    assert!(again["crews"]["auth_exclusions"][0]["next_probe_at"].is_string());
}

#[test]
fn only_declared_auth_exclusions_probe_and_timeout_keeps_them_excluded() {
    if !isolated(
        module_path!(),
        "only_declared_auth_exclusions_probe_and_timeout_keeps_them_excluded",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        ("AUTH_FIXTURE_ALLOWED", Some("permitted")),
        ("AUTH_FIXTURE_NOT_ALLOWED", Some("private")),
    ]);
    for (diagnostic, declared, expect_probe) in [
        (
            "[provider_capacity] claude selected model is at capacity",
            true,
            false,
        ),
        (
            "[provider_refusal] claude refused this request",
            true,
            false,
        ),
        (
            "[provider_unavailable] claude authentication failure (HTTP 401)",
            false,
            false,
        ),
        (
            "[provider_unavailable] claude authentication failure (HTTP 401)",
            true,
            true,
        ),
    ] {
        let pair = Pair::with_configs(CONFIG, CONFIG, &[Some("opus")]);
        provider(&pair, declared, 1);
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        pair.leaf_fails_with(&leaf, diagnostic);
        pair.pass(&drain);
        pair.pass(&drain);
        if expect_probe {
            due(&pair, &drain);
            std::fs::write(pair.follower_repo.join("probe-hang"), "hang").unwrap();
        }
        let started = std::time::Instant::now();
        pair.pass(&drain);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "declared timeout bounds a hung provider"
        );
        assert_eq!(
            pair.follower_repo.join("probe-calls").exists(),
            expect_probe,
            "{diagnostic}"
        );
        if expect_probe {
            let window = pair
                .follower
                .pull_drain_crew_window(&drain)
                .unwrap()
                .unwrap();
            assert_eq!(window.auth_exclusions.len(), 1);
            assert_eq!(window.auth_exclusions[0].attempts, 1);
            assert!(window.auth_exclusions[0].next_probe_at.unwrap() > Utc::now());
        }
    }
}
