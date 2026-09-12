use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::identity::{Crew, CrewAssignment, ReasoningEffort};
use tempfile::tempdir;

use super::{roots, write_config};
use crate::registry::resolve_default_crew;
use crate::resolved::{
    RETIRED_DUEL_CONFIG_WARNING, RETIRED_ROUTINES_CONFIG_WARNING, default_crews,
    retired_backend_override_check,
};
use crate::{ConfigSnapshot, ExecutionEnvPolicy, PersistenceConfig, ResolvedConfig};

fn single_family_crew(name: &str) -> Crew {
    let assignment = CrewAssignment {
        model: format!("{name}-model"),
        provider: name.to_string(),
        effort: None,
    };
    Crew {
        name: name.to_string(),
        assignment,
        description: None,
        tags: Vec::new(),
    }
}

#[test]
fn built_in_crews_use_standard_model_specific_names() {
    let crews = default_crews();
    // Every built-in is named for its model except `system`, which names a
    // lane. [ORB-10877] It is built in because shipped job steps name it
    // directly, so a config with no `[crews]` table must still resolve it.
    //
    // `copilot`, `cursor`, `pi`, `antigravity`, and `opencode` are
    // provider-lane exceptions: each can route to models supplied by several
    // vendors, so the crew names retain the execution provider identity.
    // [ORB-10946] [ORB-10945] [ORB-11296] [ORB-11299] [ORB-11295]
    assert_eq!(
        crews.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "antigravity",
            "astra",
            "copilot",
            "cursor",
            "fable",
            "gemini",
            "grok",
            "luna",
            "opencode",
            "opus",
            "pi",
            "sol",
            "sonnet",
            "system",
            "terra",
        ]
    );
    for (name, provider, model) in [
        ("opus", "claude", "opus"),
        ("sonnet", "claude", "sonnet"),
        ("fable", "claude", "fable"),
        ("sol", "codex", "gpt-5.6-sol"),
        ("terra", "codex", "gpt-5.6-terra"),
        ("luna", "codex", "gpt-5.6-luna"),
        ("astra", "codex", "gpt-6-astra"),
        ("gemini", "gemini", "gemini-3.8-flash"),
        ("antigravity", "antigravity", "gemini-3.8-flash-high"),
        ("grok", "grok", "grok-4.6"),
        ("copilot", "copilot", "claude-sonnet-4.5"),
        ("cursor", "cursor", "gpt-5"),
        ("pi", "pi", "sonnet"),
        ("opencode", "opencode", "anthropic/claude-sonnet-4-5"),
        ("system", "claude", "sonnet"),
    ] {
        let assignment = &crews.get(name).expect("built-in crew").assignment;
        assert_eq!(assignment.provider, provider);
        assert_eq!(assignment.model, model);
    }
    assert!(!crews.contains_key("claude"));
    assert!(!crews.contains_key("codex"));
    // The Copilot lane keeps its own identity: it is never aliased to the
    // vendor supplying its model, nor to GitHub. [ORB-10946]
    assert!(!crews.contains_key("github"));
    assert!(!crews.contains_key("claude-sonnet-4.5"));
    assert!(!crews.contains_key("anysphere"));

    let config = ResolvedConfig::built_in(PersistenceConfig::default_for_data_root(Path::new(
        ".orbit",
    )));
    assert_eq!(config.default_crew.as_deref(), Some("opus"));
}

fn load_config(body: &str) -> Result<ResolvedConfig, OrbitError> {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(workspace.path(), body);
    ResolvedConfig::load(&roots(global.path(), workspace.path()))
}

#[test]
fn crew_description_and_tags_normalize_without_loss() {
    let config = load_config(
        r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "gpt-test"
provider = "codex"
backend = "cli"
description = "  Systems implementation  "
tags = [" review ", "", "hard", "review"]
"#,
    )
    .expect("config loads");
    let crew = config.crews.get("sol").expect("sol crew");
    assert_eq!(crew.description.as_deref(), Some("Systems implementation"));
    assert_eq!(crew.tags, vec!["hard", "review"]);
}

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
codex = "gpt-5.6-sol"
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

