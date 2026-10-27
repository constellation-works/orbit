//! A token's principal and scope carried from the listener through the real
//! intake socket to the runtime. The intake server's test socket, which
//! admits the test's own user, is crate-private, so this cannot run at the
//! crate boundary; the listener's other guarantees are tested there
//! (`tests/http_api/lease_listener.rs`).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use orbit_auth::client_address::LeaseClientAddressPolicy;
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenScope, LeaseTokenVerb, PrincipalClass,
};
use orbit_auth::store::LeaseTokenStore;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{LeaseListenerConfig, lease_listener_router};
use crate::api::tests::test_support::enter_isolated_child;
use crate::lease_intake::{IntakeLimits, LeaseIntakeSocket};
use crate::state::{DashboardState, SINGLE_WORKSPACE_ID};

const PEER: &str = "192.0.2.10:50000";

fn provisioned_store(home: &Path) -> LeaseTokenStore {
    std::fs::create_dir_all(home).unwrap();
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = home.join("lease-tokens.db");
    std::fs::write(&store, b"").unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).unwrap();
    LeaseTokenStore::open_in_orbit_home(home).expect("open token store")
}

fn mint(
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

fn config(intake_socket: PathBuf, client_address: LeaseClientAddressPolicy) -> LeaseListenerConfig {
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

async fn send(
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

/// `(event, reason, remote_address, route, token_id)` for every audit row.
fn audit_rows(home: &Path) -> Vec<(String, String, String, String, Option<String>)> {
    let connection = rusqlite::Connection::open(home.join("lease-tokens.db")).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT event, reason, remote_address, route, token_id FROM lease_token_audit \
             WHERE event != 'mint' AND event != 'revoke' ORDER BY audit_id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn bearer(secret: &str) -> String {
    format!("Bearer {secret}")
}

/// Through the real intake socket and runtime: the principal and token id
/// come from the token, never the body, and the token's priority ceiling
/// and `hard = never` hold (design §7.3 "Ceilings are boundaries here").
#[test]
fn a_granted_lease_carries_the_tokens_principal_and_scope_through_the_intake_socket() {
    if !enter_isolated_child(
        module_path!(),
        "a_granted_lease_carries_the_tokens_principal_and_scope_through_the_intake_socket",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
        ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let global_root = tmp.path().join("global");
    let repo_root = tmp.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).unwrap();
    std::fs::create_dir_all(repo_root.join("src")).unwrap();
    std::fs::create_dir_all(&workspace_root).unwrap();
    std::fs::write(repo_root.join("src/x.rs"), "pub fn x() {}\n").unwrap();
    std::fs::write(repo_root.join("src/y.rs"), "pub fn y() {}\n").unwrap();
    std::fs::write(
        workspace_root.join("config.toml"),
        "[lease]\nenabled = true\n",
    )
    .unwrap();
    let runtime =
        Arc::new(OrbitRuntime::from_roots(&global_root, &workspace_root).expect("runtime"));

    let home = tmp.path().join("orbit-lease");
    let mut store = provisioned_store(&home);
    let (alice_id, alice) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &[SINGLE_WORKSPACE_ID],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::Normal,
        HardLeasePermission::Allow,
    );
    let (_, agent) = mint(
        &mut store,
        "alice/claude-code",
        PrincipalClass::Agent,
        &[SINGLE_WORKSPACE_ID],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::Normal,
        HardLeasePermission::Never,
    );

    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let socket_path = tmp.path().join("intake.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    // SAFETY: geteuid has no preconditions and cannot fail.
    let own_uid = unsafe { libc::geteuid() };
    let mut socket = LeaseIntakeSocket::for_test_listener(listener, socket_path.clone(), own_uid);
    let accept = {
        let _entered = tokio.enter();
        socket
            .serve(DashboardState::single(runtime), IntakeLimits::default())
            .unwrap()
    };
    tokio.spawn(accept);
    let pair = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_path = home.join("cert.pem");
    let key_path = home.join("key.pem");
    std::fs::write(&cert_path, pair.cert.pem()).unwrap();
    std::fs::write(&key_path, pair.signing_key.serialize_pem()).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(home.join("config.toml"), format!(
        "[lease.listener]\nbind = '0.0.0.0:0'\nintake_socket = {socket_path:?}\ntls = {{ cert = {cert_path:?}, key = {key_path:?} }}\n"
    )).unwrap();
    let settings = orbit_config::load_lease_listener_settings(&home).unwrap();
    let tls_config = LeaseListenerConfig::from_settings(&settings).unwrap();
    let tls_store = LeaseTokenStore::open_in_orbit_home(&home).unwrap();
    let router = lease_listener_router(
        &config(socket_path, LeaseClientAddressPolicy::Direct),
        store,
    );

    tokio.block_on(async {
        let (status, _, granted) = send(
            &router,
            "POST",
            "/api/leases?principal=mallory",
            PEER,
            &[("authorization", &bearer(&alice))],
            Some(json!({"selectors": ["file:src/x.rs"], "principal": "mallory"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{granted}");
        assert_eq!(granted["lease"]["principal"], "alice", "{granted}");
        assert_eq!(granted["lease"]["token_id"], alice_id.as_str(), "{granted}");

        let (status, _, refused) = send(
            &router,
            "POST",
            "/api/leases",
            PEER,
            &[("authorization", &bearer(&alice))],
            Some(json!({"selectors": ["file:src/y.rs"], "priority": "high"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
        assert!(
            refused["error"]
                .as_str()
                .is_some_and(|error| error.contains("ceiling")),
            "{refused}"
        );

        let (status, _, downgraded) = send(
            &router,
            "POST",
            "/api/leases",
            PEER,
            &[("authorization", &bearer(&agent))],
            Some(json!({"selectors": ["file:src/y.rs"], "mode": "hard"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{downgraded}");
        assert_eq!(downgraded["lease"]["principal"], "alice/claude-code");
        assert_eq!(downgraded["lease"]["mode"], "advisory", "{downgraded}");

        // The same public listener path as the CLI: real TLS and a real
        // intake socket, so HTTPS must carry the authenticated lease to Orbit.
        let listener = tokio::net::TcpListener::bind(tls_config.bind)
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shutdown, stop) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(super::serve_lease_listener_on(
            listener,
            tls_config,
            tls_store,
            async {
                let _ = stop.await;
            },
        ));
        // A peer that never sends ClientHello must not block other clients
        // or graceful shutdown. No timing race or synthetic load is needed.
        let stalled = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .add_root_certificate(
                reqwest::Certificate::from_pem(pair.cert.pem().as_bytes()).unwrap(),
            )
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            .max_tls_version(reqwest::tls::Version::TLS_1_2)
            .build()
            .unwrap();
        let base = format!("https://localhost:{port}");
        let health = client.get(format!("{base}/healthz")).send().await.unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(
            health.json::<Value>().await.unwrap(),
            json!({"status":"ok"})
        );
        let leases = client
            .get(format!("{base}/api/leases"))
            .bearer_auth(&alice)
            .send()
            .await
            .unwrap();
        assert_eq!(leases.status(), StatusCode::OK);
        assert!(
            client
                .get(format!("http://localhost:{port}/healthz"))
                .send()
                .await
                .is_err(),
            "a TLS-configured listener must never serve plaintext HTTP"
        );
        shutdown.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("pending TLS handshakes must not delay shutdown")
            .unwrap()
            .unwrap();
        drop(stalled);
    });
    assert!(
        audit_rows(&home).is_empty(),
        "operations that reached Orbit are audited there, not by the listener"
    );
}
