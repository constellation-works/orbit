use crate::identity::{
    REGISTRY_IDENTIFIER_MAX_BYTES, validate_machine_id, validate_machine_name,
    validate_new_task_prefix, validate_registry_identifier, validate_stored_task_prefix,
};

#[test]
fn machine_id_validation_keeps_transport_targets_out_of_the_identity_namespace() {
    for accepted in ["hm_a", "hm_owner", "hm_9f2c81d4", "hm_0123456789abcdef"] {
        validate_machine_id(accepted).expect("compatible generated/test machine id");
    }
    for rejected in [
        "",
        "hm_",
        "dk1",
        "user@dk1",
        "ssh:dk1",
        "hm_ssh:dk1",
        "hm_path/name",
        " hm_owner",
    ] {
        let error = validate_machine_id(rejected)
            .expect_err("transport-shaped machine id must fail")
            .to_string();
        assert!(error.contains("machine_id"), "unexpected: {error}");
    }
}

#[test]
fn registry_identifiers_are_normalized_path_free_and_bounded() {
    validate_machine_name("build-host").expect("normalized machine name");
    validate_registry_identifier("workspace_id", "ws_alpha").expect("logical workspace id");

    for rejected in [
        " workspace",
        "workspace ",
        "workspace/path",
        "workspace\\path",
        "workspace\nname",
    ] {
        assert!(
            validate_registry_identifier("workspace_id", rejected).is_err(),
            "identifier should fail: {rejected:?}"
        );
    }
    assert!(
        validate_registry_identifier(
            "workspace_id",
            &"a".repeat(REGISTRY_IDENTIFIER_MAX_BYTES + 1)
        )
        .is_err()
    );
}

#[test]
fn task_prefix_validation_reserves_orbit_artifact_namespaces_for_a_fresh_machine() {
    for accepted in ["DK", "ACME", "WXYZ"] {
        validate_new_task_prefix(accepted).expect("selectable namespace");
    }
    for reserved in ["ORB", "ADR", "L", "F"] {
        let error = validate_new_task_prefix(reserved)
            .expect_err("reserved namespace must not be selectable")
            .to_string();
        assert!(error.contains("reserved"), "unexpected: {error}");
    }
    for malformed in ["", "a", "abc", "TOOLONGG", "OR8"] {
        assert!(
            validate_new_task_prefix(malformed).is_err(),
            "malformed prefix should fail: {malformed:?}"
        );
    }
    // A machine that predates the prefix choice keeps minting under ORB.
    assert_eq!(
        validate_stored_task_prefix("ORB").expect("persisted legacy namespace"),
        "ORB"
    );
    assert!(validate_stored_task_prefix("ADR").is_err());
}
