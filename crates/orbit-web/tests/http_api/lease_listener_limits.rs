//! Regressions for unbounded listener requests at the public HTTP/settings APIs.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use hyper::body::Frame;
use orbit_auth::client_address::{LeaseClientAddressPolicy, TrustedProxies};
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenVerb, PrincipalClass,
};
use orbit_web::lease_listener::{lease_listener_router, serve_lease_listener_on};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

use super::lease_listener::{audit_rows, bearer, config, mint, provisioned_store, send};

#[tokio::test]
async fn ipv6_prefixes_mapped_ipv4_and_proxy_clients_keep_one_address_budget() {
    let tmp = tempfile::tempdir().unwrap();
    for (index, policy, peers, headers) in [
        (
            0,
            LeaseClientAddressPolicy::Direct,
            ["[2001:db8:1:2::1]:1", "[2001:db8:1:2::2]:2"],
            ["", ""],
        ),
        (
            1,
            LeaseClientAddressPolicy::Direct,
            ["192.0.2.10:1", "[::ffff:192.0.2.10]:2"],
            ["", ""],
        ),
        (
            2,
            LeaseClientAddressPolicy::BehindProxy(TrustedProxies::parse(&["10.0.0.1"]).unwrap()),
            ["10.0.0.1:1", "10.0.0.1:2"],
            ["1.1.1.1, 192.0.2.10", "8.8.8.8, 192.0.2.10"],
        ),
    ] {
        let home = tmp.path().join(index.to_string());
        let mut settings = config(tmp.path().join("absent.sock"), policy);
        settings.limits.address_per_minute = 1;
        let router = lease_listener_router(&settings, provisioned_store(&home));
        assert_eq!(
            send(
                &router,
                "GET",
                "/healthz",
                peers[0],
                &[("x-forwarded-for", headers[0])],
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, headers, body) = send(
            &router,
            "GET",
            "/healthz",
            peers[1],
            &[("x-forwarded-for", headers[1])],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(headers.contains_key("retry-after"));
        assert_eq!(body, json!({"error": "address_requests"}));
    }
}

#[tokio::test]
async fn address_rejection_never_verifies_a_valid_token_and_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut store = provisioned_store(&home);
    let (id, secret) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Lease,
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
    );
    let mut settings = config(
        tmp.path().join("absent.sock"),
        LeaseClientAddressPolicy::Direct,
    );
    settings.limits.address_per_minute = 1;
    let router = lease_listener_router(&settings, store);
    assert_eq!(
        send(&router, "GET", "/healthz", "192.0.2.10:1", &[], None)
            .await
            .0,
        StatusCode::OK
    );
    let auth = bearer(&secret);
    assert_eq!(
        send(
            &router,
            "GET",
            "/api/leases",
            "192.0.2.10:2",
            &[("authorization", &auth)],
            None
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let connection = rusqlite::Connection::open(home.join("lease-tokens.db")).unwrap();
    let last_use: Option<String> = connection
        .query_row(
            "SELECT last_used_at FROM lease_tokens WHERE token_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        last_use, None,
        "a successful token lookup would record last_used_at"
    );
    // Pause only after blocking-store work completes: virtual time cannot race
    // the authentication blocking pool or introduce wall-clock recovery waits.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    assert_eq!(
        send(&router, "GET", "/healthz", "192.0.2.10:3", &[], None)
            .await
            .0,
        StatusCode::OK
    );
}

fn read_frame(stream: &mut std::os::unix::net::UnixStream) -> Value {
    use std::io::Read;
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn write_frame(stream: &mut std::os::unix::net::UnixStream, value: Value) {
    use std::io::Write;
    let bytes = serde_json::to_vec(&value).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}

#[tokio::test]
async fn token_writes_reads_and_other_tokens_have_separate_budgets() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut store = provisioned_store(&home);
    let mut secrets = Vec::new();
    for principal in ["alice", "bob"] {
        secrets.push(
            mint(
                &mut store,
                principal,
                PrincipalClass::Human,
                &["ws_alpha"],
                LeaseTokenVerb::Lease,
                LeasePriorityCeiling::High,
                HardLeasePermission::Allow,
            )
            .1,
        );
    }
    let socket = tmp.path().join("intake.sock");
    let intake = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::task::spawn_blocking(move || {
        for _ in 0..4 {
            let (mut stream, _) = intake.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            read_frame(&mut stream);
            write_frame(
                &mut stream,
                json!({"protocol_version": orbit_web::lease_intake::LEASE_INTAKE_PROTOCOL_VERSION}),
            );
            read_frame(&mut stream);
            write_frame(
                &mut stream,
                serde_json::to_value(orbit_web::lease_intake::LeaseIntakeResponse::Ok {
                    result: json!({"accepted": true}),
                })
                .unwrap(),
            );
        }
    });
    let mut settings = config(socket, LeaseClientAddressPolicy::Direct);
    settings.limits.token_writes_per_minute = 1;
    settings.limits.token_reads_per_minute = 2;
    let router = lease_listener_router(&settings, store);
    let alice = bearer(&secrets[0]);
    let bob = bearer(&secrets[1]);
    for (method, uri, peer, auth, expected) in [
        (
            "POST",
            "/api/leases",
            "192.0.2.10:1",
            &alice,
            StatusCode::OK,
        ),
        (
            "POST",
            "/api/leases",
            "192.0.2.11:1",
            &alice,
            StatusCode::TOO_MANY_REQUESTS,
        ),
        ("GET", "/api/leases", "192.0.2.10:1", &alice, StatusCode::OK),
        ("POST", "/api/leases", "192.0.2.10:1", &bob, StatusCode::OK),
        (
            "POST",
            "/api/leases/check",
            "192.0.2.11:1",
            &alice,
            StatusCode::OK,
        ),
        (
            "GET",
            "/api/leases",
            "192.0.2.10:1",
            &alice,
            StatusCode::TOO_MANY_REQUESTS,
        ),
    ] {
        let (status, headers, _) = send(
            &router,
            method,
            uri,
            peer,
            &[("authorization", auth)],
            (method == "POST").then(|| json!({"selectors": ["file:src/a.rs"]})),
        )
        .await;
        assert_eq!(status, expected, "{method} {uri} from {peer}");
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(headers.contains_key("retry-after"));
        }
    }
    server.await.unwrap();
}

