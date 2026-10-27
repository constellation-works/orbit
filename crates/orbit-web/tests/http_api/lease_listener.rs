//! The lease listener's guarantees at its public router: only lease routes
//! and `/healthz`, a token on every lease request, the client-address rule,
//! the failed-authentication limit and audit rows without secrets. Each test
//! uses its own temporary token store and touches no Orbit state.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use orbit_auth::client_address::{LeaseClientAddressPolicy, TrustedProxies};
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenScope, LeaseTokenVerb, PrincipalClass,
};
use orbit_auth::store::LeaseTokenStore;
use serde_json::{Value, json};
use tower::ServiceExt;

use orbit_web::lease_listener::{LeaseListenerConfig, lease_listener_router};

const PEER: &str = "192.0.2.10:50000";

pub(super) fn provisioned_store(home: &Path) -> LeaseTokenStore {
    std::fs::create_dir_all(home).unwrap();
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = home.join("lease-tokens.db");
    std::fs::write(&store, b"").unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).unwrap();
    LeaseTokenStore::open_in_orbit_home(home).expect("open token store")
}

pub(super) fn mint(
    store: &mut LeaseTokenStore,
    principal: &str,
    class: PrincipalClass,
    workspaces: &[&str],
    verb: LeaseTokenVerb,
    max_priority: LeasePriorityCeiling,
    hard: HardLeasePermission,
) -> (String, String) {
    let now = Utc::now();
    let minted = store
        .mint(
            LeaseTokenScope {
                principal: principal.to_string(),
                principal_class: class,
                workspaces: workspaces.iter().map(|ws| ws.to_string()).collect(),
                verbs: verb.granted_by(),
                max_priority,
                hard,
            },
            now + chrono::Duration::days(1),
            "test",
            now,
        )
        .expect("mint");
    (
        minted.record.token_id,
        minted.secret.expose_secret().to_string(),
    )
}

/// An intake socket path that fits `sun_path` (108 bytes with the NUL), and
/// the directory that holds it, which the caller keeps alive. A managed run's
/// TMPDIR can be too long for that, so the socket takes the first of the
/// process temp dir and `/dev/shm` that fits. The token store stays under the
/// fixture's own tempdir.
pub(super) fn short_intake_socket() -> (tempfile::TempDir, PathBuf) {
    for parent in [std::env::temp_dir(), PathBuf::from("/dev/shm")] {
        let Ok(dir) = tempfile::Builder::new()
            .prefix("intake")
            .tempdir_in(&parent)
        else {
            continue;
        };
        let socket = dir.path().join("intake.sock");
        if socket.as_os_str().as_encoded_bytes().len() < 108 {
            return (dir, socket);
        }
    }
    panic!("no directory short enough for an intake socket path");
}

pub(super) fn config(
    intake_socket: PathBuf,
    client_address: LeaseClientAddressPolicy,
) -> LeaseListenerConfig {
    LeaseListenerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        tls: None,
        client_address,
        intake_socket,
        failed_auth_limit: 1000,
        failed_auth_window: Duration::from_secs(300),
        limits: Default::default(),
    }
}

pub(super) async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    peer: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = builder.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

#[derive(Debug)]
pub(super) struct RefusalAudit {
    pub(super) key: String,
    pub(super) reason: String,
    pub(super) count: u64,
}

