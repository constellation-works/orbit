use orbit_common::OrbitError;
use orbit_engine::BaseSyncMode;
use serde_json::json;

#[test]
fn job_base_sync_uses_one_trimmed_defaulting_parse() {
    for (input, expected) in [
        (json!({}), BaseSyncMode::Remote),
        (json!({"base_sync": null}), BaseSyncMode::Remote),
        (json!({"base_sync": 7}), BaseSyncMode::Remote),
        (json!({"base_sync": ""}), BaseSyncMode::Remote),
        (json!({"base_sync": " \t "}), BaseSyncMode::Remote),
        (json!({"base_sync": " remote "}), BaseSyncMode::Remote),
        (json!({"base_sync": "local"}), BaseSyncMode::Local),
        (json!({"base_sync": " local\n"}), BaseSyncMode::Local),
    ] {
        assert_eq!(BaseSyncMode::from_input(&input).unwrap(), expected);
        if input["base_sync"].is_string() {
            assert_eq!(
                serde_json::from_value::<BaseSyncMode>(input["base_sync"].clone()).unwrap(),
                expected
            );
        }
    }
    for value in ["unknown", "LOCAL", "local remote"] {
        assert!(matches!(
            BaseSyncMode::from_input(&json!({"base_sync": value})),
            Err(OrbitError::InvalidInput(_))
        ));
        assert!(serde_json::from_value::<BaseSyncMode>(json!(value)).is_err());
    }
}