#[test]
fn pr_config_defaults_to_no_task_url_template_without_config() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");

    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");

    assert_eq!(config.pr.task_url_template.as_deref(), None);
}

#[test]
fn pr_task_url_template_loads_from_workspace_config() {
    let config =
        load_config("[pr]\ntask_url_template = \"https://orbit-cli.com/tasks/{task_id}\"\n")
            .expect("config loads");

    assert_eq!(
        config.pr.task_url_template.as_deref(),
        Some("https://orbit-cli.com/tasks/{task_id}")
    );
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

#[test]
fn crews_load_when_present_and_well_formed() {
    let config = load_config(
        r#"
[crews.codex]
model = "gpt-5.5"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "codex"
"#,
    )
    .expect("config loads");

    assert_eq!(config.default_crew.as_deref(), Some("codex"));
    assert_eq!(
        config
            .crews
            .get("codex")
            .expect("crew exists")
            .assignment
            .model,
        orbit_common::test_fixtures::TEST_CODEX_MODEL
    );
    assert_eq!(
        config
            .crews
            .get("codex")
            .expect("crew exists")
            .assignment
            .effort,
        None,
        "omitted effort preserves the provider default"
    );
}

#[test]
fn codex_crew_effort_accepts_every_supported_value() {
    for (raw, expected) in [
        ("low", ReasoningEffort::Low),
        ("medium", ReasoningEffort::Medium),
        ("high", ReasoningEffort::High),
        ("xhigh", ReasoningEffort::Xhigh),
        ("max", ReasoningEffort::Max),
    ] {
        let config = load_config(&format!(
            "[crews.terra]\nmodel = \"gpt-5.6-terra\"\nprovider = \"codex\"\neffort = \"{raw}\"\n\n[workflow]\ndefault_crew = \"terra\"\n"
        ))
        .unwrap_or_else(|error| panic!("{raw} should load: {error}"));
        assert_eq!(
            config
                .crews
                .get("terra")
                .expect("terra crew")
                .assignment
                .effort,
            Some(expected),
            "{raw}"
        );
    }
}

#[test]
fn crew_effort_fails_closed_for_invalid_values_and_unsupported_providers() {
    let invalid = load_config(
        "[crews.terra]\nmodel = \"gpt-5.6-terra\"\nprovider = \"codex\"\neffort = \"medium-low\"\n\n[workflow]\ndefault_crew = \"terra\"\n",
    )
    .expect_err("invalid effort must fail config admission");
    assert!(
        invalid
            .to_string()
            .contains("expected one of low, medium, high, xhigh, max")
    );

    let unsupported = load_config(
        "[crews.gemini]\nmodel = \"gemini\"\nprovider = \"gemini\"\neffort = \"high\"\n\n[workflow]\ndefault_crew = \"gemini\"\n",
    )
    .expect_err("unsupported provider must not silently ignore effort");
    assert!(
        unsupported
            .to_string()
            .contains("does not support configured reasoning effort")
    );

    let agy_xhigh = load_config(
        "[crews.antigravity]\nmodel = \"gemini-3.8-flash-high\"\nprovider = \"antigravity\"\neffort = \"xhigh\"\n\n[workflow]\ndefault_crew = \"antigravity\"\n",
    )
    .expect_err("agy does not accept xhigh");
    assert!(
        agy_xhigh.to_string().contains("low, medium, high"),
        "{agy_xhigh}"
    );

    let agy_legacy_model = load_config(
        "[crews.antigravity]\nmodel = \"gemini-3.8-flash\"\nprovider = \"antigravity\"\n\n[workflow]\ndefault_crew = \"antigravity\"\n",
    )
    .expect_err("legacy gemini CLI model ids are not remapped");
    assert!(
        agy_legacy_model
            .to_string()
            .contains("gemini-3.8-flash-high"),
        "{agy_legacy_model}"
    );
}

#[test]
fn claude_crew_effort_accepts_every_supported_value() {
    for raw in ["low", "medium", "high", "xhigh", "max"] {
        load_config(&format!(
            "[crews.opus]\nmodel = \"opus\"\nprovider = \"claude\"\neffort = \"{raw}\"\n\n[workflow]\ndefault_crew = \"opus\"\n"
        ))
        .unwrap_or_else(|error| panic!("Claude effort {raw} should load: {error}"));
    }
}

#[test]
fn grok_crew_effort_enforces_the_verified_model_contract() {
    for raw in ["low", "medium", "high", "xhigh"] {
        load_config(&format!(
            "[crews.grok]\nmodel = \"grok-4.6\"\nprovider = \"grok\"\neffort = \"{raw}\"\n\n[workflow]\ndefault_crew = \"grok\"\n"
        ))
        .unwrap_or_else(|error| panic!("Grok 4.6 effort {raw} should load: {error}"));
    }

    for (model, effort, expected) in [
        ("grok-4.6", "max", "low, medium, high, xhigh"),
        ("grok-4.5", "xhigh", "low, medium, high"),
        ("grok-unknown", "high", "verified only"),
    ] {
        let error = load_config(&format!(
            "[crews.grok]\nmodel = \"{model}\"\nprovider = \"grok\"\neffort = \"{effort}\"\n\n[workflow]\ndefault_crew = \"grok\"\n"
        ))
        .expect_err("unsupported Grok model-effort pair must fail config admission");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn absent_crews_use_built_in_defaults() {
    let config = load_config("[scoring]\nenabled = true\n").expect("config without crews loads");

    assert_eq!(config.crews, default_crews());
    assert_eq!(config.default_crew.as_deref(), Some("opus"));
}

#[test]
fn explicitly_empty_crews_preserve_an_empty_registry() {
    let config = load_config("[crews]\n").expect("empty crew registry loads");

    assert!(config.crews.is_empty());
    assert_eq!(config.default_crew, None);
}

#[test]
fn default_crew_must_reference_defined_crew() {
    let error = load_config(
        r#"
[crews.codex]
model = "gpt-5.5"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "missing"
"#,
    )
    .expect_err("unknown default crew fails");

    assert!(matches!(error, OrbitError::InvalidInputDiagnostic { .. }));
    assert_eq!(error.did_you_mean(), Some(&["codex".to_string()][..]));
}

#[test]
fn default_crew_unset_with_custom_crews_fails_load() {
    // Only a non-seeded crew defined; no [workflow] table at all.
    let error = load_config(
        r#"
[crews.my-team]
model = "gpt-5.5"
provider = "codex"
backend = "cli"
"#,
    )
    .expect_err("missing default_crew with non-seeded crews must fail");

    let message = error.to_string();
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{message}");
    assert!(message.contains("[workflow].default_crew"), "{message}");
    assert!(message.contains("my-team"), "{message}");
}

#[test]
fn default_crew_unset_with_seeded_crew_still_loads() {
    // The canonical claude system crew is present, so the fallback applies.
    let config = load_config(
        r#"
[crews.claude]
model = "opus"
provider = "claude"
backend = "cli"
"#,
    )
    .expect("config loads");
    assert_eq!(config.default_crew.as_deref(), Some("claude"));
}

#[test]
fn workflow_default_crew_no_crews_defined_is_noop() {
    let crews = BTreeMap::new();

    let default_crew = resolve_default_crew(None, &crews, None).expect("empty registry is allowed");

    assert_eq!(default_crew, None);
}

#[test]
fn workflow_default_crew_uses_environment_then_claude_system_default() {
    let crews = BTreeMap::from([
        ("opus".to_string(), single_family_crew("claude")),
        ("sol".to_string(), single_family_crew("codex")),
        ("gemini".to_string(), single_family_crew("gemini")),
    ]);

    let env = resolve_default_crew(None, &crews, Some("google"))
        .expect("deprecated environment alias resolves");
    assert_eq!(env.as_deref(), Some("gemini"));

    let codex = resolve_default_crew(None, &crews, Some("codex"))
        .expect("provider environment maps to standard crew");
    assert_eq!(codex.as_deref(), Some("sol"));

    let system =
        resolve_default_crew(None, &crews, None).expect("canonical system default resolves");
    assert_eq!(system.as_deref(), Some("opus"));

    let error = resolve_default_crew(None, &crews, Some("bogus"))
        .expect_err("selected invalid environment value must not fall back");
    assert!(error.to_string().contains("CONSTELLATION_DEFAULT_PROVIDER"));
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
fn flat_crews_with_incomplete_assignment_fail_load() {
    let error = load_config(
        r#"
[crews.codex]
provider = "codex"
"#,
    )
    .expect_err("incomplete crew fails");

    assert!(matches!(error, OrbitError::InvalidInput(_)));
    assert!(error.to_string().contains("[crews.codex]"));
    assert!(error.to_string().contains("model"));
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

#[test]
fn workflow_auto_ship_defaults_false_and_loads_when_set() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");

    write_config(workspace.path(), "");
    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");
    assert!(!config.workflow_auto_ship);

    write_config(
        workspace.path(),
        r#"
[workflow]
auto_ship = true
"#,
    );
    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");
    assert!(config.workflow_auto_ship);
}

#[test]
fn shipped_default_config_has_no_workspace_specific_identifiers() {
    let shipped = include_str!("../../assets/default-config.toml");

    for forbidden in ["ORB-", "agent-main", "dk-server", "F2026-"] {
        assert!(
            !shipped.contains(forbidden),
            "shipped config must not contain workspace-specific marker {forbidden:?}"
        );
    }
}

#[test]
fn runtime_log_rotation_rejects_invalid_values() {
    // [ORB-00415] Malformed rotation knobs must fail at config load with a
    // clear, key-naming error.
    let error = load_config("[runtime]\nlog_retention_days = 0\n")
        .expect_err("zero retention must fail config load");
    assert!(
        error.to_string().contains("log_retention_days"),
        "message: {error}"
    );

    let error = load_config("[runtime]\nlog_max_total_mb = 10\nlog_max_file_mb = 50\n")
        .expect_err("per-file budget above total must fail config load");
    assert!(
        error.to_string().contains("log_max_file_mb"),
        "message: {error}"
    );
}

#[test]
fn runtime_log_rotation_accepts_valid_values() {
    load_config(
        "[runtime]\nlog_retention_days = 14\nlog_max_total_mb = 200\nlog_max_file_mb = 20\n",
    )
    .expect("valid log rotation config should load");
}

/// [ORB-10877] Shipped job steps name `crew: system` directly. A config that
/// points the lane elsewhere with `system_crew` must keep running system work
/// on that crew — resolving to `qa` instead would silently relocate it, which
/// on a host that deliberately picked a cheap lane crew is a cost regression.
#[test]
fn an_absent_system_crew_resolves_onto_the_configured_system_crew() {
    let resolved = load_config(
        "[workflow]\ndefault_crew = \"opus\"\nsystem_crew = \"luna\"\n\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n\n[crews.luna]\nprovider = \"codex\"\nmodel = \"gpt-5.6-luna\"\n\n[crews.qa]\nprovider = \"codex\"\nmodel = \"gpt-5.6-terra\"\n",
    )
    .expect("a config predating the system crew must load");

    let system = resolved.crews.get("system").expect("system crew resolves");
    assert_eq!(system.assignment.provider, "codex");
    assert_eq!(
        system.assignment.model, "gpt-5.6-luna",
        "the configured system_crew must win over the qa fallback"
    );
}

/// With no `system_crew` key the configured name is still the default `system`,
/// so the only thing left to resolve against is the crew that carried the lane.
#[test]
fn an_absent_system_crew_resolves_onto_the_qa_crew() {
    let resolved = load_config(
        "[workflow]\ndefault_crew = \"opus\"\n\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n\n[crews.qa]\nprovider = \"claude\"\nmodel = \"sonnet\"\n",
    )
    .expect("a config predating the system crew must load");

    let system = resolved.crews.get("system").expect("system crew resolves");
    assert_eq!(system.name, "system");
    assert_eq!(system.assignment.provider, "claude");
    assert_eq!(system.assignment.model, "sonnet");
    assert_eq!(resolved.system_crew, "system");
}

/// The alias only fills an absent name; a host that defines both keeps its own.
#[test]
fn an_explicit_system_crew_is_not_overwritten_by_the_qa_alias() {
    let resolved = load_config(
        "[workflow]\ndefault_crew = \"opus\"\n\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n\n[crews.system]\nprovider = \"codex\"\nmodel = \"gpt-5.6-luna\"\n\n[crews.qa]\nprovider = \"claude\"\nmodel = \"sonnet\"\n",
    )
    .expect("config defining both crews must load");

    let system = resolved
        .crews
        .get("system")
        .expect("system crew is defined");
    assert_eq!(system.assignment.provider, "codex");
    assert_eq!(system.assignment.model, "gpt-5.6-luna");
    assert_eq!(resolved.crews.get("qa").expect("qa stays").name, "qa");
}

/// Pre-system Gemini- and Grok-only configs had neither `system` nor `qa`.
/// Their family default is the only configured portable target, so the
/// compatibility alias must use it rather than leaving shipped system work
/// undispatchable.
#[test]
fn legacy_configs_without_qa_alias_system_onto_the_default_crew() {
    for (crew, provider, model) in [
        ("gemini", "gemini", "gemini-3.7-flash"),
        ("grok", "grok", "grok-4.6"),
    ] {
        let resolved = load_config(&format!(
            "[workflow]\ndefault_crew = \"{crew}\"\nsystem_crew = \"qa\"\n\n[crews.{crew}]\nprovider = \"{provider}\"\nmodel = \"{model}\"\n"
        ))
        .expect("a legacy single-family config must load");

        let system = resolved.crews.get("system").expect("system crew resolves");
        assert_eq!(system.name, "system");
        assert_eq!(system.assignment.provider, provider);
        assert_eq!(system.assignment.model, model);
    }
}

/// Compatibility applies only to Orbit's own historical lane names. Silently
/// substituting `qa` for an unknown custom name would make job steps run on a
/// different crew while recovery paths still fail on the configured name.
#[test]
fn an_unknown_custom_system_crew_does_not_fall_back_to_qa() {
    let resolved = load_config(
        "[workflow]\ndefault_crew = \"opus\"\nsystem_crew = \"missing\"\n\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n\n[crews.qa]\nprovider = \"claude\"\nmodel = \"sonnet\"\n",
    )
    .expect("an unresolved system crew is diagnosed at dispatch, not config load");

    assert_eq!(resolved.system_crew, "missing");
    assert!(
        !resolved.crews.contains_key("system"),
        "an unknown custom crew must not be masked by the legacy qa fallback"
    );
}

/// The ambient environment an operator would reasonably expect
/// `inherit = false` to exclude: benignly named credentials and service URLs
/// alongside the runtime context a provider CLI genuinely needs. Stated by the
/// test so the assertions never depend on the developer's shell. [ORB-10917]
fn ambient_env_with_credentials() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
        ("ANTHROPIC_API_KEY", Some("sk-ant-00000000000000000000")),
        ("HOME", Some("/home/agent")),
        ("PATH", Some("/usr/bin:/bin")),
        ("CODEX_HOME", Some("/home/agent/.codex")),
        ("ORBIT_RUN_ID", Some("jrun-10917")),
    ])
}

fn value_of<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

#[test]
fn default_policy_excludes_benignly_named_ambient_credentials() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy::default();

    let env = policy.agent_subprocess_env(&[]);

    assert_eq!(value_of(&env, "DATABASE_URL"), None);
    assert_eq!(value_of(&env, "BILLING_ENDPOINT"), None);
    assert_eq!(value_of(&env, "ANTHROPIC_API_KEY"), None);
    // The documented baseline, the configured pass list, and the ORBIT_*
    // execution envelope still reach the child.
    assert_eq!(value_of(&env, "HOME"), Some("/home/agent"));
    assert_eq!(value_of(&env, "PATH"), Some("/usr/bin:/bin"));
    assert_eq!(value_of(&env, "CODEX_HOME"), Some("/home/agent/.codex"));
    assert_eq!(value_of(&env, "ORBIT_RUN_ID"), Some("jrun-10917"));
}

