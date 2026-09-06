//! Sibling tests for `crew.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::workflow::activity_job::{Provider, ProviderSource};
use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::super::crew::select_crew_name;
use crate::OrbitRuntime;
use crate::application::task::TaskAddParams;

const CONSTELLATION_DEFAULT_PROVIDER_ENV: &str = "CONSTELLATION_DEFAULT_PROVIDER";

fn runtime_with_named_crews() -> (TempDir, OrbitRuntime) {
    let root = tempdir().expect("create temp root");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[crews.primary]
model = "default-model"
provider = "codex"
backend = "cli"

[crews.beta]
model = "beta-model"
provider = "codex"
backend = "cli"

[crews.gamma]
model = "gamma-model"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "primary"
"#,
    )
    .expect("write test config");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    (root, runtime)
}

#[test]
fn crew_discovery_projects_the_open_runtime_configuration() {
    let (_root, runtime) = runtime_with_named_crews();

    let discovery = runtime
        .crew_discovery("ws_example", Some("hm_owner".to_string()))
        .expect("crew discovery");

    assert_eq!(discovery.workspace_id, "ws_example");
    assert_eq!(discovery.owner_machine_id.as_deref(), Some("hm_owner"));
    assert_eq!(discovery.default_crew.as_deref(), Some("primary"));
    assert_eq!(
        discovery
            .crews
            .iter()
            .map(|crew| crew.name.as_str())
            .collect::<Vec<_>>(),
        ["beta", "gamma", "primary", "system"]
    );
    let system = discovery
        .crews
        .iter()
        .find(|crew| crew.name == "system")
        .expect("legacy config gains the portable system alias");
    assert_eq!(system.provider, "codex");
    assert_eq!(system.model, "default-model");
}

fn add_task_with_crew(runtime: &OrbitRuntime, crew: &str) -> String {
    add_task(runtime, Some(crew))
}

fn add_task(runtime: &OrbitRuntime, crew: Option<&str>) -> String {
    runtime
        .add_task(TaskAddParams {
            title: format!("{} task", crew.unwrap_or("default")),
            description: "Task fixture for crew resolution.".to_string(),
            crew: crew.map(ToOwned::to_owned),
            ..Default::default()
        })
        .expect("add task")
        .id
}

fn try_set_task_prefix(runtime: &OrbitRuntime, prefix: &str) {
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root()))
        .expect("open task registry");
    // Prefix is immutable after the first allocation. Tests that already
    // created a task keep the host prefix; the lookup path is prefix-agnostic.
    let _ = registry.set_task_prefix(prefix);
}

#[test]
fn run_input_task_ids_singleton_resolves_task_crew() {
    let (_root, runtime) = runtime_with_named_crews();
    let task_id = add_task_with_crew(&runtime, "beta");

    let crew = runtime
        .resolve_crew_for_run_input(&json!({ "task_ids": [task_id] }))
        .expect("resolve crew");

    assert_eq!(crew.name, "beta");
    assert_eq!(crew.assignment.model, "beta-model");
    assert_eq!(crew.assignment.provider, "codex");
}

#[test]
fn record_run_crew_persists_singleton_task_ids_task_crew_models() {
    let (_root, runtime) = runtime_with_named_crews();
    let task_id = add_task_with_crew(&runtime, "beta");
    let input = json!({ "task_ids": [task_id] });
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("agent_implement", 1, Utc::now(), Some(input.clone()), None)
        .expect("insert run");

    let crew = runtime
        .record_run_crew_from_input(&run.run_id, &input)
        .expect("record crew");
    let stored = runtime.show_job_run(&run.run_id).expect("show stored run");

    assert_eq!(crew.name, "beta");
    assert_eq!(stored.resolved_crew.as_deref(), Some("beta"));
    assert_eq!(stored.crew_model.as_deref(), Some("beta-model"));
}

#[test]
fn explicit_crew_override_wins_over_singleton_task_ids_task_crew() {
    let (_root, runtime) = runtime_with_named_crews();
    let task_id = add_task_with_crew(&runtime, "beta");

    let crew = runtime
        .resolve_crew_for_run_input(&json!({
            "crew": "gamma",
            "task_ids": [task_id]
        }))
        .expect("resolve crew");

    assert_eq!(crew.name, "gamma");
    assert_eq!(crew.assignment.model, "gamma-model");
}