pub(super) fn audit_rows(home: &Path) -> Vec<RefusalAudit> {
    let connection = rusqlite::Connection::open(home.join("lease-tokens.db")).unwrap();
    let mut statement = connection.prepare(
        "SELECT limit_key, limit_name, count FROM lease_listener_limit_audit ORDER BY first_at, limit_key"
    ).unwrap();
    statement
        .query_map([], |row| {
            Ok(RefusalAudit {
                key: row.get(0)?,
                reason: row.get(1)?,
                count: row.get(2)?,
            })
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

pub(super) fn bearer(secret: &str) -> String {
    format!("Bearer {secret}")
}

const UNKNOWN_SECRET: &str =
    "orbl_0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
async fn health_and_unknown_routes_cannot_bypass_address_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("orbit-lease");
    let store = provisioned_store(&home);
    let router = lease_listener_router(
        &config(
            tmp.path().join("no-intake.sock"),
            LeaseClientAddressPolicy::Direct,
        ),
        store,
    );
    for _ in 0..120 {
        assert_eq!(
            send(&router, "GET", "/healthz", PEER, &[], None).await.0,
            StatusCode::OK
        );
    }
    let (status, headers, _) = send(&router, "GET", "/unknown", PEER, &[], None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
}

/// The lease-only router is the boundary that keeps a token away from
/// tasks, runs, config and operator routes (`4_decisions.md`, "A lease
/// token authorizes leases and nothing else").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_only_lease_routes_and_refuses_missing_bad_or_out_of_scope_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("orbit-lease");
    let mut store = provisioned_store(&home);
    let (_, lease_secret) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
    );
    let (_, read_secret) = mint(
        &mut store,
        "bob",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Read,
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
    );
    let (revoked_id, revoked_secret) = mint(
        &mut store,
        "carol",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
    );
    store.revoke(&revoked_id, "test", Utc::now()).unwrap();
    let router = lease_listener_router(
        &config(
            tmp.path().join("no-intake.sock"),
            LeaseClientAddressPolicy::Direct,
        ),
        store,
    );

    let (status, _, body) = send(&router, "GET", "/healthz", PEER, &[], None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"status": "ok"})));

    let valid = bearer(&lease_secret);
    for (method, path) in [
        ("GET", "/"),
        ("GET", "/api/tasks"),
        ("GET", "/api/health"),
        ("GET", "/api/config"),
        ("GET", "/api/runs"),
        ("GET", "/api/audit"),
        ("POST", "/api/tasks"),
        ("GET", "/api/leases/l1/renew/extra"),
        ("GET", "/dashboard"),
        ("GET", "/healthz/extra"),
    ] {
        let (status, _, _) = send(
            &router,
            method,
            path,
            PEER,
            &[("authorization", &valid)],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }

    let lease_routes = [
        ("GET", "/api/leases"),
        ("POST", "/api/leases"),
        ("POST", "/api/leases/check"),
        ("GET", "/api/leases/l1"),
        ("DELETE", "/api/leases/l1"),
        ("POST", "/api/leases/l1/renew"),
        ("POST", "/api/leases/l1/expand"),
    ];
    for (method, path) in lease_routes {
        let (status, headers, body) = send(&router, method, path, PEER, &[], None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(body, json!({"error": "unauthorized"}));
        assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
    }
    for secret in [UNKNOWN_SECRET, "not-a-token", revoked_secret.as_str()] {
        let (status, _, body) = send(
            &router,
            "GET",
            "/api/leases",
            PEER,
            &[("authorization", &bearer(secret))],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, json!({"error": "unauthorized"}));
    }
    let (status, _, _) = send(
        &router,
        "POST",
        "/api/leases",
        PEER,
        &[("authorization", &bearer(&read_secret))],
        Some(json!({"selectors": ["file:src/x.rs"]})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a read token may not request"
    );
    let (status, _, _) = send(
        &router,
        "GET",
        "/api/leases?workspace=ws_beta",
        PEER,
        &[("authorization", &valid)],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a token is scoped to its workspaces"
    );

    let rows = audit_rows(&home);
    let reasons: Vec<&str> = rows.iter().map(|row| row.reason.as_str()).collect();
    let expected = [
        "missing_token",
        "unknown",
        "revoked",
        "verb_not_granted",
        "wrong_workspace",
    ];
    assert_eq!(reasons, expected, "{rows:?}");
    assert!(
        rows.iter().all(|row| row.key == "address:192.0.2.10"),
        "{rows:?}"
    );
    assert_eq!(
        rows[0].count,
        lease_routes.len() as u64,
        "missing-token refusals coalesce across routes"
    );
    assert_eq!(
        rows[1].count, 2,
        "unknown-token refusals share one address/window row"
    );
    let stored = std::fs::read(home.join("lease-tokens.db")).unwrap();
    for secret in [&lease_secret, &read_secret, &revoked_secret] {
        assert!(
            !stored
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "a token secret reached the token store"
        );
    }
}

/// A client that sends a token where a workspace or a path belongs has sent a
/// secret, and the audit store must not keep it (STD-05): the refusal is still
/// audited, with the token replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_sent_in_the_query_or_path_never_reaches_an_audit_row() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("orbit-lease");
    let mut store = provisioned_store(&home);
    let (_, secret) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
    );
    let router = lease_listener_router(
        &config(
            tmp.path().join("no-intake.sock"),
            LeaseClientAddressPolicy::Direct,
        ),
        store,
    );

    let (status, _, _) = send(
        &router,
        "GET",
        &format!("/api/leases?workspace={secret}"),
        PEER,
        &[("authorization", &bearer(&secret))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &router,
        "GET",
        &format!("/api/leases/{secret}"),
        PEER,
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Percent-encoded bytes decode to the same token, so the row keeps none of them.
    let encoded: String = secret.bytes().map(|byte| format!("%{byte:02X}")).collect();
    let (status, _, _) = send(
        &router,
        "GET",
        &format!("/api/leases/{encoded}"),
        PEER,
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let rows = audit_rows(&home);
    let reasons: Vec<&str> = rows.iter().map(|row| row.reason.as_str()).collect();
    assert_eq!(reasons, ["wrong_workspace", "missing_token"], "{rows:?}");

    let stored = std::fs::read(home.join("lease-tokens.db")).unwrap();
    assert!(
        !stored
            .windows(secret.len())
            .any(|window| window == secret.as_bytes()),
        "a token sent by a client reached the token store"
    );
}

/// A TCP peer, its forwarding headers and the expected status.
type ProxyCase<'a> = (&'a str, &'a [(&'a str, &'a str)], StatusCode);

/// The client address decides audit rows and rate limits, so a client must
/// not be able to choose it (design §7.3 "Client address").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_address_comes_from_the_peer_or_the_rightmost_untrusted_forwarded_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let direct_home = tmp.path().join("direct");
    let direct = lease_listener_router(
        &config(
            tmp.path().join("intake.sock"),
            LeaseClientAddressPolicy::Direct,
        ),
        provisioned_store(&direct_home),
    );
    let unknown = bearer(UNKNOWN_SECRET);
    let auth = ("authorization", unknown.as_str());
    send(
        &direct,
        "GET",
        "/api/leases",
        PEER,
        &[auth, ("x-forwarded-for", "1.2.3.4")],
        None,
    )
    .await;
    assert_eq!(audit_rows(&direct_home)[0].key, "address:192.0.2.10");

    let proxied_home = tmp.path().join("proxied");
    let trusted = TrustedProxies::parse(&["10.0.0.0/8"]).unwrap();
    let proxied = lease_listener_router(
        &config(
            tmp.path().join("intake.sock"),
            LeaseClientAddressPolicy::BehindProxy(trusted),
        ),
        provisioned_store(&proxied_home),
    );
    let cases: [ProxyCase<'_>; 6] = [
        // The client sent 1.2.3.4; the proxy appended the real address.
        (
            "10.0.0.1:443",
            &[("x-forwarded-for", "1.2.3.4, 198.51.100.7")],
            StatusCode::UNAUTHORIZED,
        ),
        // Headers are joined in order and inner proxies are skipped.
        (
            "10.0.0.1:443",
            &[
                ("x-forwarded-for", "1.2.3.4"),
                ("x-forwarded-for", "198.51.100.8, 10.0.0.2"),
            ],
            StatusCode::UNAUTHORIZED,
        ),
        // A peer outside trusted_proxy: the header is ignored.
        (
            "203.0.113.9:443",
            &[("x-forwarded-for", "1.2.3.4")],
            StatusCode::UNAUTHORIZED,
        ),
        // `Forwarded` is never read.
        (
            "10.0.0.1:443",
            &[
                ("forwarded", "for=1.2.3.4"),
                ("x-forwarded-for", "198.51.100.9"),
            ],
            StatusCode::UNAUTHORIZED,
        ),
        // A malformed rightmost untrusted entry is refused, never replaced
        // by the proxy's address.
        (
            "10.0.0.1:443",
            &[("x-forwarded-for", "1.2.3.4, not-an-ip")],
            StatusCode::BAD_REQUEST,
        ),
        // Only trusted entries: no client address either.
        (
            "10.0.0.1:443",
            &[("forwarded", "for=1.2.3.4")],
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (peer, headers, expected) in cases {
        let mut headers = headers.to_vec();
        headers.push(auth);
        let (status, _, _) = send(&proxied, "GET", "/api/leases", peer, &headers, None).await;
        assert_eq!(status, expected, "{peer} {headers:?}");
    }
    let audited: Vec<(String, String)> = audit_rows(&proxied_home)
        .into_iter()
        .map(|row| (row.key, row.reason))
        .collect();
    let expected = [
        ("198.51.100.7", "unknown"),
        ("198.51.100.8", "unknown"),
        ("203.0.113.9", "unknown"),
        ("198.51.100.9", "unknown"),
        ("10.0.0.1", "malformed_forwarded_for"),
        ("10.0.0.1", "no_untrusted_forwarded_for"),
    ]
    .map(|(address, reason)| (format!("address:{address}"), reason.to_string()));
    assert_eq!(audited, expected);
}

/// Failed authentication is limited per address so a token cannot be
/// guessed online, and the limit lifts when its window ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_failed_authentication_gets_429_until_the_window_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("orbit-lease");
    let mut store = provisioned_store(&home);
    let (_, secret) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
    );
    let window = Duration::from_millis(1500);
    let mut config = config(
        tmp.path().join("no-intake.sock"),
        LeaseClientAddressPolicy::Direct,
    );
    config.failed_auth_limit = 2;
    config.failed_auth_window = window;
    let router = lease_listener_router(&config, store);
    let unknown = bearer(UNKNOWN_SECRET);
    let valid = bearer(&secret);

    for _ in 0..2 {
        let (status, _, _) = send(
            &router,
            "GET",
            "/api/leases",
            PEER,
            &[("authorization", &unknown)],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    for _ in 0..2 {
        let (status, headers, _) = send(
            &router,
            "GET",
            "/api/leases",
            PEER,
            &[("authorization", &valid)],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(headers.contains_key(header::RETRY_AFTER));
    }
    let (status, _, _) = send(
        &router,
        "GET",
        "/api/leases",
        "192.0.2.11:50000",
        &[("authorization", &unknown)],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "another address is not limited"
    );

    tokio::time::sleep(window + Duration::from_millis(200)).await;
    let (status, _, _) = send(
        &router,
        "GET",
        "/api/leases",
        PEER,
        &[("authorization", &valid)],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "after the window the valid token is admitted and reaches the (absent) intake socket"
    );
    let reasons: Vec<String> = audit_rows(&home)
        .into_iter()
        .map(|row| row.reason)
        .collect();
    assert_eq!(
        reasons,
        ["unknown", "failed_auth", "unknown", "intake_unavailable"],
        "one failed_auth row per window"
    );
}
