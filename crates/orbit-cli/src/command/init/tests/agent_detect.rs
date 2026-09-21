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
