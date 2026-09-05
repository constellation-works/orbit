use crate::command::init::agent_detect::testing::MockAgentEnvProbe;
use crate::command::init::agent_detect::*;

#[test]
fn detect_reflects_probe_results() {
    let probe = MockAgentEnvProbe::new()
        .with_binary("claude")
        .with_binary("grok")
        .with_binary("ollama");
    let detected = detect(&probe);
    assert_eq!(
        detected,
        DetectedAgents {
            claude_cli: true,
            grok_cli: true,
            ollama_cli: true,
            ..DetectedAgents::default()
        }
    );
}

#[test]
fn empty_probe_detects_nothing() {
    let probe = MockAgentEnvProbe::new();
    assert_eq!(detect(&probe), DetectedAgents::default());
    assert!(available_crew_families(&detect(&probe)).is_empty());
}

#[test]
fn seeded_crew_availability_requires_a_detected_cli() {
    // [ORB-10801] Only a detected provider CLI makes a crew executable; an
    // exported API key no longer enables anything.
    assert!(
        available_crew_families(&DetectedAgents {
            ollama_cli: true,
            ..DetectedAgents::default()
        })
        .is_empty()
    );

    for (binary, family) in [
        ("claude", "claude"),
        ("codex", "codex"),
        ("agy", "antigravity"),
        ("gemini", "gemini"),
        ("grok", "grok"),
        ("copilot", "copilot"),
        ("cursor-agent", "cursor"),
        ("pi", "pi"),
    ] {
        let detected = detect(&MockAgentEnvProbe::new().with_binary(binary));
        assert_eq!(available_crew_families(&detected), vec![family]);
    }
}

#[test]
fn default_provider_prefers_cli_in_documented_order() {
    // claude wins when present
    let detected = DetectedAgents {
        claude_cli: true,
        codex_cli: true,
        antigravity_cli: true,
        gemini_cli: true,
        grok_cli: true,
        copilot_cli: true,
        cursor_cli: true,
        pi_cli: true,
        ollama_cli: true,
    };
    assert_eq!(default_provider(&detected), "claude");

    // codex wins when claude absent
    let detected = DetectedAgents {
        codex_cli: true,
        gemini_cli: true,
        grok_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "codex");

    // antigravity wins over legacy gemini when claude/codex are absent
    let detected = DetectedAgents {
        antigravity_cli: true,
        gemini_cli: true,
        grok_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "antigravity");

    // gemini wins when claude/codex/agy absent
    let detected = DetectedAgents {
        gemini_cli: true,
        grok_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "gemini");

    // grok wins when claude/codex/gemini absent — and an installed copilot
    // does not displace it. [ORB-10946]
    let detected = DetectedAgents {
        grok_cli: true,
        copilot_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "grok");

    // copilot wins over ollama when it is the only agent CLI present
    let detected = DetectedAgents {
        copilot_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "copilot");

    // cursor wins over ollama when the earlier agent families are absent.
    let detected = DetectedAgents {
        cursor_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "cursor");

    // pi wins over ollama when the earlier agent families are absent, and an
    // installed pi does not displace cursor. [ORB-11296]
    let detected = DetectedAgents {
        cursor_cli: true,
        pi_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "cursor");

    let detected = DetectedAgents {
        pi_cli: true,
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "pi");

    // ollama wins when nothing else
    let detected = DetectedAgents {
        ollama_cli: true,
        ..DetectedAgents::default()
    };
    assert_eq!(default_provider(&detected), "ollama");
}

#[test]
fn default_provider_last_resort_is_claude() {
    assert_eq!(default_provider(&DetectedAgents::default()), "claude");
}

#[test]
fn model_registry_returns_expected_defaults() {
    use orbit_common::model_defaults::{
        ANTIGRAVITY_DEFAULT_MODEL, CLAUDE_DEFAULT_STRONG, CODEX_DEFAULT_MODEL,
        COPILOT_DEFAULT_MODEL, CURSOR_DEFAULT_MODEL, GEMINI_DEFAULT_MODEL, GROK_DEFAULT_MODEL,
        PI_DEFAULT_MODEL,
    };
    assert_eq!(default_model_for("claude"), Some(CLAUDE_DEFAULT_STRONG));
    assert_eq!(default_model_for("codex"), Some(CODEX_DEFAULT_MODEL));
    assert_eq!(default_model_for("gemini"), Some(GEMINI_DEFAULT_MODEL));
    assert_eq!(
        default_model_for("antigravity"),
        Some(ANTIGRAVITY_DEFAULT_MODEL)
    );
    assert_eq!(default_model_for("grok"), Some(GROK_DEFAULT_MODEL));
    assert_eq!(default_model_for("copilot"), Some(COPILOT_DEFAULT_MODEL));
    assert_eq!(default_model_for("cursor"), Some(CURSOR_DEFAULT_MODEL));
    assert_eq!(default_model_for("pi"), Some(PI_DEFAULT_MODEL));
    assert_eq!(default_model_for("ollama"), None);
    assert_eq!(default_model_for("unknown"), None);
}
