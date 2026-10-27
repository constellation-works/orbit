//! The listener's notice stream at its public HTTP API, over a scripted
//! intake socket: open streams count toward the listener's limits, refusals
//! answer as on other routes, streams end with the listener, and an Orbit of
//! another intake protocol version is refused.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use orbit_auth::client_address::LeaseClientAddressPolicy;
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenVerb, PrincipalClass,
};
use orbit_web::lease_intake::{
    LEASE_INTAKE_PROTOCOL_VERSION, LeaseIntakeClient, LeaseIntakeOperation, LeaseIntakeResponse,
};
use orbit_web::lease_listener::{lease_listener_router, serve_lease_listener_on};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::lease_listener::{audit_rows, bearer, config, mint, provisioned_store, send};

fn read_frame(stream: &mut UnixStream) -> Option<Value> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).ok()?;
    let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_frame(stream: &mut UnixStream, value: &Value) -> bool {
    let bytes = serde_json::to_vec(value).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|()| stream.write_all(&bytes))
        .is_ok()
}

/// An intake socket that speaks `version` and answers every operation with
/// an empty notice page; `events` then sends an empty page every 50 ms
/// until the listener closes the connection.
fn scripted_intake(path: &Path, version: u32) {
    let intake = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for stream in intake.incoming() {
            let Ok(mut stream) = stream else { return };
            std::thread::spawn(move || {
                read_frame(&mut stream);
                if !write_frame(&mut stream, &json!({"protocol_version": version})) {
                    return;
                }
                let Some(request) = read_frame(&mut stream) else {
                    return;
                };
                let page = serde_json::to_value(LeaseIntakeResponse::Ok {
                    result: json!({"notices": [], "cursor": request["input"]["after"]}),
                })
                .unwrap();
                while write_frame(&mut stream, &page) && request["operation"] == "events" {
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }
    });
}

async fn open_stream(router: &axum::Router, peer: &str, auth: &str) -> Response {
    let mut request = Request::builder()
        .uri("/api/leases/events")
        .header(header::AUTHORIZATION, auth)
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
    router.clone().oneshot(request).await.unwrap()
}

/// An open SSE stream holds its token's stream and in-flight allowances and
/// one of the listener's requests in flight until it closes; refusals are
/// the 429 and 503 every listener route gives (design §7.3 "Limits").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_notice_streams_count_toward_the_listeners_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut store = provisioned_store(&home);
    let mut tokens = Vec::new();
    for principal in ["alice", "bob", "carol"] {
        let (_, secret) = mint(
            &mut store,
            principal,
            PrincipalClass::Human,
            &["ws_alpha"],
            LeaseTokenVerb::Read,
            LeasePriorityCeiling::Normal,
            HardLeasePermission::Allow,
        );
        tokens.push(bearer(&secret));
    }
    let [alice, bob, carol]: [String; 3] = tokens.try_into().unwrap();
    let socket = tmp.path().join("intake.sock");
    scripted_intake(&socket, LEASE_INTAKE_PROTOCOL_VERSION);
    let mut settings = config(socket, LeaseClientAddressPolicy::Direct);
    settings.limits.max_in_flight_requests = 2;
    settings.limits.max_streams_per_token = 1;
    let router = lease_listener_router(&settings, store);

    let first = open_stream(&router, "192.0.2.10:1", &alice).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()[header::CONTENT_TYPE], "text/event-stream");
    let (status, _, body) = send(
        &router,
        "GET",
        "/api/leases/events",
        "192.0.2.10:1",
        &[("authorization", &alice)],
        None,
    )
    .await;
    assert_eq!(
        (status, body),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "token_streams"})
        )
    );

    let second = open_stream(&router, "192.0.2.11:1", &bob).await;
    assert_eq!(second.status(), StatusCode::OK);
    for (path, auth) in [("/api/leases/notices", carol.as_str()), ("/healthz", "")] {
        let (status, _, body) = send(
            &router,
            "GET",
            path,
            "192.0.2.12:1",
            &[("authorization", auth)],
            None,
        )
        .await;
        assert_eq!(
            (status, body),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "in_flight_requests"})
            ),
            "{path} while two streams fill the listener"
        );
    }

    // The request allowance returns with the body; the intake socket's
    // returns when the stream's reader sees the next 50 ms page.
    drop(first);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let page = loop {
        let (status, _, page) = send(
            &router,
            "GET",
            "/api/leases/notices?after=4",
            "192.0.2.12:1",
            &[("authorization", &carol)],
            None,
        )
        .await;
        if status == StatusCode::OK || tokio::time::Instant::now() > deadline {
            break page;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        page,
        json!({"notices": [], "cursor": 4}),
        "a closed stream returns its allowances"
    );
    drop(second);

    let reasons: Vec<String> = audit_rows(&home)
        .into_iter()
        .map(|row| row.reason)
        .collect();
    for reason in ["token_streams", "in_flight_requests"] {
        assert!(reasons.iter().any(|r| r == reason), "{reason}: {reasons:?}");
    }
}