#[test]
fn configured_pass_list_is_the_admission_path_for_an_ambient_credential() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy {
        inherit: false,
        pass: vec!["DATABASE_URL".to_string()],
    };

    let env = policy.agent_subprocess_env(&[]);

    assert_eq!(
        value_of(&env, "DATABASE_URL"),
        Some("postgres://svc:hunter2@db.internal")
    );
    assert_eq!(value_of(&env, "BILLING_ENDPOINT"), None);
}

#[test]
fn provider_required_extras_reach_the_child_under_a_narrow_pass_list() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy {
        inherit: false,
        pass: Vec::new(),
    };

    let env = policy.agent_subprocess_env(&["CODEX_HOME"]);

    assert_eq!(value_of(&env, "CODEX_HOME"), Some("/home/agent/.codex"));
    assert_eq!(value_of(&env, "DATABASE_URL"), None);
}

/// `inherit = true` stays a real, explicit opt-in to full inheritance. The
/// config surface pins the flag off (see `ExecutionEnvPolicy::inherit`), so the
/// branch is asserted against a directly constructed policy.
#[test]
fn inherit_opts_into_full_environment_inheritance() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy {
        inherit: true,
        pass: Vec::new(),
    };

    let env = policy.agent_subprocess_env(&[]);

    assert_eq!(
        value_of(&env, "DATABASE_URL"),
        Some("postgres://svc:hunter2@db.internal"),
        "inherit = true forwards the whole parent environment by design"
    );
    assert_eq!(
        value_of(&env, "ANTHROPIC_API_KEY"),
        Some("sk-ant-00000000000000000000")
    );
}

