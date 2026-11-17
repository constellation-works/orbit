use chrono::Utc;
use orbit_types::task::{
    GuardOutcome, LIGHT_DELIVERY_PATHS_KEY, LightDeliveryDecision, LightDeliveryJudgement,
    RecordedLightDeliveryDecision, SKIP_REVIEW_TAG,
};

#[test]
fn recorded_outside_paths_decode_to_their_replay_keys() {
    for (first, count) in [
        ("docs/old` is outside `wrong.md", 1),
        ("docs/one` is outside `wrong.md", 2),
        ("docs/one other changed path(s) are outside docs/two.md", 2),
    ] {
        let decision = LightDeliveryDecision {
            tag: SKIP_REVIEW_TAG.into(),
            candidate: "abcdef0123456789".into(),
            judgement: LightDeliveryJudgement::Outside {
                first: first.into(),
                count,
            },
            untagged: Vec::new(),
            profile_key: LIGHT_DELIVERY_PATHS_KEY.into(),
            guard: GuardOutcome::NotConfigured,
        };
        let record = RecordedLightDeliveryDecision::from_decision(&decision);
        let entry = record.history_entry(Utc::now(), "system");

        let decoded = RecordedLightDeliveryDecision::decode(&entry)
            .expect("a note produced by the shared history encoder must decode");
        assert_eq!(decoded.replay_key(), record.replay_key());
    }
}

#[test]
fn replay_key_equality_keeps_ambiguous_legacy_notes_equivalent() {
    let with_delimiter_in_path = LightDeliveryDecision {
        tag: SKIP_REVIEW_TAG.into(),
        candidate: "abcdef0123456789".into(),
        judgement: LightDeliveryJudgement::Outside {
            first: "docs/old` is outside `wrong.md".into(),
            count: 1,
        },
        untagged: Vec::new(),
        profile_key: LIGHT_DELIVERY_PATHS_KEY.into(),
        guard: GuardOutcome::NotConfigured,
    };
    let with_delimiter_in_profile = LightDeliveryDecision {
        profile_key: "wrong.md` is outside `workflow.light_delivery_paths".into(),
        judgement: LightDeliveryJudgement::Outside {
            first: "docs/old".into(),
            count: 1,
        },
        ..with_delimiter_in_path.clone()
    };
    let path_record = RecordedLightDeliveryDecision::from_decision(&with_delimiter_in_path);
    let profile_record = RecordedLightDeliveryDecision::from_decision(&with_delimiter_in_profile);

    assert_eq!(path_record.note(), profile_record.note());
    assert_eq!(path_record.event(), profile_record.event());
    assert_eq!(path_record.replay_key(), profile_record.replay_key());
}
