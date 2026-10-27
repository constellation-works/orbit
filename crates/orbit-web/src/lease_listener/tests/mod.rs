//! A token's principal and scope carried from the listener through the real
//! intake socket to the runtime. The intake server's test socket, which
//! admits the test's own user, is crate-private, so this cannot run at the
//! crate boundary; the listener's other guarantees are tested there
//! (`tests/http_api/lease_listener.rs`).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
use crate::lease_intake::{
    IntakeLimits, LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeOperation,
    LeaseIntakeRequest, LeaseIntakeResponse, LeaseIntakeSocket,
};
use crate::state::{DashboardState, SINGLE_WORKSPACE_ID};

const PEER: &str = "192.0.2.10:50000";

#[derive(Clone)]
struct TraceCapture(Arc<Mutex<Vec<u8>>>);

impl Write for TraceCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

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
    let trace = TraceCapture(Arc::new(Mutex::new(Vec::new())));
    let trace_writer = trace.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || trace_writer.clone())
            .finish(),
    )
    .unwrap();
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
    std::fs::write(repo_root.join("src/z.rs"), "pub fn z() {}\n").unwrap();
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
    let (high_id, high) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &[SINGLE_WORKSPACE_ID],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
    );
    let (_, narrow) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &[SINGLE_WORKSPACE_ID],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
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

    std::fs::write(repo_root.join("src").join(&alice), "fixture").unwrap();

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
            .serve(
                DashboardState::single(Arc::clone(&runtime)),
                IntakeLimits::default(),
            )
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

    let mut revoker = LeaseTokenStore::open_in_orbit_home(&home).unwrap();

    tokio.block_on(async {
        for path in [alice.clone(), format!("%6F{}", &alice[1..])] {
            let (status, _, response) = send(
                &router,
                "GET",
                &format!("/api/leases/{path}"),
                PEER,
                &[("authorization", &bearer(&alice))],
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
            assert!(
                !response.to_string().contains(&alice),
                "secret in response: {response}"
            );
            assert!(
                !response.to_string().contains(&path),
                "encoded secret in response: {response}"
            );
        }
        for priority in ["high", "normal"] {
            let (status, _, response) = send(
                &router,
                "POST",
                "/api/leases",
                PEER,
                &[("authorization", &bearer(&alice))],
                Some(json!({"selectors": [format!("file:src/{alice}")], "priority": priority})),
            )
            .await;
            assert_eq!(
                status,
                if priority == "high" {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::OK
                },
                "{response}"
            );
        }
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

        let (status, _, high_grant) = send(
            &router,
            "POST",
            "/api/leases",
            PEER,
            &[("authorization", &bearer(&high))],
            Some(json!({"selectors": ["file:src/z.rs"], "mode": "hard", "priority": "high"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{high_grant}");
        let high_lease = high_grant["lease"]["lease_id"].as_str().unwrap();
        assert_eq!(high_grant["lease"]["mode"], "hard");
        assert_eq!(high_grant["lease"]["priority"], "high");
        revoker.revoke(&high_id, "test", Utc::now()).unwrap();
        let uri = format!("/api/leases/{high_lease}/renew");
        let (status, _, revoked) = send(
            &router,
            "POST",
            &uri,
            PEER,
            &[("authorization", &bearer(&high))],
            Some(json!({"ttl_seconds": 14400})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{revoked}");
        let (status, _, denied) = send(
            &router,
            "POST",
            &uri,
            PEER,
            &[("authorization", &bearer(&narrow))],
            Some(json!({"ttl_seconds": 14400})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{denied}");
        let (status, _, shown) = send(
            &router,
            "GET",
            &format!("/api/leases/{high_lease}"),
            PEER,
            &[("authorization", &bearer(&narrow))],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{shown}");
        assert_eq!(
            shown["lease"]["expires_at"],
            high_grant["lease"]["expires_at"]
        );
        assert_eq!(shown["lease"]["mode"], "hard");
        assert_eq!(shown["lease"]["priority"], "high");

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
    let trace_output = String::from_utf8(trace.0.lock().unwrap().clone()).unwrap();
    assert!(!trace_output.contains(&alice));
    assert!(!trace_output.contains(&format!("%6F{}", &alice[1..])));
    let events = runtime
        .list_audit_events(None, None, None, None, 100)
        .unwrap();
    let intake: Vec<_> = events
        .iter()
        .filter(|event| {
            event.command == "lease.intake.show"
                && event.status == orbit_types::telemetry::AuditEventStatus::Failure
        })
        .collect();
    assert_eq!(intake.len(), 2);
    for event in intake {
        let row = serde_json::to_string(event).unwrap();
        assert!(!row.contains(&alice), "secret in owner audit: {row}");
        assert!(!row.contains(&format!("%6F{}", &alice[1..])));
        let payload: Value = serde_json::from_str(event.arguments_json.as_ref().unwrap()).unwrap();
        assert_eq!(payload["principal"], "alice");
        assert_eq!(payload["token_id"], alice_id);
        assert_eq!(payload["operation"], "show");
        assert_eq!(payload["outcome"], "error");
        assert!(
            payload["client_address"]
                .as_str()
                .unwrap()
                .contains("192.0.2.10")
        );
    }
    for command in ["lease.request.granted", "lease.request.refused"] {
        let event = events
            .iter()
            .find(|event| {
                event.command == command
                    && event
                        .arguments_json
                        .as_deref()
                        .is_some_and(|payload| payload.contains("[REDACTED_SECRET]"))
            })
            .expect("redacted selector audit row");
        let payload: Value = serde_json::from_str(event.arguments_json.as_ref().unwrap()).unwrap();
        assert!(!event.arguments_json.as_ref().unwrap().contains(&alice));
        assert_eq!(payload["principal"], "alice");
        assert_eq!(payload["token_id"], alice_id);
        assert!(
            payload["client_address"]
                .as_str()
                .unwrap()
                .contains("192.0.2.10")
        );
    }
    assert!(
        audit_rows(&home).is_empty(),
        "operations that reached Orbit are audited there, not by the listener"
    );
}

/// The next data chunk of a streaming body, or `None` when it ends.
async fn next_chunk(body: &mut Body) -> Option<axum::body::Bytes> {
    use hyper::body::Body as _;
    loop {
        let frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut *body).poll_frame(cx))
            .await?
            .ok()?;
        if let Ok(data) = frame.into_data() {
            return Some(data);
        }
    }
}

/// Read `count` SSE events as `(id, data)`, skipping keep-alive comments.
async fn read_events(body: &mut Body, buffer: &mut String, count: usize) -> Vec<(u64, Value)> {
    let mut events = Vec::new();
    while events.len() < count {
        if let Some(end) = buffer.find("\n\n") {
            let event: String = buffer.drain(..end + 2).collect();
            if let Some(rest) = event.strip_prefix("id: ") {
                let (id, data) = rest.split_once("\ndata: ").expect("an id and data");
                events.push((
                    id.parse().unwrap(),
                    serde_json::from_str(data.trim_end()).unwrap(),
                ));
            }
            continue;
        }
        let chunk = tokio::time::timeout(Duration::from_secs(10), next_chunk(body))
            .await
            .expect("an event within 10 seconds")
            .expect("the stream stays open");
        buffer.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    events
}

/// Through the listener, the real intake socket and the runtime, notice
/// polling and the SSE stream return the notices Orbit Web returns for the
/// same workspace and cursor, in the same order, and a token reads only its
/// own workspaces' notices (design §5 "Push and poll", §7.3).
#[test]
fn listener_notices_and_events_match_orbit_web_and_stay_in_the_tokens_workspaces() {
    if !enter_isolated_child(
        module_path!(),
        "listener_notices_and_events_match_orbit_web_and_stay_in_the_tokens_workspaces",
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
    for name in ["x", "y"] {
        std::fs::write(repo_root.join(format!("src/{name}.rs")), "pub fn f() {}\n").unwrap();
    }
    std::fs::write(
        workspace_root.join("config.toml"),
        "[lease]\nenabled = true\n",
    )
    .unwrap();
    let runtime =
        Arc::new(OrbitRuntime::from_roots(&global_root, &workspace_root).expect("runtime"));

    let home = tmp.path().join("orbit-lease");
    let mut store = provisioned_store(&home);
    let token = |store: &mut LeaseTokenStore, principal: &str, workspace: &str, verb| {
        mint(
            store,
            principal,
            PrincipalClass::Human,
            &[workspace],
            verb,
            LeasePriorityCeiling::Normal,
            HardLeasePermission::Allow,
        )
        .1
    };
    let alice = bearer(&token(
        &mut store,
        "alice",
        SINGLE_WORKSPACE_ID,
        LeaseTokenVerb::Lease,
    ));
    let bob = bearer(&token(
        &mut store,
        "bob",
        SINGLE_WORKSPACE_ID,
        LeaseTokenVerb::Lease,
    ));
    let reader = bearer(&token(
        &mut store,
        "reader",
        SINGLE_WORKSPACE_ID,
        LeaseTokenVerb::Read,
    ));
    let outsider = bearer(&token(
        &mut store,
        "outsider",
        "ws_other",
        LeaseTokenVerb::Lease,
    ));

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
            .serve(
                DashboardState::single(Arc::clone(&runtime)),
                IntakeLimits::default(),
            )
            .unwrap()
    };
    tokio.spawn(accept);
    let router = lease_listener_router(
        &config(socket_path.clone(), LeaseClientAddressPolicy::Direct),
        store,
    );
    let web = crate::serve::build_app(DashboardState::single(Arc::clone(&runtime))).unwrap();

    tokio.block_on(async {
        let web_notices = |query: String| {
            let web = web.clone();
            async move {
                let (status, _, page) = send(
                    &web,
                    "GET",
                    &format!("/api/leases/notices{query}"),
                    "127.0.0.1:50000",
                    &[("host", "127.0.0.1")],
                    None,
                )
                .await;
                assert_eq!(status, StatusCode::OK, "Orbit Web {query}: {page}");
                page
            }
        };
        let (status, _, first) = send(
            &router,
            "POST",
            "/api/leases",
            PEER,
            &[("authorization", &alice)],
            Some(json!({"selectors": ["file:src/x.rs"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, _, _) = send(
            &router,
            "POST",
            "/api/leases",
            PEER,
            &[("authorization", &bob)],
            Some(json!({"selectors": ["file:src/x.rs"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = first["lease"]["lease_id"].as_str().unwrap();
        let (status, _, _) = send(
            &router,
            "DELETE",
            &format!("/api/leases/{id}"),
            PEER,
            &[("authorization", &alice)],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let all = web_notices("?after=0".into()).await;
        let notices = all["notices"].as_array().unwrap().clone();
        assert!(notices.len() >= 3, "granted, overlap and released: {all}");
        let cursor = notices[0]["seq"].as_u64().unwrap();
        for query in [
            "?after=0".to_string(),
            format!("?after={cursor}&limit=1"),
            format!("?after={cursor}&lease_id={id}"),
        ] {
            let (status, _, listened) = send(
                &router,
                "GET",
                &format!("/api/leases/notices{query}"),
                PEER,
                &[("authorization", &reader)],
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{query}: {listened}");
            assert_eq!(listened, web_notices(query.clone()).await, "{query}");
        }

        // `Last-Event-ID` is the cursor, in preference to `after`.
        let mut request = Request::builder()
            .uri("/api/leases/events?after=0&limit=1")
            .header("authorization", &reader)
            .header("last-event-id", cursor.to_string())
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(PEER.parse::<SocketAddr>().unwrap()));
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let mut body = response.into_body();
        let mut buffer = String::new();
        let expected = web_notices(format!("?after={cursor}")).await;
        let expected: Vec<(u64, Value)> = expected["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| (notice["seq"].as_u64().unwrap(), notice.clone()))
            .collect();
        let streamed = read_events(&mut body, &mut buffer, expected.len()).await;
        assert_eq!(streamed, expected);

        // A notice written after the stream opened is pushed on it.
        let last = expected.last().unwrap().0;
        let (status, _, _) = send(
            &router,
            "POST",
            "/api/leases",
            PEER,
            &[("authorization", &alice)],
            Some(json!({"selectors": ["file:src/y.rs"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let pushed = read_events(&mut body, &mut buffer, 1).await;
        let expected = web_notices(format!("?after={last}")).await;
        assert_eq!(pushed[0].1, expected["notices"][0]);
        assert_eq!(pushed[0].1["kind"], "granted");
        drop(body);

        // Another workspace's token never reads this workspace's notices.
        for path in ["/api/leases/notices", "/api/leases/events"] {
            let (status, _, _) = send(
                &router,
                "GET",
                &format!("{path}?workspace={SINGLE_WORKSPACE_ID}"),
                PEER,
                &[("authorization", &outsider)],
                None,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
            let (status, _, body) = send(
                &router,
                "GET",
                path,
                PEER,
                &[("authorization", &outsider)],
                None,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert_eq!(body["code"], "unknown_workspace", "{path}");
        }
    });

    // Orbit checks the scope again, so a listener that skipped its own
    // check still could not read another workspace.
    let mut client = LeaseIntakeClient::connect(&socket_path, Duration::from_secs(10)).unwrap();
    for operation in [LeaseIntakeOperation::Notices, LeaseIntakeOperation::Events] {
        let request = LeaseIntakeRequest {
            operation,
            workspace: SINGLE_WORKSPACE_ID.to_string(),
            authority: LeaseIntakeAuthority {
                token_id: "tok-outsider".to_string(),
                scope: LeaseTokenScope {
                    principal: "outsider".to_string(),
                    principal_class: PrincipalClass::Human,
                    workspaces: vec!["ws_other".to_string()],
                    verbs: LeaseTokenVerb::Lease.granted_by(),
                    max_priority: LeasePriorityCeiling::Normal,
                    hard: HardLeasePermission::Allow,
                },
                client_address: "192.0.2.10".to_string(),
            },
            input: json!({"after": 0}),
        };
        match client.call(&request).unwrap() {
            LeaseIntakeResponse::Error(error) => {
                assert_eq!(error.code, "scope_refused", "{operation:?}")
            }
            LeaseIntakeResponse::Ok { result } => panic!("{operation:?} read {result}"),
        }
    }
    drop(tokio);
}
