use std::fs;

use crate::command::Execute;

use super::super::set::ConfigSetArgs;
use super::test_runtime;

fn set_args(
    key: &str,
    value: &str,
    global: bool,
    seed_from_global: bool,
    fresh: bool,
) -> ConfigSetArgs {
    ConfigSetArgs {
        key: key.to_string(),
        value: value.to_string(),
        global,
        seed_from_global,
        fresh,
    }
}

#[test]
fn set_without_flags_fails_closed_when_workspace_config_missing() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();

    let error = set_args("workflow.base_branch", "agent-main", false, false, false)
        .execute(&runtime)
        .expect_err("missing workspace config must fail closed");
    let message = error.to_string();
    assert!(message.contains("--seed-from-global"), "{message}");
    assert!(message.contains("--fresh"), "{message}");
    assert!(!workspace_root.join("config.toml").exists());
}

#[test]
fn set_with_seed_from_global_copies_global_content_then_applies_edit() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        "# hand-written global comment\n[workflow]\nbase_branch = \"main\"\ndefault_crew = \"sol\"\n",
    )
    .expect("write global config");

    set_args("workflow.base_branch", "agent-main", false, true, false)
        .execute(&runtime)
        .expect("seed-from-global set succeeds");

    let saved = fs::read_to_string(workspace_root.join("config.toml"))
        .expect("read seeded workspace config");
    assert!(saved.contains("# hand-written global comment"), "{saved}");
    assert!(saved.contains("base_branch = \"agent-main\""), "{saved}");
    assert!(saved.contains("default_crew = \"sol\""), "{saved}");
}

#[test]
fn set_with_fresh_starts_from_empty_document() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        "[workflow]\nbase_branch = \"main\"\ndefault_crew = \"sol\"\n",
    )
    .expect("write global config");

    set_args("workflow.base_branch", "agent-main", false, false, true)
        .execute(&runtime)
        .expect("fresh set succeeds");

    let saved = fs::read_to_string(workspace_root.join("config.toml"))
        .expect("read fresh workspace config");
    assert!(saved.contains("base_branch = \"agent-main\""), "{saved}");
    assert!(!saved.contains("default_crew"), "{saved}");
}

#[test]
fn set_global_targets_global_file_even_when_workspace_config_exists() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        workspace_root.join("config.toml"),
        "[workflow]\nbase_branch = \"agent-main\"\n",
    )
    .expect("write workspace config");

    set_args("workflow.base_branch", "release", true, false, false)
        .execute(&runtime)
        .expect("global set succeeds");

    let global_saved =
        fs::read_to_string(global_root.join("config.toml")).expect("read global config");
    assert!(
        global_saved.contains("base_branch = \"release\""),
        "{global_saved}"
    );

    let workspace_saved = fs::read_to_string(workspace_root.join("config.toml"))
        .expect("read untouched workspace config");
    assert!(
        workspace_saved.contains("base_branch = \"agent-main\""),
        "{workspace_saved}"
    );
}

#[test]
fn set_rejects_invalid_value_without_writing_workspace_config() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    fs::write(
        workspace_root.join("config.toml"),
        "[execution.codex]\nsandbox = \"workspace-write\"\n",
    )
    .expect("write workspace config");
    let original = fs::read(workspace_root.join("config.toml")).expect("read original config");

    let error = set_args(
        "execution.codex.sandbox",
        "not-a-real-mode",
        false,
        false,
        false,
    )
    .execute(&runtime)
    .expect_err("invalid sandbox mode must be rejected");
    assert!(error.to_string().contains("sandbox"), "{error}");

    let after = fs::read(workspace_root.join("config.toml")).expect("read config after failed set");
    assert_eq!(after, original);
}

#[test]
fn set_rejects_unknown_key() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    fs::write(workspace_root.join("config.toml"), "").expect("write empty workspace config");

    let error = set_args("workflow.not_a_real_key", "value", false, false, false)
        .execute(&runtime)
        .expect_err("unknown key must be rejected");
    assert!(error.did_you_mean().is_some());
}

