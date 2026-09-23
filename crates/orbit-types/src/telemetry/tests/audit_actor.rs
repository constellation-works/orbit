use crate::telemetry::{ACTOR_ALIAS_MAP_VERSION, ActorKind, canonical_actor_for_role_label};

/// The defect this module exists to fix: one actor recorded at three
/// granularities must aggregate as one actor.
#[test]
fn agent_granularities_collapse_to_one_canonical_actor() {
    for label in [
        "claude",
        "opus",
        "sonnet",
        "claude-opus-5",
        "claude-sonnet-5",
        "fable",
        "fable-5.1",
        "claude-fable-5-1",
    ] {
        let actor = canonical_actor_for_role_label(label);
        assert_eq!(actor.kind, ActorKind::Agent, "{label}");
        assert_eq!(actor.id, "claude", "{label}");
        assert_eq!(actor.family.as_deref(), Some("claude"), "{label}");
        assert_eq!(actor.vendor.as_deref(), Some("anthropic"), "{label}");
    }

    for label in ["codex", "gpt-6-sol", "gpt-6-luna", "gpt-5.4-mini"] {
        let actor = canonical_actor_for_role_label(label);
        assert_eq!(actor.id, "codex", "{label}");
        assert_eq!(actor.vendor.as_deref(), Some("openai"), "{label}");
        if label.starts_with("gpt-6-") {
            assert_eq!(actor.family.as_deref(), Some("codex"), "{label}");
            assert_eq!(actor.model.as_deref(), Some(label), "{label}");
        }
    }

    let grok = canonical_actor_for_role_label("grok-4.6");
    assert_eq!(grok.id, "grok");
    assert_eq!(grok.vendor.as_deref(), Some("xai"));
}

/// Collapsing granularity must not erase it: the model stays retrievable.
#[test]
fn model_remains_retrievable_after_family_collapse() {
    let with_model = canonical_actor_for_role_label("claude-opus-5");
    assert_eq!(with_model.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(with_model.family.as_deref(), Some("claude"));

    // A bare family name carries no model, and must not invent one.
    let bare_family = canonical_actor_for_role_label("claude");
    assert_eq!(bare_family.model, None);
    assert_eq!(bare_family.family.as_deref(), Some("claude"));
}

#[test]
fn synthetic_and_human_labels_are_kinded_away_from_agents() {
    let cases = [
        ("admin", ActorKind::System),
        ("system", ActorKind::System),
        ("hook", ActorKind::Hook),
        ("human", ActorKind::Human),
        ("unknown", ActorKind::Unattributed),
        ("unverified", ActorKind::Unattributed),
        ("agent", ActorKind::Unattributed),
    ];

    for (label, expected) in cases {
        let actor = canonical_actor_for_role_label(label);
        assert_eq!(actor.kind, expected, "{label}");
        assert!(!actor.is_agent(), "{label} must not count as an agent");
        // Non-agents keep their own identity rather than being merged.
        assert_eq!(actor.id, label, "{label}");
        assert_eq!(actor.family, None, "{label}");
        assert_eq!(actor.vendor, None, "{label}");
        assert_eq!(actor.model, None, "{label}");
    }
}

/// `unverified` is a trust marker. Resolving it to an agent would silently
/// promote an unauthenticated caller into the agent population.
#[test]
fn unverified_is_never_resolved_into_an_agent() {
    let actor = canonical_actor_for_role_label("unverified");
    assert_eq!(actor.kind, ActorKind::Unattributed);
    assert_eq!(actor.id, "unverified");
    assert!(actor.family.is_none());
}

#[test]
fn labels_are_matched_case_insensitively_and_trimmed() {
    assert_eq!(canonical_actor_for_role_label("  Admin ").id, "admin");
    assert_eq!(canonical_actor_for_role_label("CLAUDE").id, "claude");
    assert_eq!(
        canonical_actor_for_role_label(" Claude-Opus-5 ")
            .family
            .as_deref(),
        Some("claude")
    );
    // The model preserves the label's original casing; only matching folds it.
    assert_eq!(
        canonical_actor_for_role_label(" Claude-Opus-5 ")
            .model
            .as_deref(),
        Some("Claude-Opus-5")
    );
}

#[test]
fn blank_label_is_unattributed_unknown() {
    for label in ["", "   "] {
        let actor = canonical_actor_for_role_label(label);
        assert_eq!(actor.kind, ActorKind::Unattributed);
        assert_eq!(actor.id, "unknown");
    }
}

/// An agent model Orbit does not recognize stays its own agent row rather than
/// being folded into an unrelated bucket or dropped.
#[test]
fn unrecognized_model_stays_a_distinct_agent() {
    let actor = canonical_actor_for_role_label("some-future-model-9");
    assert_eq!(actor.kind, ActorKind::Agent);
    assert_eq!(actor.id, "some-future-model-9");
    assert_eq!(actor.model.as_deref(), Some("some-future-model-9"));
    assert_eq!(actor.family, None);
    assert_eq!(actor.vendor, None);
}

#[test]
fn every_derivation_is_stamped_with_the_alias_map_version() {
    for label in ["claude", "admin", "", "unverified", "gpt-6-luna"] {
        assert_eq!(
            canonical_actor_for_role_label(label).alias_version,
            ACTOR_ALIAS_MAP_VERSION,
            "{label}"
        );
    }
}

/// Resolution must be a pure function of the label, or a re-run of an
/// aggregate over old rows would not reproduce.
#[test]
fn resolution_is_deterministic() {
    for label in ["claude-opus-5", "admin", "unknown", "grok-4.6"] {
        assert_eq!(
            canonical_actor_for_role_label(label),
            canonical_actor_for_role_label(label),
            "{label}"
        );
    }
}

#[test]
fn actor_kind_round_trips_through_its_wire_string() {
    for kind in [
        ActorKind::Human,
        ActorKind::Agent,
        ActorKind::System,
        ActorKind::Hook,
        ActorKind::Unattributed,
    ] {
        assert_eq!(kind.as_str().parse::<ActorKind>(), Ok(kind));
    }
}
