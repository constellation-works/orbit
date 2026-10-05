//! Generation admission forgets participants that have exited.
//!
//! `.generation-compat.json` is the envelope of identities admitted since the
//! authority last had no holder of `.generation.lock`. A guard drop does not
//! rewrite that file; the next join must, or a departed writer keeps refusing
//! newcomers and forcing live, compatible processes to yield.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use orbit_common::fs::generation::{
    Access, CompatibilityIdentity, GenerationGuard, LedgerCompatibility, Participant,
    ParticipantRole, pending_switch,
};
use serde_json::{Value, json};

fn digest(n: u64) -> String {
    format!("{n:064x}")
}

fn identity(version: u32, writer_floor: u32) -> CompatibilityIdentity {
    CompatibilityIdentity {
        store_schema: LedgerCompatibility {
            version,
            writer_floor,
            reader_floor: 0,
        },
        workspace_layout: LedgerCompatibility {
            version: 1,
            writer_floor: 0,
            reader_floor: 0,
        },
        features: BTreeMap::new(),
    }
}

fn join(
    root: &Path,
    digest: &str,
    identity: &CompatibilityIdentity,
    role: ParticipantRole,
    access: Access,
) -> Result<GenerationGuard, orbit_common::OrbitError> {
    let participant = Participant {
        digest,
        identity,
        role,
        access,
    };
    // A zero bound fails immediately if join decides to quiesce. Success
    // therefore means no live participant was asked to yield.
    GenerationGuard::join(root, &participant, Duration::ZERO, || Ok(0))
}

fn compat(root: &Path) -> Value {
    let raw = std::fs::read_to_string(root.join(".generation-compat.json")).expect("compat record");
    serde_json::from_str(&raw).expect("compat json")
}

fn assert_envelope(root: &Path, store: Value, writer_min_layout: Option<u32>) {
    let record = compat(root);
    let lock = std::fs::read_to_string(root.join(".generation.lock")).expect("generation lock");
    let recorded = lock
        .strip_prefix("1:")
        .and_then(|body| body.strip_suffix('\n'))
        .expect("v1 generation record");
    assert_eq!(record["contract"], "compatibility-generation-v2");
    assert_eq!(record["record_digest"], recorded);
    assert_eq!(
        record["envelope"],
        json!({
            "store_schema": store,
            "workspace_layout": {
                "min_version": 1,
                "max_version": 1,
                "max_reader_floor": 0,
                "max_writer_floor": 0,
                "writer_min_version": writer_min_layout,
            },
            "features": {}
        })
    );
}

fn store(
    min_version: u32,
    max_version: u32,
    max_writer_floor: u32,
    writer_min_version: Option<u32>,
) -> Value {
    json!({
        "min_version": min_version,
        "max_version": max_version,
        "max_reader_floor": 0,
        "max_writer_floor": max_writer_floor,
        "writer_min_version": writer_min_version,
    })
}

#[test]
fn exited_writer_does_not_refuse_a_later_reader() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let old = identity(10, 0);
    let old_guard = join(
        root,
        &digest(10),
        &old,
        ParticipantRole::Command,
        Access::Write,
    )
    .expect("v10 writer joins");
    assert_envelope(root, store(10, 10, 0, Some(10)), Some(1));
    drop(old_guard);

    let reader = identity(12, 12);
    let reader_guard = join(
        root,
        &digest(12),
        &reader,
        ParticipantRole::Command,
        Access::ReadOnly,
    )
    .expect("read-only v12 joins after the v10 writer has exited");
    assert_envelope(root, store(12, 12, 12, None), None);
    drop(reader_guard);
}

#[test]
fn exited_writer_does_not_force_a_compatible_live_participant_to_yield() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let old = identity(10, 0);
    drop(
        join(
            root,
            &digest(10),
            &old,
            ParticipantRole::Command,
            Access::Write,
        )
        .expect("v10 writer joins"),
    );

    let mid = identity(11, 0);
    let live = join(
        root,
        &digest(11),
        &mid,
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("additive v11 writer joins an empty authority");
    assert_envelope(root, store(11, 11, 0, Some(11)), Some(1));
    assert!(
        pending_switch(root).is_none(),
        "reseeding an empty authority records no switch"
    );

    let newer = identity(12, 11);
    let admitted = join(
        root,
        &digest(12),
        &newer,
        ParticipantRole::Command,
        Access::Write,
    )
    .expect("v12 writer joins beside the live v11 process");
    assert!(
        pending_switch(root).is_none(),
        "a departed v10 writer must not ask the live v11 process to yield"
    );
    assert_envelope(root, store(11, 12, 11, Some(11)), Some(1));
    drop(admitted);
    drop(live);
}

#[test]
fn live_writer_is_not_treated_as_an_empty_authority() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let old = identity(10, 0);
    let live = join(
        root,
        &digest(10),
        &old,
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("v10 writer holds the authority");

    let breaking = identity(12, 12);
    let refused = match join(
        root,
        &digest(12),
        &breaking,
        ParticipantRole::Command,
        Access::Write,
    ) {
        Ok(_guard) => panic!("a live v10 writer still refuses a breaking v12 writer"),
        Err(error) => error.to_string(),
    };
    assert!(refused.contains("live writer at version 10"), "{refused}");
    assert!(refused.contains("did not yield"), "{refused}");
    assert!(pending_switch(root).is_none(), "{refused}");
    assert_envelope(root, store(10, 10, 0, Some(10)), Some(1));
    drop(live);
}