#[test]
fn config_admission_pins_inherit_off() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        workspace.path(),
        "[execution.env]\ninherit = true\npass = [\"HOME\"]\n",
    );

    let resolved =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");

    assert!(
        !resolved.execution_env.inherit(),
        "execution.env.inherit is inert; a config file must not re-enable inheritance"
    );
}

#[test]
fn built_in_defaults_are_reachable_without_any_config_file() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");

    let resolved =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("built-ins load");

    assert_eq!(resolved.crews, default_crews());
    assert_eq!(
        resolved.snapshot.execution_env_pass,
        ConfigSnapshot::default().execution_env_pass
    );
}

#[test]
fn complexity_crew_pools_are_validated_deduplicated_and_projected() {
    let config = load_config(
        r#"
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["terra", " grok ", "terra"]
hard_complexity_crews = ["astra"]
"#,
    )
    .expect("valid pools");
    assert_eq!(
        config.complexity_crews.medium,
        Some(vec!["grok".into(), "terra".into()])
    );
    for (key, expected) in [
        ("workflow.low_complexity_crews", serde_json::json!(["luna"])),
        (
            "workflow.medium_complexity_crews",
            serde_json::json!(["grok", "terra"]),
        ),
        (
            "workflow.hard_complexity_crews",
            serde_json::json!(["astra"]),
        ),
    ] {
        assert_eq!(config.snapshot.value_for(key), Some(expected));
    }
    for invalid in [
        r#"["missing"]"#,
        r#"[" "]"#,
        r#"["grok", ""]"#,
        "[1]",
        "false",
    ] {
        let error = load_config(&format!(
            "[workflow]\nmedium_complexity_crews = {invalid}\n"
        ))
        .expect_err("bad pools must fail config admission");
        assert!(
            error.to_string().contains("medium_complexity_crews"),
            "{error}"
        );
    }
}

#[test]
fn complexity_crew_pools_layer_by_replacement_and_empty_disables() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(
        global.path(),
        "[workflow]\nlow_complexity_crews = [\"luna\"]\nmedium_complexity_crews = [\"grok\"]\nhard_complexity_crews = [\"astra\"]\n",
    );
    write_config(
        workspace.path(),
        "[workflow]\nmedium_complexity_crews = [\"terra\"]\nhard_complexity_crews = []\n",
    );
    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("layered pools");
    assert_eq!(config.complexity_crews.low, Some(vec!["luna".into()]));
    assert_eq!(config.complexity_crews.medium, Some(vec!["terra".into()]));
    assert_eq!(config.complexity_crews.hard, Some(vec![]));
    let defaults = load_config("").expect("defaults");
    assert_eq!(defaults.complexity_crews.medium, Some(vec![]));
}
