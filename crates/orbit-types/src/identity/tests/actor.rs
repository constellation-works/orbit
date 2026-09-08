mod mapping {
    use super::super::super::actor::{agent_from_model, provider_from_model};

    #[test]
    fn agent_from_model_maps_known_prefixes() {
        assert_eq!(agent_from_model("claude-opus-4-7"), Some("claude"));
        assert_eq!(agent_from_model("claude-fable-5-1"), Some("claude"));
        assert_eq!(agent_from_model("fable"), Some("claude"));
        assert_eq!(agent_from_model("fable-5.1"), Some("claude"));
        assert_eq!(agent_from_model("gpt-5.5"), Some("codex"));
        assert_eq!(agent_from_model("gemini-3.1-pro-preview"), Some("gemini"));
        assert_eq!(agent_from_model("ollama:llama3.2"), Some("ollama"));
        assert_eq!(agent_from_model("grok-4"), Some("grok"));
        assert_eq!(agent_from_model("grok3-latest"), Some("grok"));
    }

    #[test]
    fn agent_from_model_returns_none_for_unknown_prefix() {
        assert_eq!(agent_from_model("unknown-model"), None);
        assert_eq!(agent_from_model(""), None);
    }

    #[test]
    fn provider_from_model_maps_known_prefixes() {
        assert_eq!(provider_from_model("claude-sonnet-4-7"), Some("anthropic"));
        assert_eq!(provider_from_model("gpt-5.5"), Some("openai"));
        assert_eq!(provider_from_model("gemini-3-pro"), Some("google"));
        assert_eq!(provider_from_model("ollama:mistral"), Some("ollama"));
        assert_eq!(provider_from_model("grok-4"), Some("xai"));
        assert_eq!(provider_from_model("grok3-mini"), Some("xai"));
        assert_eq!(provider_from_model("unknown-model"), None);
    }
}

mod serde_round_trip {
    use super::super::super::actor::ActorIdentity;

    fn round_trip(actor: &ActorIdentity) -> ActorIdentity {
        let encoded = serde_json::to_string(actor).expect("serialize actor");
        serde_json::from_str(&encoded).expect("deserialize actor")
    }

    #[test]
    fn every_variant_survives_a_round_trip() {
        for actor in [
            ActorIdentity::human("daniel"),
            ActorIdentity::agent("claude / opus"),
            ActorIdentity::agent("gpt-5"),
            ActorIdentity::System,
        ] {
            assert_eq!(round_trip(&actor), actor, "round trip changed {actor:?}");
        }
    }

    #[test]
    fn unambiguous_agent_models_stay_flat_strings() {
        let encoded = serde_json::to_string(&ActorIdentity::agent("gpt-5")).expect("serialize");
        assert_eq!(encoded, r#""gpt-5""#);
        assert_eq!(
            serde_json::to_string(&ActorIdentity::System).expect("serialize"),
            r#""system""#
        );
    }

    #[test]
    fn variants_the_flat_label_cannot_represent_use_the_tagged_form() {
        assert_eq!(
            serde_json::to_string(&ActorIdentity::human("daniel")).expect("serialize"),
            r#"{"human":"daniel"}"#
        );
        assert_eq!(
            serde_json::to_string(&ActorIdentity::agent("claude / opus")).expect("serialize"),
            r#"{"agent":{"model":"claude / opus"}}"#
        );
        // A model that collides with a reserved flat label must not read back
        // as `System` or `Human`.
        for model in ["system", "human"] {
            assert_eq!(
                round_trip(&ActorIdentity::agent(model)),
                ActorIdentity::agent(model)
            );
        }
    }

    #[test]
    fn legacy_encodings_still_deserialize() {
        let cases = [
            (r#""system""#, ActorIdentity::System),
            (r#""gpt-5""#, ActorIdentity::agent("gpt-5")),
            (r#""claude / opus""#, ActorIdentity::agent("opus")),
            (r#""human""#, ActorIdentity::human("human")),
            (
                r#"{"agent":{"name":"claude","model":"opus"}}"#,
                ActorIdentity::agent("opus"),
            ),
            (
                r#"{"agent":{"name":"claude"}}"#,
                ActorIdentity::agent("claude"),
            ),
            (r#"{"human":"daniel"}"#, ActorIdentity::human("daniel")),
        ];
        for (encoded, expected) in cases {
            let decoded: ActorIdentity = serde_json::from_str(encoded).expect("deserialize legacy");
            assert_eq!(decoded, expected, "decoding {encoded}");
        }
    }
}