/// Opening a stream spends a token read, so the read budget's 429 and
/// `Retry-After` apply to it as to every read route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_a_notice_stream_spends_the_tokens_read_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut store = provisioned_store(&home);
    let (_, secret) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Read,
        LeasePriorityCeiling::Normal,
        HardLeasePermission::Allow,
    );
    let alice = bearer(&secret);
    let socket = tmp.path().join("intake.sock");
    scripted_intake(&socket, LEASE_INTAKE_PROTOCOL_VERSION);
    let mut settings = config(socket, LeaseClientAddressPolicy::Direct);
    settings.limits.token_reads_per_minute = 1;
    let router = lease_listener_router(&settings, store);

    let open = open_stream(&router, "192.0.2.10:1", &alice).await;
    assert_eq!(open.status(), StatusCode::OK);
    let refused = open_stream(&router, "192.0.2.10:1", &alice).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(refused.headers().contains_key(header::RETRY_AFTER));
    drop((open, refused));
    let reasons: Vec<String> = audit_rows(&home)
        .into_iter()
        .map(|row| row.reason)
        .collect();
    assert_eq!(reasons, ["token_reads"]);
}

/// A stream holds its connection, so it counts toward `max_connections`,
/// and shutting the listener down ends it rather than waiting out the drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notice_stream_holds_a_connection_and_ends_at_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut store = provisioned_store(&home);
    let (_, secret) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Read,
        LeasePriorityCeiling::Normal,
        HardLeasePermission::Allow,
    );
    let socket = tmp.path().join("intake.sock");
    scripted_intake(&socket, LEASE_INTAKE_PROTOCOL_VERSION);
    let mut settings = config(socket, LeaseClientAddressPolicy::Direct);
    settings.limits.max_connections = 1;
    settings.limits.request_timeout_seconds = 60;
    let listener = tokio::net::TcpListener::bind(settings.bind).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve_lease_listener_on(listener, settings, store, async {
        let _ = shutdown.await;
    }));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut stream = client
        .get(format!("http://{address}/api/leases/events"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let chunk = tokio::time::timeout(Duration::from_secs(5), stream.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(&chunk[..], b": keep-alive\n\n");

    let excess = client
        .get(format!("http://{address}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(excess.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        excess.json::<Value>().await.unwrap(),
        json!({"error": "max_connections"})
    );

    stop.send(()).unwrap();
    // The drain would otherwise wait out the 65-second grace period.
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("an open notice stream must not hold the listener's shutdown")
        .unwrap()
        .unwrap();
    let rest = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = stream.chunk().await.transpose() {
            if chunk.is_err() {
                break;
            }
        }
    })
    .await;
    assert!(rest.is_ok(), "the stream ends with the listener");
}

/// The intake protocol is versioned (design §7.2 "Versioned"): the listener
/// refuses an Orbit of another version, naming both versions, and answers
/// 503 rather than reading notices across versions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_orbit_of_another_intake_protocol_version_is_refused_naming_both() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut store = provisioned_store(&home);
    let (_, secret) = mint(
        &mut store,
        "alice",
        PrincipalClass::Human,
        &["ws_alpha"],
        LeaseTokenVerb::Read,
        LeasePriorityCeiling::Normal,
        HardLeasePermission::Allow,
    );
    let socket = tmp.path().join("intake.sock");
    let other = LEASE_INTAKE_PROTOCOL_VERSION + 1;
    scripted_intake(&socket, other);

    let error = LeaseIntakeClient::connect(&socket, Duration::from_secs(5))
        .err()
        .expect("a refused handshake")
        .to_string();
    for version in [LEASE_INTAKE_PROTOCOL_VERSION, other] {
        assert!(error.contains(&format!("version {version}")), "{error}");
    }

    let router = lease_listener_router(&config(socket, LeaseClientAddressPolicy::Direct), store);
    for path in ["/api/leases/notices", "/api/leases/events"] {
        let (status, _, _) = send(
            &router,
            "GET",
            path,
            "192.0.2.10:1",
            &[("authorization", &bearer(&secret))],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
}

/// Any change to the operations changes the protocol, so its version must
/// change with it (design §7.2 "Versioned"). Adding an operation fails to
/// compile here until it is listed under a new version.
#[test]
fn the_intake_operations_are_those_of_the_current_protocol_version() {
    let spelled = |operation: LeaseIntakeOperation| match operation {
        LeaseIntakeOperation::Request
        | LeaseIntakeOperation::Renew
        | LeaseIntakeOperation::Expand
        | LeaseIntakeOperation::Release
        | LeaseIntakeOperation::Show
        | LeaseIntakeOperation::List
        | LeaseIntakeOperation::Check
        | LeaseIntakeOperation::Notices
        | LeaseIntakeOperation::Events => operation.as_str(),
    };
    let operations = [
        LeaseIntakeOperation::Request,
        LeaseIntakeOperation::Renew,
        LeaseIntakeOperation::Expand,
        LeaseIntakeOperation::Release,
        LeaseIntakeOperation::Show,
        LeaseIntakeOperation::List,
        LeaseIntakeOperation::Check,
        LeaseIntakeOperation::Notices,
        LeaseIntakeOperation::Events,
    ]
    .map(spelled);
    assert_eq!(
        (LEASE_INTAKE_PROTOCOL_VERSION, operations.as_slice()),
        (
            2,
            [
                "request", "renew", "expand", "release", "show", "list", "check", "notices",
                "events"
            ]
            .as_slice()
        ),
        "an intake operation was added, removed or renamed: bump \
         LEASE_INTAKE_PROTOCOL_VERSION and list the new version's operations here"
    );
}
