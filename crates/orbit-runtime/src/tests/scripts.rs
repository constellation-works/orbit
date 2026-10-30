//! Script grant persistence at the store/witness boundary: a grant row
//! round-trips, verifies only with its witness, and a tampered or
//! witness-less row is refused.

use orbit_store::Store;
use orbit_types::workflow::activity_job::{
    ScriptGrant, ScriptGrantLimits, ScriptHostRole, ScriptNetwork, script_grant_id,
    script_sha256_hex,
};

use crate::scripts::{
    forget_script_grant_witness, record_script_grant_witness, verify_script_grant_witness,
};

fn fixture_grant() -> (ScriptGrant, Vec<u8>, Vec<u8>) {
    let manifest = b"schemaVersion: 1\nkind: Script\n".to_vec();
    let entry = b"#!/bin/sh\nexit 0\n".to_vec();
    let digest = orbit_types::workflow::activity_job::script_content_digest(&manifest, &entry);
    let grant = ScriptGrant {
        grant_id: script_grant_id("ws_fixture", "package", &digest),
        workspace_id: "ws_fixture".to_string(),
        name: "package".to_string(),
        manifest_digest: script_sha256_hex(&manifest),
        entry_digest: script_sha256_hex(&entry),
        digest,
        interpreter_paths: vec!["/bin/sh".into()],
        network: ScriptNetwork::None,
        read_roots: Vec::new(),
        write_roots: vec!["/srv/backups".into()],
        env_pass: Vec::new(),
        limits: ScriptGrantLimits::default(),
        host_role: ScriptHostRole::Owner,
        grantor: "operator".to_string(),
        granted_at: "2026-10-29T00:00:00Z".to_string(),
        provenance: "cli".to_string(),
        expires_at: None,
    };
    (grant, manifest, entry)
}

#[test]
fn script_grant_round_trips_and_verifies_only_with_its_witness() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("orbit.db")).expect("store opens");
    let (grant, manifest, entry) = fixture_grant();

    store.upsert_script_grant(&grant).expect("grant row writes");
    let stored = store
        .get_script_grant("ws_fixture", "package")
        .expect("grant reads")
        .expect("grant exists");
    assert_eq!(stored, grant);

    // A row without a witness is refused, not authority.
    let refused =
        verify_script_grant_witness(dir.path(), &stored).expect_err("witness-less row refuses");
    assert!(refused.contains("no authorization witness"), "{refused}");

    record_script_grant_witness(dir.path(), &grant, &manifest, &entry).expect("witness writes");
    verify_script_grant_witness(dir.path(), &stored).expect("witnessed row verifies");

    // A row widened after granting (the forged-row shape) no longer
    // matches its witness.
    let mut widened = grant.clone();
    widened.network = ScriptNetwork::Any;
    store
        .upsert_script_grant(&widened)
        .expect("widened row writes");
    let stored_widened = store
        .get_script_grant("ws_fixture", "package")
        .expect("widened grant reads")
        .expect("widened grant exists");
    let refused =
        verify_script_grant_witness(dir.path(), &stored_widened).expect_err("widened row refuses");
    assert!(refused.contains("does not match"), "{refused}");

    // Revocation's halves: deleting the row and forgetting the witness
    // leaves nothing behind.
    assert!(
        store
            .delete_script_grant("ws_fixture", "package")
            .expect("grant deletes")
    );
    forget_script_grant_witness(dir.path(), "ws_fixture", "package");
    assert!(
        store
            .get_script_grant("ws_fixture", "package")
            .expect("grant reads after delete")
            .is_none()
    );
}