#[test]
fn custom_prefix_task_ids_resolve_task_crew() {
    let (_root, runtime) = runtime_with_named_crews();
    try_set_task_prefix(&runtime, "DAN");
    let task_id = add_task_with_crew(&runtime, "beta");

    let crew = runtime
        .resolve_crew_for_run_input(&json!({ "task_ids": [task_id] }))
        .expect("resolve crew");

    assert_eq!(crew.name, "beta");
    assert_eq!(crew.assignment.model, "beta-model");
}

#[test]
fn missing_task_ids_fall_back_to_default_crew() {
    let (_root, runtime) = runtime_with_named_crews();

    let crew = runtime
        .resolve_crew_for_run_input(&json!({
            "task_ids": ["DAN-10002", "T-crew"]
        }))
        .expect("missing fixture ids must not fail crew resolution");

    assert_eq!(crew.name, "primary");
    assert_eq!(crew.assignment.model, "default-model");
}

#[test]
fn multi_task_ids_with_the_same_crew_use_that_crew() {
    let (_root, runtime) = runtime_with_named_crews();
    let first = add_task_with_crew(&runtime, "beta");
    let second = add_task_with_crew(&runtime, "beta");

    let crew = runtime
        .resolve_crew_for_run_input(&json!({
            "task_ids": [first, second]
        }))
        .expect("unanimous task crew");

    assert_eq!(crew.name, "beta");
    assert_eq!(crew.assignment.model, "beta-model");
}

#[test]
fn multi_task_ids_without_crew_fall_back_to_default_crew() {
    let (_root, runtime) = runtime_with_named_crews();
    let first = add_task(&runtime, None);
    let second = add_task(&runtime, None);

    let crew = runtime
        .resolve_crew_for_run_input(&json!({
            "task_ids": [first, second]
        }))
        .expect("unset task crews use the workspace default");

    assert_eq!(crew.name, "primary");
    assert_eq!(crew.assignment.model, "default-model");
}

#[test]
fn mixed_task_crews_fail_closed_instead_of_inheriting_default() {
    let (_root, runtime) = runtime_with_named_crews();
    let beta_task_id = add_task_with_crew(&runtime, "beta");
    let gamma_task_id = add_task_with_crew(&runtime, "gamma");

    let error = runtime
        .resolve_crew_for_run_input(&json!({
            "task_ids": [beta_task_id, gamma_task_id]
        }))
        .expect_err("mixed crews must fail closed");

    let message = error.to_string();
    assert!(
        message.contains("mixes crews") && message.contains("workflow.default_crew"),
        "unexpected mixed-crew error: {message}"
    );
}

#[test]
fn mixed_set_and_unset_task_crews_fail_closed() {
    let (_root, runtime) = runtime_with_named_crews();
    let assigned = add_task_with_crew(&runtime, "beta");
    let unset = add_task(&runtime, None);

    let error = runtime
        .resolve_crew_for_run_input(&json!({
            "task_ids": [assigned, unset]
        }))
        .expect_err("set vs unset is a mixed bundle");

    let message = error.to_string();
    assert!(
        message.contains("mixes crews"),
        "unexpected mixed-crew error: {message}"
    );
}

#[test]
fn implementer_dispatch_uses_task_crew_not_default_crew() {
    let (_root, runtime) = runtime_with_named_crews();
    try_set_task_prefix(&runtime, "DAN");
    let task_id = add_task_with_crew(&runtime, "beta");
    let run_input = json!({ "task_ids": [task_id] });
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(run_input.clone()),
            None,
        )
        .expect("insert implement_one parent run");

    let persisted = runtime
        .record_run_crew_from_input(&run.run_id, &run_input)
        .expect("persist resolved crew");
    let stored = runtime.show_job_run(&run.run_id).expect("show child run");
    assert_eq!(persisted.name, "beta");
    assert_eq!(stored.resolved_crew.as_deref(), Some("beta"));
    assert_eq!(stored.crew_model.as_deref(), Some("beta-model"));

    // implement_one has no activity `crew`; dispatch re-resolves from run input.
    let config = RuntimeHost::agent_crew_config_for_input(&runtime, &run_input)
        .expect("implementer dispatch")
        .expect("crew config");
    assert_eq!(config.model.as_deref(), Some("beta-model"));
    assert_eq!(config.provider, Some(Provider::Codex));
    assert_ne!(config.model.as_deref(), Some("default-model"));
}

