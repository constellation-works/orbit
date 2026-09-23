use crate::command::init::agent_detect::DetectedAgents;
use crate::command::init::agent_prompt::testing::CannedPrompter;
use crate::command::init::agent_prompt::*;
use crate::command::init::seed::config_seed_from_detection;

fn claude_and_codex() -> DetectedAgents {
    DetectedAgents {
        claude_cli: true,
        codex_cli: true,
        ..DetectedAgents::default()
    }
}

#[test]
fn empty_answer_accepts_the_recommended_default_crew() {
    let detected = claude_and_codex();
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new([""]);
    let result = collect_default_crew(&detected, &seed, &mut prompter).unwrap();

    assert_eq!(result.as_deref(), Some("opus"));
    let transcript = prompter.transcript();
    assert!(transcript.contains("one crew assignment"));
    assert!(transcript.contains("run's resolved crew"));
    assert!(transcript.contains("Recommended default crew:\n  opus         claude       opus"));
    assert!(transcript.contains("Use this default crew? [Y/n]: "));
    for retired in ["Reviewer", "Implementer", "Planner", "Custom", "custom"] {
        assert!(!transcript.contains(retired), "{retired}");
    }
}

/// [ORB-12719] Declining the recommendation lists every seeded crew by name,
/// recommendation first, and writes the chosen name — never a `custom` table.
#[test]
fn declining_lists_seeded_crews_by_name_and_returns_the_chosen_one() {
    let detected = claude_and_codex();
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new(["n", "9", "0", "custom", "2"]);
    let result = collect_default_crew(&detected, &seed, &mut prompter).unwrap();

    assert_eq!(result.as_deref(), Some("astra"));
    let transcript = prompter.transcript();
    assert!(transcript.contains(
        "Choose the default crew:\n\n   1. opus         claude       opus\n   2. astra        codex        gpt-6-astra\n   3. fable        claude       fable\n   4. luna         codex        gpt-6-luna\n   5. sol          codex        gpt-6-sol\n   6. sonnet       claude       sonnet\n   7. terra        codex        gpt-5.6-terra"
    ), "{transcript}");
    assert!(!transcript.contains(" 8."), "{transcript}");
    assert_eq!(transcript.matches("Please enter 1-7.").count(), 3);
    assert!(!transcript.contains("Custom"));
    assert!(!transcript.contains("Provider ["));
    assert!(!transcript.contains("Model ["));
}

#[test]
fn empty_choice_after_declining_keeps_the_recommendation() {
    let detected = DetectedAgents {
        codex_cli: true,
        grok_cli: true,
        ..DetectedAgents::default()
    };
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new(["no", ""]);
    let result = collect_default_crew(&detected, &seed, &mut prompter).unwrap();

    assert_eq!(result.as_deref(), Some("astra"));
    assert!(prompter.transcript().contains("Choice [1]: "));
}

/// A host with no crew-backed CLI has nothing to choose: init explains that
/// and asks nothing, so the answers meant for the identity prompts are not
/// consumed here.
#[test]
fn no_seeded_crew_skips_the_prompt() {
    let detected = DetectedAgents {
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new([] as [&str; 0]);
    let result = collect_default_crew(&detected, &seed, &mut prompter).unwrap();

    assert_eq!(result, None);
    let transcript = prompter.transcript();
    assert!(transcript.contains("Ollama CLI         found"));
    assert!(transcript.contains("empty [crews] registry"));
    assert!(!transcript.contains("Use this default crew?"));
}

#[test]
fn detected_agents_lists_every_family_when_only_copilot_is_present() {
    let detected = DetectedAgents {
        copilot_cli: true,
        ..DetectedAgents::default()
    };
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new([""]);

    let result = collect_default_crew(&detected, &seed, &mut prompter).expect("crew choice");

    assert_eq!(result.as_deref(), Some("copilot"));
    assert!(prompter.transcript().contains(
        "Detected agents:\n  Claude CLI         not found\n  Codex CLI          not found\n  Antigravity CLI    not found\n  Gemini CLI         not found\n  Grok CLI           not found\n  Copilot CLI        found\n  Cursor Agent CLI   not found\n  Pi CLI             not found\n  OpenCode CLI       not found\n  Ollama CLI         not found"
    ));
}

#[test]
fn system_crew_prompt_offers_only_detected_cheap_tier_crews_by_name() {
    let detected = DetectedAgents {
        claude_cli: true,
        codex_cli: true,
        gemini_cli: true,
        grok_cli: true,
        ..DetectedAgents::default()
    };
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new(["2"]);
    let system = collect_system_crew(&seed, &mut prompter)
        .expect("system choice")
        .expect("system is available");

    assert_eq!(system, "sonnet");
    let transcript = prompter.transcript();
    assert!(transcript.contains(
        "   1. luna         codex        gpt-6-luna\n   2. sonnet       claude       sonnet\n   3. grok         grok         grok-4.7\n   4. gemini       gemini       gemini-3.8-flash"
    ), "{transcript}");
    assert!(transcript.contains("System crew [1]: "));
    assert!(!transcript.contains("Custom"));
    assert!(!transcript.contains("gpt-6-sol"));
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
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new([] as [&str; 0]);
    let system = collect_system_crew(&seed, &mut prompter).expect("system selection");

    assert_eq!(system.as_deref(), Some("grok"));
    assert!(prompter.transcript().is_empty());
}

#[test]
fn system_crew_is_omitted_without_a_supported_family() {
    let detected = DetectedAgents {
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    let seed = config_seed_from_detection(&detected);
    let mut prompter = CannedPrompter::new([] as [&str; 0]);
    let system = collect_system_crew(&seed, &mut prompter).expect("system selection");

    assert!(system.is_none());
    assert!(prompter.transcript().is_empty());
}
