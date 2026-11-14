use orbit_types::workflow::activity_job::Provider;

/// [ORB-18096] Persisted records naming retired providers still read and
/// display, while fresh parses refuse them as retired (not unknown).
#[test]
fn retired_provider_ids_still_deserialize_but_do_not_parse_for_dispatch() {
    for (raw, expected) in [
        ("cursor", Provider::Cursor),
        ("pi", Provider::Pi),
        ("opencode", Provider::Opencode),
    ] {
        let persisted: Provider = serde_json::from_str(&format!(r#""{raw}""#))
            .unwrap_or_else(|error| panic!("persisted {raw} must deserialize: {error}"));
        assert_eq!(persisted, expected);
        assert_eq!(persisted.as_str(), raw, "display keeps the retired id");
        assert!(persisted.is_retired());
        assert!(!persisted.has_cli_runtime());
        let error = Provider::parse(raw).expect_err("dispatch parse must refuse retired");
        let message = error.to_string();
        assert!(message.contains("retired"), "{message}");
        assert!(message.contains(raw), "{message}");
        assert!(!Provider::ALL.contains(&expected));
    }
    // A persisted crew assignment (provider is a plain string) still reads.
    let assignment: orbit_types::identity::CrewAssignment =
        serde_json::from_str(r#"{"model":"old","provider":"cursor"}"#)
            .expect("persisted crew assignment reads");
    assert_eq!(assignment.provider, "cursor");
}
