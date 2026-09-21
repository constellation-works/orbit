use std::fs;

use tempfile::tempdir;

use crate::command::init::agent_detect::{DetectedAgents, detect, testing::MockAgentEnvProbe};
use crate::command::init::agent_prompt::testing::CannedPrompter;
use crate::command::init::seed::{
    collect_interactive_crew_choices, config_seed_from_detection, config_would_be_written,
};
use crate::tests::env_isolation::EnvGuard;

fn claude_and_codex() -> DetectedAgents {
    DetectedAgents {
        claude_cli: true,
        codex_cli: true,
        ..DetectedAgents::default()
    }
}

/// Interactive init asks for the default crew, then the system crew when
/// more than one cheap-tier family is present, and records both by seeded
/// crew name. It never asks for QA, even on the Claude+Codex host that used
/// to trigger that prompt.
#[test]
fn interactive_init_records_default_and_system_crews_by_name() {
    let detected = claude_and_codex();
    let mut prompter = CannedPrompter::new(["", "2"]);
    let seed = collect_interactive_crew_choices(
        &detected,
        config_seed_from_detection(&detected),
        &mut prompter,
    )
    .expect("interactive choices");

    assert_eq!(seed.default_crew.as_deref(), Some("opus"));
    assert_eq!(seed.system_crew.as_deref(), Some("sonnet"));
    let transcript = prompter.transcript();
    assert!(transcript.contains("Use this default crew? [Y/n]: "));
    assert!(transcript.contains("System crew [1]: "));
    assert!(!transcript.contains("QA crew"));
    assert!(!transcript.contains("custom"));
}

/// [ORB-12719] Accepting every recommendation reproduces the non-interactive
/// seed exactly: the prompts add no crew the detection step did not.
#[test]
fn accepting_every_recommendation_matches_the_non_interactive_seed() {
    let detected = claude_and_codex();
    let non_interactive = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new(["", ""]);
    let interactive =
        collect_interactive_crew_choices(&detected, non_interactive.clone(), &mut prompter)
            .expect("interactive choices");

    assert_eq!(interactive.families, non_interactive.families);
    assert_eq!(
        interactive.default_crew.as_deref(),
        non_interactive.recommended_default_crew()
    );
    assert_eq!(
        interactive.system_crew.as_deref(),
        non_interactive.recommended_system_crew()
    );
    assert_eq!(interactive.seeded_crews(), non_interactive.seeded_crews());
}

/// With no crew-backed CLI there is nothing to choose, so no prompt runs and
/// the seed stays the empty projection of the detection snapshot.
#[test]
fn no_supported_family_asks_nothing() {
    let detected = DetectedAgents {
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    let mut prompter = CannedPrompter::new([] as [&str; 0]);
    let seed = collect_interactive_crew_choices(
        &detected,
        config_seed_from_detection(&detected),
        &mut prompter,
    )
    .expect("no prompts to answer");

    assert!(seed.families.is_empty());
    assert_eq!(seed.default_crew, None);
    assert_eq!(seed.system_crew, None);
    assert!(!prompter.transcript().contains(": "));
}

/// When config.toml already exists and --force is unset, prompts are
/// skipped — `orbit init` is idempotent over an existing global root.
#[test]
fn existing_config_short_circuits_before_prompts() {
    let _env = EnvGuard::acquire();
    let root = tempdir().expect("orbit root");
    let config_path = root.path().join("config.toml");
    assert!(config_would_be_written(Some(root.path()), false).expect("fresh root"));

    fs::write(&config_path, "# pre-existing\n").expect("preseed");
    assert!(!config_would_be_written(Some(root.path()), false).expect("existing root"));
    assert!(config_would_be_written(Some(root.path()), true).expect("forced root"));
}

/// The seed carries only families Orbit ships crews for. `ollama` is detected
/// for the prompt's benefit but must not reach config seeding, which has no
/// ollama crew to write.
#[test]
fn seed_projects_detected_clis_onto_crew_families_only() {
    let detected = detect(
        &MockAgentEnvProbe::new()
            .with_binary("claude")
            .with_binary("grok")
            .with_binary("ollama"),
    );

    let seed = config_seed_from_detection(&detected);

    assert_eq!(
        seed.families.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["claude", "grok"]
    );
    assert_eq!(seed.default_crew, None);
    assert_eq!(seed.system_crew, None);
}

#[test]
fn seed_is_empty_when_no_provider_cli_is_installed() {
    let seed = config_seed_from_detection(&DetectedAgents::default());

    assert!(seed.families.is_empty());
}
