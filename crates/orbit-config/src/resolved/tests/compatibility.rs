use super::super::compatibility::{
    RETIRED_DUEL_CONFIG_WARNING, RETIRED_ROUTINES_CONFIG_WARNING, retired_backend_override_check,
};
use super::*;

#[test]
fn retired_duel_config_written_by_orbit_init_loads_and_is_ignored() {
    let config = load_config(
        r#"
[workflow]
base_branch = "agent-main"

[duel]
candidates = ["claude", "codex", "gemini"]

[duel.models]
claude = "opus"
codex = "gpt-6-sol"
gemini = "pro"
"#,
    )
    .expect("retired init-era duel config must load");

    assert_eq!(config.workflow_base_branch, "agent-main");
    assert!(config.snapshot.value_for("duel.candidates").is_none());
    assert!(config.snapshot.value_for("duel.models").is_none());
    assert!(RETIRED_DUEL_CONFIG_WARNING.contains("[duel]"));
    assert!(RETIRED_DUEL_CONFIG_WARNING.contains("[duel.models]"));
}

/// [ORB-12236] Registering an owner checkout is the automation opt-in. A
/// workspace that still carries the retired key keeps loading, and the key
/// selects nothing.
#[test]
fn retired_routines_role_loads_and_is_not_a_registry_key() {
    let config = load_config(
        r#"
[workflow]
base_branch = "agent-main"

[routines]
role = "source"
"#,
    )
    .expect("retired [routines] config must load");

    assert_eq!(config.workflow_base_branch, "agent-main");
    assert!(config.snapshot.value_for("routines.role").is_none());
    assert!(RETIRED_ROUTINES_CONFIG_WARNING.contains("[routines]"));
}

#[test]
fn deprecated_task_id_pattern_loads_valid_regex_from_workspace_config() {
    load_config("[knowledge]\ntask_id_pattern = \"[A-Z]+-\\\\d+\"\n").expect("config loads");
}

#[test]
fn deprecated_task_id_pattern_ignores_invalid_regex_at_load_time() {
    load_config("[knowledge]\ntask_id_pattern = \"[unclosed\"\n")
        .expect("deprecated invalid regex must load");
}

#[test]
fn deprecated_task_id_pattern_ignores_empty_string() {
    load_config("[knowledge]\ntask_id_pattern = \"  \"\n")
        .expect("deprecated empty pattern must load");
}

#[test]
fn deprecated_task_id_pattern_absent_when_section_absent() {
    let config = load_config("[scoring]\nenabled = true\n").expect("config loads");
    assert_eq!(config.pr.task_url_template.as_deref(), None);
}

/// [ORB-10801] `[runtime] backend` selected the retired agent-loop execution
/// backend. `cli` named the surviving path, so it stays accepted and inert.
#[test]
fn retired_runtime_backend_cli_is_accepted_and_ignored() {
    load_config("[runtime]\nbackend = \"cli\"\n").expect("`backend = \"cli\"` must keep loading");
}

/// [ORB-10801] `ORBIT_BACKEND` was tier 2 of the same retired chain, and gets
/// the same treatment: `cli` is inert, the removed values fail closed.
#[test]
fn retired_backend_env_override_is_inert_for_cli_and_fails_closed_otherwise() {
    let empty = toml::Value::Table(toml::map::Map::new());

    for accepted in [None, Some(""), Some("cli")] {
        retired_backend_override_check(&empty, accepted)
            .unwrap_or_else(|error| panic!("{accepted:?} must be accepted: {error}"));
    }

    for removed in ["http", "auto"] {
        let error = retired_backend_override_check(&empty, Some(removed))
            .expect_err("a removed ORBIT_BACKEND value must fail closed");
        let message = error.to_string();
        assert!(message.contains("ORBIT_BACKEND"), "message: {message}");
        assert!(message.contains(removed), "message: {message}");
        assert!(message.contains("CLI agent path"), "message: {message}");
    }
}

/// [ORB-10801] The removed values fail closed rather than being reinterpreted
/// as CLI agent execution behind the operator's back.
#[test]
fn retired_runtime_backend_http_fails_closed_with_migration() {
    for removed in ["http", "auto", "clii"] {
        let error = load_config(&format!("[runtime]\nbackend = \"{removed}\"\n"))
            .expect_err("retired backend value must fail config load");
        let message = error.to_string();

        assert!(message.contains("[runtime]"), "message: {message}");
        assert!(message.contains(removed), "message: {message}");
        assert!(message.contains("CLI agent path"), "message: {message}");
    }
}

