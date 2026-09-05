use orbit_common::model_defaults::CODEX_DEFAULT_MODEL;

use crate::command::init::agent_detect::DetectedAgents;
use crate::command::init::agent_prompt::testing::CannedPrompter;
use crate::command::init::agent_prompt::*;

#[test]
fn empty_answer_accepts_one_recommended_default_crew() {
    let detected = DetectedAgents {
        claude_cli: true,
        codex_cli: true,
        ..DetectedAgents::default()
    };
    let mut prompter = CannedPrompter::new([""]);
    let result = collect_crew_setting(&detected, &mut prompter).unwrap();

    assert_eq!(result.provider.as_deref(), Some("claude"));
    let transcript = prompter.transcript();
    assert!(transcript.contains("one crew assignment"));
    assert!(transcript.contains("run's resolved crew"));
    assert!(transcript.contains("Use this default crew? [Y/n]: "));
    for retired in ["Reviewer", "Implementer", "Planner"] {
        assert!(!transcript.contains(retired));
    }
}

#[test]
fn customization_selects_one_detected_agent() {
    let detected = DetectedAgents {
        claude_cli: true,
        codex_cli: true,
        ..DetectedAgents::default()
    };
    let mut prompter = CannedPrompter::new(["n", "2", ""]);
    let result = collect_crew_setting(&detected, &mut prompter).unwrap();

    assert_eq!(result.provider.as_deref(), Some("codex"));
    assert_eq!(result.model.as_deref(), Some(CODEX_DEFAULT_MODEL));
    assert!(
        prompter
            .transcript()
            .contains("Choose an agent for the default crew:")
    );
}

#[test]
fn custom_provider_prompts_for_provider_and_model() {
    let detected = DetectedAgents::default();
    let mut prompter = CannedPrompter::new(["n", "custom", "gemini", "my-model"]);
    let result = collect_crew_setting(&detected, &mut prompter).unwrap();
    assert_eq!(result.provider.as_deref(), Some("gemini"));
    assert_eq!(result.model.as_deref(), Some("my-model"));
    // [ORB-10801] There is no backend left to choose, so init never asks.
    assert!(!prompter.transcript().contains("Backend"));
}

#[test]
fn custom_provider_reprompts_for_blank_unknown_model() {
    let detected = DetectedAgents::default();
    let mut prompter = CannedPrompter::new(["n", "custom", "openai_compat", "", "my-model"]);
    let result = collect_crew_setting(&detected, &mut prompter).unwrap();
    assert_eq!(result.model.as_deref(), Some("my-model"));
    assert!(
        prompter
            .transcript()
            .contains("Model is required for a crew assignment.")
    );
}

#[test]
fn detected_agents_lists_every_family_when_only_copilot_is_present() {
    let detected = DetectedAgents {
        copilot_cli: true,
        ..DetectedAgents::default()
    };
    let mut prompter = CannedPrompter::new([""]);

    let result = collect_crew_setting(&detected, &mut prompter).expect("crew setting");

    assert_eq!(result.provider.as_deref(), Some("copilot"));
    assert!(prompter.transcript().contains(
        "Detected agents:\n  Claude CLI         not found\n  Codex CLI          not found\n  Antigravity CLI    not found\n  Gemini CLI         not found\n  Grok CLI           not found\n  Copilot CLI        found\n  Cursor Agent CLI   not found\n  Pi CLI             not found\n  OpenCode CLI       not found\n  Ollama CLI         not found"
    ));
}

#[test]
fn system_crew_prompt_offers_only_detected_cheap_tier_options() {
    let detected = DetectedAgents {
        claude_cli: true,
        codex_cli: true,
        gemini_cli: true,
        grok_cli: true,
        ..DetectedAgents::default()
    };
    let mut prompter = CannedPrompter::new(["2"]);
    let system = collect_system_crew_setting(&detected, &mut prompter)
        .expect("system choice")
        .expect("system is available");

    assert_eq!(system.provider.as_deref(), Some("claude"));
    assert_eq!(
        system.model.as_deref(),
        Some(orbit_common::model_defaults::CLAUDE_DEFAULT_WEAK)
    );
    let transcript = prompter.transcript();
    assert!(transcript.contains("gpt-5.6-luna"));
    assert!(transcript.contains("sonnet"));
    assert!(transcript.contains("grok-4.6"));
    assert!(transcript.contains("gemini-3.8-flash"));
    assert!(transcript.contains("System crew [1]: "));
    assert!(!transcript.contains("Custom"));
    assert!(!transcript.contains("gpt-5.6-sol"));
    assert!(!transcript.contains("opus"));
    assert!(!transcript.contains("terra"));
    assert!(!transcript.contains("QA crew"));
}

#[test]
fn system_crew_auto_accepts_the_single_detected_family() {
    let detected = DetectedAgents {
        grok_cli: true,
        ..DetectedAgents::default()
    };
    let mut prompter = CannedPrompter::new([] as [&str; 0]);
    let system = collect_system_crew_setting(&detected, &mut prompter)
        .expect("system selection")
        .expect("system is available");

    assert_eq!(system.provider.as_deref(), Some("grok"));
    assert_eq!(
        system.model.as_deref(),
        Some(orbit_common::model_defaults::GROK_DEFAULT_MODEL)
    );
    assert!(prompter.transcript().is_empty());
}

#[test]
fn system_crew_is_omitted_without_a_supported_family() {
    let detected = DetectedAgents {
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    let mut prompter = CannedPrompter::new([] as [&str; 0]);
    let system = collect_system_crew_setting(&detected, &mut prompter).expect("system selection");

    assert!(system.is_none());
    assert!(prompter.transcript().is_empty());
}