struct StalledBody(Arc<tokio::sync::Notify>);
impl hyper::body::Body for StalledBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        self.0.notify_one();
        Poll::Pending
    }
}

struct OversizedBody(Arc<std::sync::atomic::AtomicUsize>);
impl hyper::body::Body for OversizedBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let count = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Poll::Ready((count < 100).then(|| Ok(Frame::data(Bytes::from_static(b"xxxxxxxx")))))
    }
}

#[tokio::test]
async fn oversized_bodies_are_not_polled_and_stalled_bodies_hold_concurrency_until_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
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
    let mut settings = config(
        tmp.path().join("absent.sock"),
        LeaseClientAddressPolicy::Direct,
    );
    settings.limits.max_in_flight_requests = 1;
    settings.limits.max_body_bytes = 32;
    settings.limits.request_timeout_seconds = 60;
    let router = lease_listener_router(&settings, store);
    let polled = Arc::new(tokio::sync::Notify::new());
    let make_request = |length: Option<&str>| {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/leases")
            .header("content-type", "application/json")
            .header("authorization", bearer(&secret));
        if let Some(length) = length {
            builder = builder.header("content-length", length);
        }
        let mut request = builder
            .body(Body::new(StalledBody(Arc::clone(&polled))))
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.10:1".parse::<std::net::SocketAddr>().unwrap(),
        ));
        request
    };
    let response = router
        .clone()
        .oneshot(make_request(Some("33")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        tokio::time::timeout(Duration::ZERO, polled.notified())
            .await
            .is_err(),
        "declared oversized body must not be polled"
    );
    drop(response);
    let chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut chunked = make_request(None);
    *chunked.body_mut() = Body::new(OversizedBody(Arc::clone(&chunks)));
    let response = router.clone().oneshot(chunked).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        chunks.load(std::sync::atomic::Ordering::Relaxed),
        5,
        "stop on the first chunk over the limit, without consuming the remaining body"
    );
    drop(response);
    let first = tokio::spawn(router.clone().oneshot(make_request(None)));
    polled.notified().await;
    assert_eq!(
        send(&router, "GET", "/healthz", "192.0.2.11:1", &[], None)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    let response = first.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    drop(response);
    assert_eq!(
        send(&router, "GET", "/healthz", "192.0.2.11:1", &[], None)
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn stalled_headers_and_connection_saturation_are_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut settings = config(
        tmp.path().join("absent.sock"),
        LeaseClientAddressPolicy::Direct,
    );
    settings.limits.max_connections = 1;
    settings.limits.header_timeout_seconds = 1;
    let listener = tokio::net::TcpListener::bind(settings.bind).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve_lease_listener_on(
        listener,
        settings,
        provisioned_store(&home),
        async {
            let _ = shutdown.await;
        },
    ));
    let mut stalled = tokio::net::TcpStream::connect(address).await.unwrap();
    stalled
        .write_all(b"GET /healthz HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    let mut excess = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(5), excess.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stalled.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        response.is_empty() || response.starts_with(b"HTTP/1.1 408"),
        "{response:?}"
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(
        audit_rows(&home)
            .iter()
            .any(|row| row.reason == "max_connections")
    );
}

#[test]
fn listener_limit_settings_defaults_and_invalid_values_match_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = orbit_config::ConfigRoots::new(tmp.path(), tmp.path().join("workspace"));
    let defaults = orbit_config::load_lease_listener_settings(tmp.path()).unwrap();
    let resolved = orbit_config::ResolvedConfig::load(&roots).unwrap();
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.address_per_minute")
            .unwrap(),
        json!(defaults.address_per_minute)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.ipv6_prefix_len")
            .unwrap(),
        json!(defaults.ipv6_prefix_len)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.token_reads_per_minute")
            .unwrap(),
        json!(defaults.token_reads_per_minute)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.token_writes_per_minute")
            .unwrap(),
        json!(defaults.token_writes_per_minute)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.max_streams_per_token")
            .unwrap(),
        json!(defaults.max_streams_per_token)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.max_in_flight_per_token")
            .unwrap(),
        json!(defaults.max_in_flight_per_token)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.max_connections")
            .unwrap(),
        json!(defaults.max_connections)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.max_in_flight_requests")
            .unwrap(),
        json!(defaults.max_in_flight_requests)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.max_body_bytes")
            .unwrap(),
        json!(defaults.max_body_bytes)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.header_timeout_seconds")
            .unwrap(),
        json!(defaults.header_timeout_seconds)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.request_timeout_seconds")
            .unwrap(),
        json!(defaults.request_timeout_seconds)
    );
    assert_eq!(
        resolved
            .snapshot
            .value_for("lease.listener.limits.max_tracked_keys")
            .unwrap(),
        json!(defaults.max_tracked_keys)
    );
    for (key, bad) in [
        ("address_per_minute", "100001"),
        ("address_per_minute", "0"),
        ("ipv6_prefix_len", "129"),
        ("token_reads_per_minute", "100001"),
        ("token_reads_per_minute", "0"),
        ("token_writes_per_minute", "100001"),
        ("token_writes_per_minute", "0"),
        ("max_streams_per_token", "101"),
        ("max_streams_per_token", "0"),
        ("max_in_flight_per_token", "1001"),
        ("max_in_flight_per_token", "0"),
        ("max_connections", "10001"),
        ("max_connections", "0"),
        ("max_in_flight_requests", "1001"),
        ("max_in_flight_requests", "0"),
        ("max_body_bytes", "1048577"),
        ("max_body_bytes", "0"),
        ("header_timeout_seconds", "301"),
        ("header_timeout_seconds", "0"),
        ("request_timeout_seconds", "301"),
        ("request_timeout_seconds", "0"),
        ("max_tracked_keys", "65537"),
        ("max_tracked_keys", "2"),
        ("failed_auth", "0"),
        ("failed_auth_window_seconds", "0"),
        ("address_per_minute", "'invalid'"),
    ] {
        let key = format!("lease.listener.limits.{key}");
        std::fs::write(tmp.path().join("config.toml"), format!("{key} = {bad}\n")).unwrap();
        let listener_error = orbit_config::load_lease_listener_settings(tmp.path())
            .unwrap_err()
            .to_string();
        let registry_error = orbit_config::ResolvedConfig::load(&roots)
            .unwrap_err()
            .to_string();
        assert!(listener_error.contains(&key), "{listener_error}");
        assert!(registry_error.contains(&key), "{registry_error}");
    }
}
