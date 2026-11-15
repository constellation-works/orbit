//! Lease token security invariants at the crate's public boundary: the store
//! keeps only a hash, verification returns the scope or a typed refusal, a
//! revocation holds on the next use, and a missing or shared store is refused.
#![cfg(unix)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "token fixtures panic on a bad permission, clock or store path so the failure names that step"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenRefusal, LeaseTokenScope, LeaseTokenVerb,
    PrincipalClass,
};
use orbit_auth::store::{
    LEASE_TOKEN_STORE_FILE, LeaseTokenDecision, LeaseTokenRevocation, LeaseTokenStore,
};

/// A listener-style Orbit home: `0700`, holding a provisioned `0600` store.
fn provisioned_home(root: &Path) -> PathBuf {
    let home = root.join(".orbit");
    fs::create_dir(&home).expect("create home");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("chmod home");
    let store = home.join(LEASE_TOKEN_STORE_FILE);
    fs::write(&store, b"").expect("provision store");
    fs::set_permissions(&store, fs::Permissions::from_mode(0o600)).expect("chmod store");
    home
}

fn scope(principal: &str, class: PrincipalClass, verb: LeaseTokenVerb) -> LeaseTokenScope {
    LeaseTokenScope {
        principal: principal.to_string(),
        principal_class: class,
        workspaces: vec!["ws_alpha".to_string()],
        verbs: verb.granted_by(),
        max_priority: LeasePriorityCeiling::Normal,
        hard: HardLeasePermission::Never,
    }
}

/// Every byte the store has written: the database and its sidecars.
fn store_bytes(home: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in fs::read_dir(home).expect("read home") {
        bytes.extend(fs::read(entry.expect("entry").path()).expect("read store file"));
    }
    bytes
}

#[test]
fn a_minted_token_is_stored_as_a_hash_and_verified_until_revoked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = provisioned_home(dir.path());
    let mut store = LeaseTokenStore::open_in_orbit_home(&home).expect("open store");
    let now = Utc::now();
    let expires_at = now + Duration::hours(1);

    let human = store
        .mint(
            scope("alice", PrincipalClass::Human, LeaseTokenVerb::Lease),
            expires_at,
            "operator",
            now,
        )
        .expect("mint human token");
    let reader = store
        .mint(
            scope(
                "alice/claude-code",
                PrincipalClass::Agent,
                LeaseTokenVerb::Read,
            ),
            expires_at,
            "operator",
            now,
        )
        .expect("mint agent token");
    let secret = human.secret.expose_secret().to_string();
    let reader_secret = reader.secret.expose_secret().to_string();
    assert!(
        secret.starts_with("orbl_") && secret.len() == 5 + 64,
        "{secret}"
    );
    assert!(
        !format!("{human:?}").contains(&secret),
        "Debug leaks the secret"
    );

    let mode = fs::metadata(home.join(LEASE_TOKEN_STORE_FILE))
        .expect("store metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    match store
        .verify(&secret, "ws_alpha", LeaseTokenVerb::Lease, now)
        .expect("verify")
    {
        LeaseTokenDecision::Allow(record) => {
            assert_eq!(record.token_id, human.record.token_id);
            assert_eq!(record.scope.principal, "alice");
            assert_eq!(record.scope.principal_class, PrincipalClass::Human);
            assert_eq!(record.scope.workspaces, vec!["ws_alpha".to_string()]);
            assert_eq!(record.scope.hard, HardLeasePermission::Never);
            assert_eq!(record.last_used_at, Some(now));
        }
        other => panic!("valid token refused: {other:?}"),
    }

    let refusals = [
        (secret.as_str(), "ws_beta", LeaseTokenVerb::Read, now),
        (
            reader_secret.as_str(),
            "ws_alpha",
            LeaseTokenVerb::Lease,
            now,
        ),
        ("orbl_not-a-token", "ws_alpha", LeaseTokenVerb::Read, now),
        (
            secret.as_str(),
            "ws_alpha",
            LeaseTokenVerb::Read,
            expires_at + Duration::seconds(1),
        ),
    ];
    let expected = [
        LeaseTokenRefusal::WrongWorkspace,
        LeaseTokenRefusal::VerbNotGranted,
        LeaseTokenRefusal::Unknown,
        LeaseTokenRefusal::Expired,
    ];
    for ((presented, workspace, verb, at), refusal) in refusals.into_iter().zip(expected) {
        assert_eq!(
            store
                .verify(presented, workspace, verb, at)
                .expect("verify refusal"),
            LeaseTokenDecision::Deny(refusal)
        );
    }

    let revoked = store
        .revoke(&human.record.token_id, "operator", now)
        .expect("revoke");
    assert!(matches!(revoked, LeaseTokenRevocation::Revoked(_)));
    assert_eq!(
        store
            .verify(&secret, "ws_alpha", LeaseTokenVerb::Read, now)
            .expect("verify revoked"),
        LeaseTokenDecision::Deny(LeaseTokenRefusal::Revoked)
    );

    let listed = store.list().expect("list");
    assert_eq!(listed.len(), 2);
    let listed_text = format!("{listed:?}");
    assert!(listed.iter().any(|record| record.revoked_at.is_some()));
    drop(store);

    // Neither the store (tokens and audit rows) nor the listing holds a secret.
    let written = String::from_utf8_lossy(&store_bytes(&home)).into_owned();
    for minted in [&secret, &reader_secret] {
        let random_part = &minted["orbl_".len()..];
        assert!(!written.contains(random_part), "the store holds a secret");
        assert!(
            !listed_text.contains(random_part),
            "the listing shows a secret"
        );
    }
}

