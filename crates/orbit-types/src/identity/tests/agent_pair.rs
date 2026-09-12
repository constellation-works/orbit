mod resolution {
    use std::collections::BTreeMap;

    const TEST_CLAUDE_WEAK_MODEL: &str = "claude-sonnet-4-6";
    const TEST_CODEX_MODEL: &str = "gpt-5.5";

    use super::super::super::IdentityError;
    use super::super::super::agent_pair::*;

    fn assignment(model: &str, provider: &str) -> CrewAssignment {
        CrewAssignment {
            model: model.to_string(),
            provider: provider.to_string(),
            effort: None,
        }
    }

    fn registry() -> BTreeMap<String, Crew> {
        let mut registry = BTreeMap::new();
        registry.insert(
            "codex".to_string(),
            Crew {
                name: "codex".to_string(),
                assignment: assignment(TEST_CODEX_MODEL, "codex"),
                description: None,
                tags: Vec::new(),
            },
        );
        registry.insert(
            "claude".to_string(),
            Crew {
                name: "claude".to_string(),
                assignment: assignment(TEST_CLAUDE_WEAK_MODEL, "claude"),
                description: None,
                tags: Vec::new(),
            },
        );
        registry
    }

    #[test]
    fn resolve_crew_returns_assignments_for_known_name() {
        let crew = resolve_crew("codex", &registry()).expect("crew resolves");

        assert_eq!(crew.name, "codex");
        assert_eq!(crew.assignment.model, TEST_CODEX_MODEL);
        assert_eq!(crew.assignment.provider, "codex");
    }

    #[test]
    fn resolve_crew_lists_defined_names_on_unknown() {
        let error = resolve_crew("missing", &registry()).expect_err("unknown crew fails");

        match error {
            IdentityError::InvalidWithSuggestions { did_you_mean, .. } => {
                assert_eq!(did_you_mean, vec!["claude", "codex"]);
            }
            other => panic!("expected InvalidInputDiagnostic, got {other:?}"),
        }
    }

    #[test]
    fn infer_agent_family_from_model_handles_claude_gpt_gemini_grok_prefixes() {
        assert_eq!(
            infer_agent_family_from_model("claude-opus-4-7").as_deref(),
            Some("claude")
        );
        for fable in ["claude-fable-5-1", "fable", "fable-5.1"] {
            assert_eq!(
                infer_agent_family_from_model(fable).as_deref(),
                Some("claude"),
                "{fable}"
            );
        }
        assert_eq!(
            infer_agent_family_from_model("gpt-5.5").as_deref(),
            Some("codex")
        );
        assert_eq!(
            infer_agent_family_from_model("o3-mini").as_deref(),
            Some("codex")
        );
        assert_eq!(
            infer_agent_family_from_model("gemini-3.1-pro").as_deref(),
            Some("gemini")
        );
        assert_eq!(
            infer_agent_family_from_model("grok-4").as_deref(),
            Some("grok")
        );
        assert_eq!(
            infer_agent_family_from_model("grok3").as_deref(),
            Some("grok")
        );
        assert_eq!(
            infer_agent_family_from_model("gemini-3.8-flash-high").as_deref(),
            Some("gemini")
        );
        for family in ["codex", "claude", "gemini", "grok"] {
            assert_eq!(
                infer_agent_family_from_model(family).as_deref(),
                Some(family),
                "{family} is itself a canonical family"
            );
        }
        assert_eq!(infer_agent_family_from_model("llama"), None);
    }

    #[test]
    fn require_canonical_agent_family_normalizes_or_refuses() {
        assert_eq!(
            require_canonical_agent_family(None, Some("gpt-5.5"))
                .expect("full model string")
                .as_deref(),
            Some("codex")
        );
        assert_eq!(
            require_canonical_agent_family(None, Some("claude-opus-4-7"))
                .expect("full model string")
                .as_deref(),
            Some("claude")
        );
        assert_eq!(
            require_canonical_agent_family(None, Some("codex"))
                .expect("canonical family")
                .as_deref(),
            Some("codex")
        );
        assert_eq!(
            require_canonical_agent_family(None, None).expect("absent identity"),
            None
        );

        let error = require_canonical_agent_family(None, Some("llama"))
            .expect_err("unrecognized model is refused");
        let message = error.to_string();
        assert!(
            message.contains("llama"),
            "names the refused value: {message}"
        );
        assert!(
            message.contains("canonical agent family"),
            "explains the contract: {message}"
        );
    }

    #[test]
    fn antigravity_cli_does_not_conflict_with_gemini_model_family() {
        assert_eq!(
            normalize_agent_family_for_model(Some("agy"), Some("gemini-3.8-flash-high"))
                .expect("agy + gemini model")
                .as_deref(),
            Some("gemini")
        );
        assert_eq!(
            normalize_agent_family_for_model(Some("antigravity"), Some("gemini-3.8-flash-high"))
                .expect("antigravity + gemini model")
                .as_deref(),
            Some("gemini")
        );
        assert_eq!(
            normalize_agent_family_for_model(Some("/usr/bin/agy"), Some("claude-sonnet-4-6"))
                .expect("agy can run non-gemini models")
                .as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn antigravity_effort_accepts_low_medium_high_and_rejects_xhigh_max() {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ] {
            effort
                .validate_for_provider_model("antigravity", Some("gemini-3.8-flash-high"))
                .expect("supported effort");
        }
        let err = ReasoningEffort::Xhigh
            .validate_for_provider_model("antigravity", Some("gemini-3.8-flash-high"))
            .expect_err("xhigh is unsupported");
        assert!(err.contains("low, medium, high"), "{err}");
        assert!(err.contains("not remapped"), "{err}");
        let legacy = ReasoningEffort::High
            .validate_for_provider_model("antigravity", Some("gemini-3.8-flash"))
            .expect_err("legacy gemini CLI id is not remapped");
        assert!(legacy.contains("gemini-3.8-flash-high"), "{legacy}");
        assert!(legacy.contains("not remapped"), "{legacy}");
    }
}