/// [ORB-10801] `[crews.<name>] backend` pinned the same retired selector. A
/// crew that still declares `cli` keeps loading; the removed values are
/// refused rather than re-pointed at the CLI agent silently.
#[test]
fn retired_crew_backend_is_inert_for_cli_and_fails_closed_otherwise() {
    load_config(
        r#"
[crews.legacy]
model = "gpt-test"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "legacy"
"#,
    )
    .expect("`backend = \"cli\"` must keep loading");

    for removed in ["http", "auto"] {
        let error = load_config(&format!(
            r#"
[crews.legacy]
model = "gpt-test"
provider = "codex"
backend = "{removed}"

[workflow]
default_crew = "legacy"
"#
        ))
        .expect_err("a removed crew backend must fail config load");
        let message = error.to_string();
        assert!(message.contains("[crews.legacy]"), "message: {message}");
        assert!(message.contains(removed), "message: {message}");
        assert!(message.contains("CLI agent path"), "message: {message}");
    }
}

#[test]
fn legacy_role_tables_fail_with_flat_shape_rewrite_guidance() {
    let error = load_config(
        r#"
[crews.legacy]
planner = { model = "planner-model", provider = "claude", backend = "cli" }
implementer = { model = "implementer-model", provider = "codex", backend = "cli" }
reviewer = { model = "reviewer-model", provider = "gemini", backend = "cli" }

[workflow]
default_crew = "legacy"
"#,
    )
    .expect_err("legacy role tables must fail config load");
    let message = error.to_string();

    for expected in [
        "[crews.legacy]",
        "planner/implementer/reviewer",
        "model",
        "provider",
    ] {
        assert!(
            message.contains(expected),
            "expected {message:?} to contain {expected:?}"
        );
    }
}

#[test]
fn flat_crew_mixed_with_role_tables_fails_with_flat_shape_rewrite_guidance() {
    let error = load_config(
        r#"
[crews.mixed]
model = "gpt-test"
provider = "codex"
backend = "cli"
implementer = { model = "gpt-test", provider = "codex", backend = "cli" }
"#,
    )
    .expect_err("mixed crew shape must fail config load");
    let message = error.to_string();

    for expected in [
        "[crews.mixed]",
        "planner/implementer/reviewer",
        "model",
        "provider",
    ] {
        assert!(
            message.contains(expected),
            "expected {message:?} to contain {expected:?}"
        );
    }
}

#[test]
fn task_artifact_store_rejects_removed_key() {
    let error = load_config("[task]\nartifact_store = \"v2\"\n")
        .expect_err("artifact store selector must be rejected");
    let message = error.to_string();

    assert!(message.contains("[task] artifact_store"));
    assert!(message.contains("no longer supported"));
    assert!(message.contains("v2"));
}

/// [ORB-12723] `workflow.pilot_max_complexity` is removed with the pilot
/// ceiling it drove. An existing config that still sets it keeps loading
/// (the loader warns), the key is no registry setting, and `orbit config
/// get`/`set` name the removal rather than offering a did-you-mean.
#[test]
fn removed_pilot_max_complexity_loads_and_is_refused_by_config_get_and_set() {
    let config = load_config(
        r#"
[workflow]
base_branch = "agent-main"
pilot_max_complexity = "xhard"
"#,
    )
    .expect("a config carrying the removed key must load");

    assert_eq!(config.workflow_base_branch, "agent-main");
    assert!(
        config
            .snapshot
            .value_for("workflow.pilot_max_complexity")
            .is_none()
    );
    assert!(crate::describe_config_key("workflow.pilot_max_complexity").is_none());

    let error = crate::admit_config_key("workflow.pilot_max_complexity")
        .expect_err("a removed key is not addressable");
    let message = error.to_string();
    assert!(
        message.contains("workflow.pilot_max_complexity") && message.contains("removed"),
        "{message}"
    );
    assert!(
        !message.contains("unknown config key"),
        "a removed key is reported as removed, not unknown: {message}"
    );
}