#[test]
fn fractional_expiry_survives_listing_reopening_and_verification() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = provisioned_home(dir.path());
    let minted_at = DateTime::parse_from_rfc3339("2026-10-26T10:25:34.900Z")
        .expect("fixed mint instant")
        .with_timezone(&Utc);
    let before_expiry = minted_at + Duration::milliseconds(200);
    let expires_at = minted_at + Duration::seconds(1);
    let mut store = LeaseTokenStore::open_in_orbit_home(&home).expect("open store");
    let minted = store
        .mint(
            scope("alice", PrincipalClass::Human, LeaseTokenVerb::Lease),
            expires_at,
            "operator",
            minted_at,
        )
        .expect("mint one-second token");
    let secret = minted.secret.expose_secret().to_string();

    assert_eq!(minted.record.expires_at, expires_at);
    assert_eq!(store.list().expect("list token")[0].expires_at, expires_at);
    drop(store);

    let mut reopened = LeaseTokenStore::open_in_orbit_home(&home).expect("reopen store");
    assert_eq!(
        reopened.list().expect("list reopened token")[0].expires_at,
        expires_at
    );
    assert!(matches!(
        reopened
            .verify(
                &secret,
                "ws_alpha",
                LeaseTokenVerb::Lease,
                before_expiry,
            )
            .expect("verify before expiry"),
        LeaseTokenDecision::Allow(record) if record.expires_at == expires_at
    ));
    for at in [expires_at, expires_at + Duration::nanoseconds(1)] {
        assert_eq!(
            reopened
                .verify(&secret, "ws_alpha", LeaseTokenVerb::Lease, at)
                .expect("verify expired token"),
            LeaseTokenDecision::Deny(LeaseTokenRefusal::Expired)
        );
    }
}

#[test]
fn a_missing_or_shared_store_is_refused_and_never_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let operator_home = dir.path().join("operator");
    fs::create_dir(&operator_home).expect("create operator home");
    fs::set_permissions(&operator_home, fs::Permissions::from_mode(0o700)).expect("chmod");

    let missing = LeaseTokenStore::open_in_orbit_home(&operator_home)
        .err()
        .expect("a home without a store is refused")
        .to_string();
    assert!(missing.contains("orbit-lease"), "{missing}");
    assert!(!operator_home.join(LEASE_TOKEN_STORE_FILE).exists());

    let shared_dir = tempfile::tempdir().expect("tempdir");
    let home = provisioned_home(shared_dir.path());
    let store = home.join(LEASE_TOKEN_STORE_FILE);
    fs::set_permissions(&store, fs::Permissions::from_mode(0o644)).expect("chmod store");
    let readable = LeaseTokenStore::open_in_orbit_home(&home)
        .err()
        .expect("a world-readable store is refused")
        .to_string();
    assert!(readable.contains("644"), "{readable}");

    fs::set_permissions(&store, fs::Permissions::from_mode(0o600)).expect("chmod store");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o750)).expect("chmod home");
    let shared_home = LeaseTokenStore::open_in_orbit_home(&home)
        .err()
        .expect("a group-accessible home is refused")
        .to_string();
    assert!(shared_home.contains("750"), "{shared_home}");
}

#[test]
fn listener_rejection_snapshots_upsert_one_row_per_key_limit_and_window() {
    let dir = tempfile::tempdir().unwrap();
    let home = provisioned_home(dir.path());
    let mut store = LeaseTokenStore::open_in_orbit_home(&home).unwrap();
    let now = Utc::now();
    let mut row = orbit_auth::limiter::LimitAudit {
        key: "address:192.0.2.1".into(),
        limit: "address_requests",
        first_at: now,
        last_at: now,
        count: 1,
    };
    store.record_limit_audits(&[row.clone()]).unwrap();
    row.count = 10_000;
    row.last_at = now + Duration::seconds(30);
    store.record_limit_audits(&[row.clone()]).unwrap();
    // A delayed older snapshot must never decrease a newer count or last time.
    row.count = 2;
    row.last_at = now + Duration::seconds(1);
    store.record_limit_audits(&[row]).unwrap();
    let connection = rusqlite::Connection::open(home.join(LEASE_TOKEN_STORE_FILE)).unwrap();
    let (rows, count, first_at, last_at): (u64, u64, String, String) = connection
        .query_row(
            "SELECT count(*), sum(count), first_at, last_at FROM lease_listener_limit_audit",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((rows, count), (1, 10_000));
    assert_eq!(DateTime::parse_from_rfc3339(&first_at).unwrap(), now);
    assert_eq!(
        DateTime::parse_from_rfc3339(&last_at).unwrap(),
        now + Duration::seconds(30)
    );
}
