use orbit_engine::RuntimeHost;
use orbit_types::workflow::{ExecutorDef, ExecutorType};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::seed_executor;

#[test]
fn cli_executor_resolution_preserves_registered_static_args() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_executor(&runtime, "codex", None);

    let resolved = runtime
        .resolve_cli_executor("codex")
        .expect("resolve codex executor");

    assert_eq!(resolved.command, "codex");
    assert_eq!(resolved.args, ["exec", "--json"]);
}

/// Table-driven coverage of executor selection through the centralized
/// `Provider` surface (ORB-10091): every provider that ships a CLI runtime
/// resolves to a command named after its canonical id; a provider that
/// parses but is HTTP-only (`openai_compat`) and a provider that does not
/// parse both fail with a stable diagnostic and never fall back.
#[test]
fn cli_executor_selection_diagnostics_table() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    for provider in ["claude", "codex", "gemini", "grok"] {
        let resolved = runtime
            .resolve_cli_executor(provider)
            .unwrap_or_else(|err| panic!("{provider} should resolve: {err:?}"));
        assert_eq!(resolved.command, provider, "command for {provider}");
        assert!(resolved.args.is_empty(), "fallback args for {provider}");
    }

    for unsupported in ["ollama", "openai_compat"] {
        let error = runtime
            .resolve_cli_executor(unsupported)
            .expect_err("unsupported provider must not fall back to CLI");
        assert!(
            format!("{error:?}").contains("unsupported by the Orbit CLI entry point"),
            "unsupported diagnostic for {unsupported}: {error:?}"
        );
    }

    let unknown = runtime
        .resolve_cli_executor("bogus_provider")
        .expect_err("unknown provider must not resolve to a default runtime");
    let msg = format!("{unknown:?}");
    assert!(
        msg.contains("unknown provider"),
        "unknown diagnostic: {msg}"
    );
    assert!(
        msg.contains("no CLI runtime registered"),
        "unknown diagnostic: {msg}"
    );
}

/// A `local_shell` step resolves its own executor family. The two resolution
/// boundaries never cross: an agent provider cannot be reached through the
/// shell boundary, and a shell definition cannot be reached through the agent
/// boundary. [ORB-11294]
#[test]
fn shell_and_agent_executor_resolution_stay_separate() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_local_shell_executor(&runtime, "local-shell", ExecutorType::LocalShell);
    seed_executor(&runtime, "codex", None);

    let resolved = runtime
        .resolve_local_shell_executor("local-shell")
        .expect("resolve shipped local-shell executor");
    assert_eq!(resolved.command.as_deref(), Some("/bin/sh"));
    assert_eq!(resolved.args, ["-eu"]);
    assert_eq!(resolved.timeout_seconds, Some(120));
    assert_eq!(
        resolved.env.get("ORBIT_SHELL_MARKER").map(String::as_str),
        Some("set")
    );

    let agent_through_shell = runtime
        .resolve_local_shell_executor("codex")
        .expect_err("an agent executor must not back a shell step");
    assert!(
        format!("{agent_through_shell:?}").contains("requires a local_shell executor"),
        "{agent_through_shell:?}"
    );

    let shell_through_agent = runtime
        .resolve_cli_executor("local-shell")
        .expect_err("a shell executor must not back an agent activity");
    assert!(
        format!("{shell_through_agent:?}").contains("requires a direct_agent or agent_cli"),
        "{shell_through_agent:?}"
    );
}

/// A definition persisted before the rename spells the family `cli_command`.
/// It must still load, and its `command` / `args` / `env` must still act as the
/// step defaults. [ORB-11294]
#[test]
fn legacy_cli_command_definitions_still_back_shell_steps() {
    let legacy: ExecutorType =
        serde_yaml::from_str("cli_command").expect("legacy executor_type still deserializes");
    assert_eq!(legacy, ExecutorType::LocalShell);

    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_local_shell_executor(&runtime, "legacy-shell", legacy);

    let resolved = runtime
        .resolve_local_shell_executor("legacy-shell")
        .expect("legacy definition backs a shell step");
    assert_eq!(resolved.command.as_deref(), Some("/bin/sh"));
}

#[test]
fn an_unregistered_shell_executor_is_named_in_the_diagnostic() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let error = runtime
        .resolve_local_shell_executor("no-such-shell")
        .expect_err("unregistered executor must not resolve to a default");
    assert!(format!("{error:?}").contains("no-such-shell"), "{error:?}");
}

fn seed_local_shell_executor(runtime: &OrbitRuntime, name: &str, executor_type: ExecutorType) {
    let now = chrono::Utc::now();
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: name.to_string(),
            executor_type,
            command: Some("/bin/sh".to_string()),
            args: vec!["-eu".to_string()],
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: Some(120),
            env: std::collections::HashMap::from([(
                "ORBIT_SHELL_MARKER".to_string(),
                "set".to_string(),
            )]),
            sandbox: None,
            allow_fallback: false,
            created_at: Some(now),
            updated_at: Some(now),
        })
        .expect("seed local shell executor");
}