#[test]
fn set_rejects_unknown_key_before_asking_about_seeding_a_missing_workspace_config() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    assert!(!workspace_root.join("config.toml").exists());

    let error = set_args("workflow.not_a_real_key", "value", false, false, false)
        .execute(&runtime)
        .expect_err("unknown key must be rejected even with no workspace config yet");
    let message = error.to_string();
    assert!(message.contains("unknown config key"), "{message}");
    assert!(!message.contains("--seed-from-global"), "{message}");
    assert!(!workspace_root.join("config.toml").exists());
}

fn write_sol_crew(path: &std::path::Path) {
    fs::write(
        path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nmodel = \"gpt-5.6-sol\"\nprovider = \"codex\"\n# hand-authored comment\n",
    )
    .expect("write sol crew config");
}

#[test]
fn set_crew_effort_on_existing_sol_crew_round_trips() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_sol_crew(&workspace_root.join("config.toml"));

    set_args("crews.sol.effort", "high", false, false, false)
        .execute(&runtime)
        .expect("set sol effort succeeds");

    let saved =
        fs::read_to_string(workspace_root.join("config.toml")).expect("read workspace config");
    assert!(saved.contains("effort = \"high\""), "{saved}");
    assert!(saved.contains("# hand-authored comment"), "{saved}");
    assert!(saved.contains("model = \"gpt-5.6-sol\""), "{saved}");
}

#[test]
fn set_crew_effort_rejects_invalid_value_without_writing() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_sol_crew(&workspace_root.join("config.toml"));
    let original = fs::read(workspace_root.join("config.toml")).expect("read original");

    let error = set_args("crews.sol.effort", "medium-low", false, false, false)
        .execute(&runtime)
        .expect_err("invalid effort must be rejected");
    assert!(error.to_string().contains("expected one of"), "{error}");

    let after = fs::read(workspace_root.join("config.toml")).expect("read after failed set");
    assert_eq!(after, original);
}

#[test]
fn set_crew_effort_rejects_unsupported_provider_without_writing() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    fs::write(
        workspace_root.join("config.toml"),
        "[workflow]\ndefault_crew = \"gemini\"\n\n[crews.gemini]\nmodel = \"gemini\"\nprovider = \"gemini\"\n",
    )
    .expect("write gemini crew");
    let original = fs::read(workspace_root.join("config.toml")).expect("read original");

    let error = set_args("crews.gemini.effort", "high", false, false, false)
        .execute(&runtime)
        .expect_err("unsupported provider must be rejected");
    assert!(
        error
            .to_string()
            .contains("does not support configured reasoning effort"),
        "{error}"
    );
    assert_eq!(
        fs::read(workspace_root.join("config.toml")).expect("read after failed set"),
        original
    );
}

#[test]
fn set_rejects_misspelled_crew_field_without_writing() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_sol_crew(&workspace_root.join("config.toml"));
    let original = fs::read(workspace_root.join("config.toml")).expect("read original");

    let error = set_args("crews.sol.effrot", "high", false, false, false)
        .execute(&runtime)
        .expect_err("misspelled crew field must be rejected");
    assert!(error.to_string().contains("effrot"), "{error}");
    assert!(
        error
            .did_you_mean()
            .is_some_and(|suggestions| suggestions.iter().any(|key| key == "crews.sol.effort")),
        "{error:?}"
    );
    assert_eq!(
        fs::read(workspace_root.join("config.toml")).expect("read after failed set"),
        original
    );
}

#[test]
fn set_claude_crew_effort_accepts_supported_value() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    fs::write(
        workspace_root.join("config.toml"),
        "[workflow]\ndefault_crew = \"opus\"\n\n[crews.opus]\nmodel = \"opus\"\nprovider = \"claude\"\n",
    )
    .expect("write opus crew");

    set_args("crews.opus.effort", "max", false, false, false)
        .execute(&runtime)
        .expect("set claude effort succeeds");

    let saved =
        fs::read_to_string(workspace_root.join("config.toml")).expect("read workspace config");
    assert!(saved.contains("effort = \"max\""), "{saved}");
}