#[test]
fn implementer_dispatch_without_task_crew_uses_default_crew() {
    let (_root, runtime) = runtime_with_named_crews();
    let task_id = add_task(&runtime, None);
    let config =
        RuntimeHost::agent_crew_config_for_input(&runtime, &json!({ "task_ids": [task_id] }))
            .expect("implementer dispatch")
            .expect("crew config");
    assert_eq!(config.model.as_deref(), Some("default-model"));
    assert_eq!(config.provider, Some(Provider::Codex));
}

/// Table-driven proof of the crew-selection precedence (ORB-10091, contract §3)
/// and of the environment-default tier. Each row fixes the higher tiers and
/// varies the `env` column (`CONSTELLATION_DEFAULT_PROVIDER`): a row that has an
/// explicit / task / workspace choice keeps its result regardless of `env`,
/// while the otherwise-defaulted rows flip when the single env setting changes.
#[test]
fn select_crew_name_precedence_and_environment_default() {
    struct Row {
        name: &'static str,
        explicit: Option<&'static str>,
        task: Option<&'static str>,
        workspace: Option<&'static str>,
        env: Option<&'static str>,
        system: Option<&'static str>,
        expect: Option<(&'static str, ProviderSource)>,
    }

    let rows = [
        // Explicit wins over every lower tier, including a set env.
        Row {
            name: "explicit beats all (env ignored)",
            explicit: Some("gamma"),
            task: Some("beta"),
            workspace: Some("primary"),
            env: Some("gemini"),
            system: Some("claude"),
            expect: Some(("gamma", ProviderSource::Explicit)),
        },
        // Task config wins over workspace + env when there is no explicit choice.
        Row {
            name: "task beats workspace+env",
            explicit: None,
            task: Some("beta"),
            workspace: Some("primary"),
            env: Some("gemini"),
            system: Some("claude"),
            expect: Some(("beta", ProviderSource::TaskConfig)),
        },
        // A configured workspace default is never overridden by the env lever.
        Row {
            name: "workspace beats env (unchanged by env)",
            explicit: None,
            task: None,
            workspace: Some("primary"),
            env: Some("gemini"),
            system: Some("claude"),
            expect: Some(("primary", ProviderSource::WorkspaceDefault)),
        },
        // Otherwise-defaulted: the env setting is the selection...
        Row {
            name: "env applies when otherwise-defaulted (claude)",
            explicit: None,
            task: None,
            workspace: None,
            env: Some("claude"),
            system: Some("claude"),
            expect: Some(("claude", ProviderSource::EnvironmentDefault)),
        },
        // ...and changing that one setting changes the resolved crew.
        Row {
            name: "env applies when otherwise-defaulted (gemini)",
            explicit: None,
            task: None,
            workspace: None,
            env: Some("gemini"),
            system: Some("claude"),
            expect: Some(("gemini", ProviderSource::EnvironmentDefault)),
        },
        // No env set: fall through to the system default.
        Row {
            name: "system default when nothing else",
            explicit: None,
            task: None,
            workspace: None,
            env: None,
            system: Some("claude"),
            expect: Some(("claude", ProviderSource::SystemDefault)),
        },
        // Whitespace at a tier is not a selection; fall through.
        Row {
            name: "whitespace at a tier is skipped",
            explicit: Some("   "),
            task: None,
            workspace: None,
            env: Some("codex"),
            system: Some("claude"),
            expect: Some(("codex", ProviderSource::EnvironmentDefault)),
        },
        // Nothing at any tier -> no selection (caller surfaces the error).
        Row {
            name: "nothing selected",
            explicit: None,
            task: None,
            workspace: None,
            env: None,
            system: None,
            expect: None,
        },
    ];

    for row in rows {
        let got = select_crew_name(row.explicit, row.task, row.workspace, row.env, row.system);
        assert_eq!(got, row.expect, "{}", row.name);
    }
}

/// End-to-end wiring: `resolve_crew_for_task` actually reads
/// `CONSTELLATION_DEFAULT_PROVIDER`, and the environment tier stays subordinate
/// to a configured `[workflow].default_crew`. Because a workspace default beats
/// the env tier, a leaked value cannot affect concurrent tests.
#[test]
fn resolve_crew_for_task_reads_env_default_but_workspace_crew_wins() {
    // The process-wide guard every env-mutating test in this binary shares.
    // A module-local lock only serialized this test against its siblings; a
    // workspace init running elsewhere read the leaked `claude` and tried to
    // resolve it as a crew.
    let _env =
        orbit_common::test_env::scoped([(CONSTELLATION_DEFAULT_PROVIDER_ENV, Some("claude"))]);

    let (_root, runtime) = runtime_with_named_crews();
    let resolved = runtime.resolve_crew_for_task(None, None);

    let crew = resolved.expect("resolve crew");
    // `[workflow].default_crew = "primary"` (workspace tier) beats the env tier.
    assert_eq!(crew.name, "primary");
}

/// [ORB-11242] The dispatch gate is keyed on the crew a task *actually*
/// resolves to, so a task carrying an excluded crew cannot start a provider
/// even though nothing about the activity mentions the restriction.
#[test]
fn run_crew_allowlist_refuses_a_task_crew_it_excludes() {
    let (_root, runtime) = runtime_with_named_crews();
    let task_id = add_task_with_crew(&runtime, "beta");
    let input = json!({ "task_ids": [task_id], "allowed_crews": ["primary"] });

    let error = RuntimeHost::agent_crew_config_for_input(&runtime, &input)
        .expect_err("an excluded task crew must not dispatch");
    let message = error.to_string();
    assert!(message.contains("`beta`"), "{message}");
    assert!(message.contains("beta-model"), "{message}");
    assert!(message.contains("primary"), "{message}");
}

/// The permitted control for the refusal above, and the omitted-option case:
/// both must dispatch exactly as they did before the allowlist existed.
#[test]
fn run_crew_allowlist_permits_a_listed_crew_and_ignores_an_absent_list() {
    let (_root, runtime) = runtime_with_named_crews();
    let task_id = add_task_with_crew(&runtime, "beta");

    for input in [
        json!({ "task_ids": [task_id], "allowed_crews": ["beta"] }),
        json!({ "task_ids": [task_id], "allowed_crews": [] }),
        json!({ "task_ids": [task_id], "allowed_crews": "" }),
        json!({ "task_ids": [task_id] }),
    ] {
        let config = RuntimeHost::agent_crew_config_for_input(&runtime, &input)
            .unwrap_or_else(|error| panic!("dispatch must succeed for {input}: {error}"))
            .expect("crew config");
        assert_eq!(config.model.as_deref(), Some("beta-model"), "{input}");
        assert_eq!(config.provider, Some(Provider::Codex), "{input}");
    }
}

/// A crew that is a different registry name for the same configured
/// `(provider, model)` is permitted: the allowlist restricts provider usage,
/// and naming a wrapper is not itself provider usage.
#[test]
fn run_crew_allowlist_matches_on_effective_configured_identity() {
    let (_root, runtime) = runtime_with_named_crews();
    // `system` is synthesized onto the same assignment as `primary`.
    let task_id = add_task_with_crew(&runtime, "system");

    let config = RuntimeHost::agent_crew_config_for_input(
        &runtime,
        &json!({ "task_ids": [task_id], "allowed_crews": ["primary"] }),
    )
    .expect("an alias of a permitted identity dispatches")
    .expect("crew config");
    assert_eq!(config.model.as_deref(), Some("default-model"));
}

/// A malformed allowlist fails the dispatch instead of being read as "no
/// restriction", which would silently widen the window it was meant to narrow.
#[test]
fn run_crew_allowlist_fails_closed_on_a_malformed_or_unknown_entry() {
    let (_root, runtime) = runtime_with_named_crews();
    let task_id = add_task_with_crew(&runtime, "beta");

    for input in [
        json!({ "task_ids": [task_id], "allowed_crews": ["not-a-crew"] }),
        json!({ "task_ids": [task_id], "allowed_crews": [""] }),
        json!({ "task_ids": [task_id], "allowed_crews": [7] }),
        json!({ "task_ids": [task_id], "allowed_crews": 7 }),
    ] {
        RuntimeHost::agent_crew_config_for_input(&runtime, &input)
            .expect_err(&format!("a malformed allowlist must fail closed: {input}"));
    }
}
