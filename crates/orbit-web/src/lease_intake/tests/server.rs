use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenScope, LeaseTokenVerb, PrincipalClass,
};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::task::locks::lease;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use serde_json::{Value, json};

use super::super::{
    IntakeLimits, LEASE_INTAKE_MAX_FRAME_BYTES, LEASE_INTAKE_PROTOCOL_VERSION,
    LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeError, LeaseIntakeHello,
    LeaseIntakeOperation, LeaseIntakeRequest, LeaseIntakeResponse, LeaseIntakeSocket,
};
use crate::api::tests::test_support::enter_isolated_child;
use crate::state::{DashboardState, RegistrySource, SINGLE_WORKSPACE_ID};

fn call(
    client: &mut LeaseIntakeClient,
    principal: &str,
    operation: LeaseIntakeOperation,
    input: Value,
) -> LeaseIntakeResponse {
    call_workspace(client, principal, SINGLE_WORKSPACE_ID, operation, input)
}

fn call_workspace(
    client: &mut LeaseIntakeClient,
    principal: &str,
    workspace: &str,
    operation: LeaseIntakeOperation,
    input: Value,
) -> LeaseIntakeResponse {
    let request = LeaseIntakeRequest {
        operation,
        workspace: workspace.to_string(),
        authority: LeaseIntakeAuthority {
            token_id: format!("tok-{principal}"),
            scope: LeaseTokenScope {
                principal: principal.to_string(),
                principal_class: PrincipalClass::Human,
                workspaces: vec![workspace.to_string()],
                verbs: vec![LeaseTokenVerb::Lease, LeaseTokenVerb::Read],
                max_priority: LeasePriorityCeiling::Normal,
                hard: HardLeasePermission::Allow,
            },
            client_address: "192.0.2.7".to_string(),
        },
        input,
    };
    client.call(&request).expect("intake call")
}

fn call_scoped(
    client: &mut LeaseIntakeClient,
    principal: &str,
    token_id: &str,
    max_priority: LeasePriorityCeiling,
    hard: HardLeasePermission,
    operation: LeaseIntakeOperation,
    input: Value,
) -> LeaseIntakeResponse {
    client
        .call(&LeaseIntakeRequest {
            operation,
            workspace: SINGLE_WORKSPACE_ID.to_string(),
            authority: LeaseIntakeAuthority {
                token_id: token_id.to_string(),
                scope: LeaseTokenScope {
                    principal: principal.to_string(),
                    principal_class: PrincipalClass::Human,
                    workspaces: vec![SINGLE_WORKSPACE_ID.to_string()],
                    verbs: vec![LeaseTokenVerb::Lease, LeaseTokenVerb::Read],
                    max_priority,
                    hard,
                },
                client_address: "192.0.2.7".to_string(),
            },
            input,
        })
        .expect("intake call")
}

fn seed_registered_workspace(base: &std::path::Path, id: &str) -> std::path::PathBuf {
    let repo_root = base.join(id);
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(orbit_dir.join("local")).expect("create workspace directories");
    std::fs::create_dir_all(repo_root.join("src")).expect("create source directory");
    std::fs::write(orbit_dir.join("config.toml"), "[lease]\nenabled = true\n")
        .expect("write lease config");
    std::fs::write(
        orbit_dir.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {id}\n"),
    )
    .expect("write workspace identity");
    std::fs::write(repo_root.join("src/lease.rs"), "pub fn lease() {}\n")
        .expect("write workspace file");
    std::fs::write(repo_root.join("src/new.rs"), "pub fn new() {}\n")
        .expect("write unleased workspace file");
    repo_root
}

fn ok(response: LeaseIntakeResponse) -> Value {
    match response {
        LeaseIntakeResponse::Ok { result } => result,
        LeaseIntakeResponse::Error(error) => panic!("intake refused: {error:?}"),
    }
}

fn refusal(response: LeaseIntakeResponse) -> LeaseIntakeError {
    match response {
        LeaseIntakeResponse::Error(error) => error,
        LeaseIntakeResponse::Ok { result } => panic!("intake allowed: {result}"),
    }
}

struct FrameLimitFixture {
    runtime: Arc<OrbitRuntime>,
    socket_path: std::path::PathBuf,
    _socket: LeaseIntakeSocket,
    _tokio: tokio::runtime::Runtime,
    _tmp: tempfile::TempDir,
}

impl FrameLimitFixture {
    fn start() -> Self {
        Self::start_with(IntakeLimits::default())
    }

    fn start_with(limits: IntakeLimits) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("global")).expect("global root");
        seed_registered_workspace(tmp.path(), "ws_frame_limit");
        Self::serve(tmp, limits, "intake.sock")
    }

    /// Stop serving and serve the same owner store again from a new runtime
    /// and socket, as after an Orbit Web or listener restart.
    fn restart(self, limits: IntakeLimits) -> Self {
        let Self {
            runtime,
            _socket,
            _tokio,
            _tmp,
            ..
        } = self;
        drop(_socket);
        _tokio.shutdown_background();
        drop(runtime);
        Self::serve(_tmp, limits, "intake-restarted.sock")
    }

    fn serve(tmp: tempfile::TempDir, limits: IntakeLimits, socket_name: &str) -> Self {
        let global_root = tmp.path().join("global");
        let repo = tmp.path().join("ws_frame_limit");
        let runtime = Arc::new(
            OrbitRuntime::from_roots(&global_root, &repo.join(".orbit")).expect("runtime"),
        );
        let socket_path = tmp.path().join(socket_name);
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind");
        // SAFETY: geteuid has no preconditions and cannot fail.
        let own_uid = unsafe { libc::geteuid() };
        let mut socket =
            LeaseIntakeSocket::for_test_listener(listener, socket_path.clone(), own_uid);
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let accept = {
            let _entered = tokio.enter();
            socket
                .serve(DashboardState::single(Arc::clone(&runtime)), limits)
                .expect("serve")
        };
        tokio.spawn(accept);
        Self {
            runtime,
            socket_path,
            _socket: socket,
            _tokio: tokio,
            _tmp: tmp,
        }
    }

    fn connect(&self) -> LeaseIntakeClient {
        LeaseIntakeClient::connect(&self.socket_path, Duration::from_secs(10)).expect("connect")
    }
}

fn oversized_response_paths() -> Vec<String> {
    (0..200)
        .map(|index| format!("file:src/{index:03}-{}.rs", "x".repeat(180)))
        .collect()
}

#[test]
fn oversized_read_responses_are_bounded_errors_and_other_connections_still_work() {
    if !enter_isolated_child(
        module_path!(),
        "oversized_read_responses_are_bounded_errors_and_other_connections_still_work",
    ) {
        return;
    }
    let fixture = FrameLimitFixture::start();
    let granted = lease::request(
        &fixture.runtime,
        json!({
            "principal": "alice", "selectors": ["dir:src"],
            "tags": ["x".repeat(LEASE_INTAKE_MAX_FRAME_BYTES as usize)]
        }),
        None,
        None,
    )
    .expect("seed a large lease through the runtime API");
    let lease_id = granted["lease"]["lease_id"].as_str().expect("lease id");
    let paths = oversized_response_paths();
    for (operation, input, result) in [
        (
            LeaseIntakeOperation::List,
            json!({}),
            lease::list(&fixture.runtime).unwrap(),
        ),
        (
            LeaseIntakeOperation::Show,
            json!({"lease_id": lease_id}),
            lease::show(&fixture.runtime, json!({"lease_id": lease_id})).unwrap(),
        ),
        (
            LeaseIntakeOperation::Check,
            json!({"paths": paths}),
            lease::check(&fixture.runtime, json!({"paths": paths})).unwrap(),
        ),
    ] {
        assert!(
            serde_json::to_vec(&LeaseIntakeResponse::Ok { result })
                .unwrap()
                .len()
                > LEASE_INTAKE_MAX_FRAME_BYTES as usize,
            "the fixture must exceed the real frame limit for {operation:?}"
        );
        let mut client = fixture.connect();
        let error = refusal(call(&mut client, "alice", operation, input));
        assert_eq!(error.code, "frame_too_large", "{operation:?}: {error:?}");

        // Also prove framing remains synchronized on the original connection.
        ok(call(
            &mut client,
            "alice",
            LeaseIntakeOperation::Check,
            json!({"paths": ["file:unheld.rs"]}),
        ));
        let mut second = fixture.connect();
        let small = ok(call(
            &mut second,
            "bob",
            LeaseIntakeOperation::Request,
            json!({"selectors": ["file:unheld.rs"]}),
        ));
        assert_eq!(small["granted"], true, "{small}");
    }
}

/// Every notice in the workspace, oldest first.
fn all_notices(runtime: &OrbitRuntime) -> Vec<Value> {
    let page = lease::notices(runtime, json!({"after": 0, "limit": 500})).expect("notices");
    let notices = page["notices"].as_array().expect("notices").clone();
    assert!(notices.len() < 500, "the fixture must read every notice");
    notices
}

fn active_lease_ids(runtime: &OrbitRuntime) -> Vec<String> {
    lease::list(runtime).expect("list")["leases"]
        .as_array()
        .expect("leases")
        .iter()
        .map(|lease| lease["lease_id"].as_str().expect("lease id").to_string())
        .collect()
}

/// A token low on priority with `hard = never` is still bounded (ORB-18776):
/// a request whose selectors exceed the token limit is refused before the
/// store writes a grant or any overlap notice.
#[test]
fn oversized_token_request_is_refused_before_any_grant_or_notice() {
    if !enter_isolated_child(
        module_path!(),
        "oversized_token_request_is_refused_before_any_grant_or_notice",
    ) {
        return;
    }
    let fixture = FrameLimitFixture::start();
    let held = lease::request(
        &fixture.runtime,
        json!({"principal": "alice", "selectors": ["dir:src"]}),
        None,
        None,
    )
    .expect("seed an overlapping lease");
    let notices_before = all_notices(&fixture.runtime);
    let mut client = fixture.connect();
    for _ in 0..2 {
        let error = refusal(call_scoped(
            &mut client,
            "bob",
            "tok-bob-low",
            LeasePriorityCeiling::Low,
            HardLeasePermission::Never,
            LeaseIntakeOperation::Request,
            json!({"selectors": oversized_response_paths(), "mode": "advisory"}),
        ));
        assert_eq!(error.code, "invalid_input", "{error:?}");
        assert_eq!(
            active_lease_ids(&fixture.runtime),
            [held["lease"]["lease_id"].as_str().unwrap()],
            "a refused request must not leave a grant"
        );
        assert_eq!(
            all_notices(&fixture.runtime),
            notices_before,
            "a refused request must not notify the holders it would overlap"
        );
    }

    // Nor can a token grow a small lease past the limit by expanding it.
    let small = ok(call_scoped(
        &mut client,
        "bob",
        "tok-bob-low",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/one.rs"]}),
    ));
    let lease_id = small["lease"]["lease_id"].clone();
    let notices_before = all_notices(&fixture.runtime);
    let error = refusal(call_scoped(
        &mut client,
        "bob",
        "tok-bob-low",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
        LeaseIntakeOperation::Expand,
        json!({"lease_id": lease_id, "selectors": oversized_response_paths()}),
    ));
    assert_eq!(error.code, "invalid_input", "{error:?}");
    let shown = lease::show(&fixture.runtime, json!({"lease_id": lease_id})).unwrap();
    assert_eq!(shown["lease"]["selectors"], json!(["file:src/one.rs"]));
    assert_eq!(all_notices(&fixture.runtime), notices_before);
}

/// With a frame too small for every overlap, a token grant answers with the
/// prefix that fits and `truncated: true` instead of being granted, notified
/// and then released (ORB-18776). Every overlapped holder's notice names a
/// lease that really exists. An answer that cannot fit even without
/// holders is refused before anything is written.
#[test]
fn token_grant_answers_fit_the_frame_without_compensating_release() {
    if !enter_isolated_child(
        module_path!(),
        "token_grant_answers_fit_the_frame_without_compensating_release",
    ) {
        return;
    }
    let limits = IntakeLimits {
        max_frame_bytes: 8 * 1024,
        ..IntakeLimits::default()
    };
    let fixture = FrameLimitFixture::start_with(limits);
    let held: Vec<String> = (0..40)
        .map(|index| {
            let granted = lease::request(
                &fixture.runtime,
                json!({
                    "principal": "alice",
                    "selectors": [format!("file:src/{index:02}-{}.rs", "h".repeat(120))],
                }),
                None,
                None,
            )
            .expect("seed a disjoint advisory lease");
            granted["lease"]["lease_id"].as_str().unwrap().to_string()
        })
        .collect();
    let mut client = fixture.connect();
    let mut low_never = |input: Value| {
        call_scoped(
            &mut client,
            "bob",
            "tok-bob-low",
            LeasePriorityCeiling::Low,
            HardLeasePermission::Never,
            LeaseIntakeOperation::Request,
            input,
        )
    };

    let response = low_never(json!({"selectors": ["dir:src"], "mode": "hard"}));
    let size = serde_json::to_vec(&response).unwrap().len();
    assert!(
        size <= 8 * 1024,
        "the answer must fit the frame: {size} bytes"
    );
    let granted = ok(response);
    assert_eq!(granted["granted"], true, "{granted}");
    assert_eq!(granted["downgraded"], true, "{granted}");
    assert_eq!(granted["truncated"], true, "{granted}");
    let overlaps = granted["overlaps"].as_array().unwrap();
    assert!(
        !overlaps.is_empty() && overlaps.len() < held.len(),
        "{granted}"
    );
    let shown: Vec<_> = overlaps.iter().map(|o| o["held_by_id"].clone()).collect();
    let oldest: Vec<_> = held[..overlaps.len()].iter().map(|id| json!(id)).collect();
    assert_eq!(shown, oldest, "the overlaps are the oldest holders' prefix");
    let lease_id = granted["lease"]["lease_id"].as_str().unwrap();
    assert!(
        active_lease_ids(&fixture.runtime).contains(&lease_id.to_string()),
        "the grant is delivered, not released"
    );
    let overlap_notices: Vec<_> = all_notices(&fixture.runtime)
        .into_iter()
        .filter(|notice| notice["kind"] == "overlap")
        .collect();
    assert_eq!(
        overlap_notices.len(),
        held.len(),
        "every holder is notified"
    );
    for notice in overlap_notices {
        assert_eq!(notice["payload"]["lease_id"], lease_id, "{notice}");
    }

    let leases_before = active_lease_ids(&fixture.runtime);
    let notices_before = all_notices(&fixture.runtime);
    let error = refusal(low_never(json!({
        "selectors": ["dir:src"], "tags": ["t".repeat(7500)],
    })));
    assert_eq!(error.code, "invalid_input", "{error:?}");
    assert_eq!(active_lease_ids(&fixture.runtime), leases_before);
    assert_eq!(all_notices(&fixture.runtime), notices_before);
}

/// Bob's low, never-hard tokens may hold at most the token limit of active
/// leases in a workspace (ORB-18776). The count is the owner's lease rows by
/// principal, so it survives a restart and a replacement token, and a refusal
/// writes neither a lease nor an overlap notice.
#[test]
fn token_active_leases_are_bounded_per_principal_across_restart_and_tokens() {
    if !enter_isolated_child(
        module_path!(),
        "token_active_leases_are_bounded_per_principal_across_restart_and_tokens",
    ) {
        return;
    }
    const LIMIT: usize = 64;
    let fixture = FrameLimitFixture::start();
    let request = |client: &mut LeaseIntakeClient, token: &str, selector: &str| {
        ok(call_scoped(
            client,
            "bob",
            token,
            LeasePriorityCeiling::Low,
            HardLeasePermission::Never,
            LeaseIntakeOperation::Request,
            json!({"selectors": [selector]}),
        ))
    };
    let mut client = fixture.connect();
    let mut first = None;
    for index in 0..LIMIT {
        let granted = request(&mut client, "tok-bob-1", &format!("file:src/{index:02}.rs"));
        assert_eq!(granted["granted"], true, "{granted}");
        first.get_or_insert_with(|| granted["lease"]["lease_id"].clone());
    }
    let refused_unchanged = |runtime: &OrbitRuntime, refused: Value| {
        assert_eq!(refused["granted"], false, "{refused}");
        assert_eq!(refused["reason"], "active_lease_limit", "{refused}");
        assert_eq!(refused["limit"], LIMIT, "{refused}");
        assert_eq!(active_lease_ids(runtime).len(), LIMIT);
    };
    let notices = all_notices(&fixture.runtime);
    // A directory over every held file would notify each of them.
    let refused = request(&mut client, "tok-bob-1", "dir:src");
    refused_unchanged(&fixture.runtime, refused);
    assert_eq!(all_notices(&fixture.runtime), notices);
    drop(client);

    let fixture = fixture.restart(IntakeLimits::default());
    let mut client = fixture.connect();
    let refused = request(&mut client, "tok-bob-2", "dir:src");
    refused_unchanged(&fixture.runtime, refused);
    assert_eq!(all_notices(&fixture.runtime), notices);

    let released = ok(call_scoped(
        &mut client,
        "bob",
        "tok-bob-2",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
        LeaseIntakeOperation::Release,
        json!({"lease_id": first.expect("first lease")}),
    ));
    assert_eq!(released["released"], true, "{released}");
    let granted = request(&mut client, "tok-bob-2", "file:src/again.rs");
    assert_eq!(granted["granted"], true, "a released lease frees its place");

    // The limit is per principal: another principal is not refused.
    let other = ok(call(
        &mut client,
        "carol",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["dir:src"]}),
    ));
    assert_eq!(other["granted"], true, "{other}");
    assert_eq!(
        other["overlaps"].as_array().unwrap().len(),
        LIMIT,
        "{other}"
    );
}

#[test]
fn token_renewal_obeys_current_scope_and_workspace_policy() {
    if !enter_isolated_child(
        module_path!(),
        "token_renewal_obeys_current_scope_and_workspace_policy",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
        ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
    ]);
    let fixture = FrameLimitFixture::start();
    let mut client = fixture.connect();
    let granted = ok(call_scoped(
        &mut client,
        "alice",
        "high-token",
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/lease.rs"], "mode": "hard", "priority": "high"}),
    ));
    let id = granted["lease"]["lease_id"].as_str().expect("lease id");
    assert_eq!(granted["lease"]["mode"], "hard");
    assert_eq!(granted["lease"]["priority"], "high");
    let before = granted["lease"]["expires_at"].clone();
    let narrow = refusal(call_scoped(
        &mut client,
        "alice",
        "narrow-token",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
        LeaseIntakeOperation::Renew,
        json!({"lease_id": id, "ttl_seconds": 14400}),
    ));
    assert_eq!(narrow.code, "invalid_input");
    let shown = ok(call(
        &mut client,
        "alice",
        LeaseIntakeOperation::Show,
        json!({"lease_id": id}),
    ));
    assert_eq!(shown["lease"]["expires_at"], before);
    assert_eq!(shown["lease"]["mode"], "hard");
    assert_eq!(shown["lease"]["priority"], "high");
    let renewed = ok(call_scoped(
        &mut client,
        "alice",
        "high-token",
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Renew,
        json!({"lease_id": id, "ttl_seconds": 600}),
    ));
    assert_eq!(renewed["renewed"], true);
    let other = refusal(call(
        &mut client,
        "bob",
        LeaseIntakeOperation::Renew,
        json!({"lease_id": id}),
    ));
    assert_eq!(other.code, "lease_not_held");
    // The service admits policy at startup. Reopen the same store under a
    // tightened policy and exercise the actual intake socket again.
    let root = fixture._tmp.path();
    let workspace = root.join("ws_frame_limit/.orbit");
    std::fs::write(workspace.join("config.toml"),
        "[lease]\nenabled = true\n[lease.hard]\nhuman = \"never\"\n[lease.max_priority]\nhuman = \"low\"\n").unwrap();
    let restricted = Arc::new(OrbitRuntime::from_roots(&root.join("global"), &workspace).unwrap());
    let path = root.join("restricted.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut socket = LeaseIntakeSocket::for_test_listener(listener, path.clone(), uid);
    let accept = {
        let _entered = fixture._tokio.enter();
        socket
            .serve(
                DashboardState::single(Arc::clone(&restricted)),
                IntakeLimits::default(),
            )
            .unwrap()
    };
    fixture._tokio.spawn(accept);
    let mut restricted_client = LeaseIntakeClient::connect(&path, Duration::from_secs(10)).unwrap();
    let denied = refusal(call_scoped(
        &mut restricted_client,
        "alice",
        "high-token",
        LeasePriorityCeiling::High,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Renew,
        json!({"lease_id": id, "ttl_seconds": 14400}),
    ));
    assert_eq!(denied.code, "invalid_input");
    let operator = lease::renew(&restricted, json!({"lease_id": id, "ttl_seconds": 600})).unwrap();
    assert_eq!(operator["renewed"], true);
    let compliant = ok(call_scoped(
        &mut restricted_client,
        "alice",
        "narrow-token",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/new.rs"], "mode": "advisory", "priority": "low"}),
    ));
    let compliant_id = compliant["lease"]["lease_id"].as_str().unwrap();
    let renewed = ok(call_scoped(
        &mut restricted_client,
        "alice",
        "narrow-token",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
        LeaseIntakeOperation::Renew,
        json!({"lease_id": compliant_id}),
    ));
    assert_eq!(renewed["renewed"], true);
    let released = ok(call_scoped(
        &mut restricted_client,
        "alice",
        "narrow-token",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Never,
        LeaseIntakeOperation::Release,
        json!({"lease_id": id}),
    ));
    assert_eq!(released["released"], true);
}

/// Review finding F1 on ORB-18430: a token renewed and released any lease
/// in its workspace, so bob's token could end alice's hard lease and take
/// its paths. A token now acts only on its own principal's leases, and an
/// unknown lease is refused identically.
#[test]
fn token_renews_and_releases_only_its_own_principals_leases() {
    if !enter_isolated_child(
        module_path!(),
        "token_renews_and_releases_only_its_own_principals_leases",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
        ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
    ]);
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    let repo_root = tmp.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).unwrap();
    std::fs::create_dir_all(repo_root.join("src")).unwrap();
    std::fs::create_dir_all(&workspace_root).unwrap();
    std::fs::write(repo_root.join("src/x.rs"), "pub fn x() {}\n").unwrap();
    std::fs::write(
        workspace_root.join("config.toml"),
        "[lease]\nenabled = true\n",
    )
    .unwrap();
    let runtime =
        Arc::new(OrbitRuntime::from_roots(&global_root, &workspace_root).expect("runtime"));
    let state = DashboardState::single(runtime);

    let socket_path = tmp.path().join("intake.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind");
    // SAFETY: geteuid has no preconditions and cannot fail.
    let own_uid = unsafe { libc::geteuid() };
    let mut socket = LeaseIntakeSocket::for_test_listener(listener, socket_path.clone(), own_uid);
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let accept = {
        let _entered = tokio.enter();
        socket.serve(state, IntakeLimits::default()).expect("serve")
    };
    tokio.spawn(accept);
    let mut client =
        LeaseIntakeClient::connect(&socket_path, Duration::from_secs(10)).expect("connect");

    let granted = ok(call(
        &mut client,
        "alice",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/x.rs"], "mode": "hard"}),
    ));
    let lease_id = granted["lease"]["lease_id"].as_str().expect("lease id");
    assert_eq!(granted["lease"]["mode"], "hard", "{granted}");

    let unknown = refusal(call(
        &mut client,
        "bob",
        LeaseIntakeOperation::Release,
        json!({"lease_id": "lease-unknown"}),
    ));
    assert_eq!(unknown.code, "lease_not_held");
    for operation in [LeaseIntakeOperation::Release, LeaseIntakeOperation::Renew] {
        let refused = refusal(call(
            &mut client,
            "bob",
            operation,
            json!({"lease_id": lease_id, "ttl_seconds": 600}),
        ));
        assert_eq!(
            refused, unknown,
            "another principal's lease must be refused exactly like an unknown one"
        );
    }
    let shown = ok(call(
        &mut client,
        "bob",
        LeaseIntakeOperation::Show,
        json!({"lease_id": lease_id}),
    ));
    assert_eq!(shown["lease"]["state"], "granted", "{shown}");
    let taken = ok(call(
        &mut client,
        "bob",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/x.rs"], "mode": "hard"}),
    ));
    assert_eq!(
        taken["granted"], false,
        "alice's hard lease still holds: {taken}"
    );

    let renewed = ok(call(
        &mut client,
        "alice",
        LeaseIntakeOperation::Renew,
        json!({"lease_id": lease_id, "ttl_seconds": 600}),
    ));
    assert_eq!(renewed["renewed"], true, "{renewed}");
    let released = ok(call(
        &mut client,
        "alice",
        LeaseIntakeOperation::Release,
        json!({"lease_id": lease_id}),
    ));
    assert_eq!(released["released"], true, "{released}");
    drop(client);
    tokio.shutdown_background();
}

#[test]
fn ambiguous_workspace_id_name_selector_is_refused_without_touching_either_workspace() {
    if !enter_isolated_child(
        module_path!(),
        "ambiguous_workspace_id_name_selector_is_refused_without_touching_either_workspace",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
        ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
    ]);
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    let first_repo = seed_registered_workspace(tmp.path(), "ws_alpha");
    let second_repo = seed_registered_workspace(tmp.path(), "ws_ws_alpha");

    let now = Utc::now();
    let registry = WorkspaceRegistry {
        workspaces: vec![
            Workspace {
                id: "ws_alpha".to_string(),
                name: "alpha".to_string(),
                owner_machine_id: None,
                git_remote: None,
                ship_mode: None,
                base_branch: "main".to_string(),
                status: WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
            },
            Workspace {
                id: "ws_ws_alpha".to_string(),
                name: "ws_alpha".to_string(),
                owner_machine_id: None,
                git_remote: None,
                ship_mode: None,
                base_branch: "main".to_string(),
                status: WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
            },
        ],
        checkouts: vec![
            WorkspaceCheckout::owner(
                "ws_alpha".to_string(),
                first_repo.clone(),
                first_repo.join(".orbit"),
            ),
            WorkspaceCheckout::owner(
                "ws_ws_alpha".to_string(),
                second_repo.clone(),
                second_repo.join(".orbit"),
            ),
        ],
        ..WorkspaceRegistry::default()
    };
    orbit_registry::workspace_registry::save_registry_to(
        &registry,
        &global_root.join("workspaces.json"),
    )
    .expect("save registry");
    let state = DashboardState::from_registry(
        global_root.clone(),
        RegistrySource::new(global_root.join("workspaces.json"), None, None),
    )
    .expect("registry-backed dashboard state");

    let socket_path = tmp.path().join("intake.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind");
    // SAFETY: geteuid has no preconditions and cannot fail.
    let own_uid = unsafe { libc::geteuid() };
    let mut socket = LeaseIntakeSocket::for_test_listener(listener, socket_path.clone(), own_uid);
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let accept = {
        let _entered = tokio.enter();
        socket.serve(state, IntakeLimits::default()).expect("serve")
    };
    tokio.spawn(accept);
    let mut client =
        LeaseIntakeClient::connect(&socket_path, Duration::from_secs(10)).expect("connect");

    let first_grant = ok(call_workspace(
        &mut client,
        "alice",
        "alpha",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/lease.rs"], "mode": "hard"}),
    ));
    let first_lease_id = first_grant["lease"]["lease_id"]
        .as_str()
        .expect("first lease id")
        .to_string();
    let first_expiry = first_grant["lease"]["expires_at"]
        .as_str()
        .expect("first lease expiry")
        .to_string();
    assert_eq!(first_grant["granted"], true, "{first_grant}");

    let second_grant = ok(call_workspace(
        &mut client,
        "alice",
        "ws_ws_alpha",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/lease.rs"], "mode": "hard"}),
    ));
    let second_lease_id = second_grant["lease"]["lease_id"]
        .as_str()
        .expect("second lease id")
        .to_string();
    let second_expiry = second_grant["lease"]["expires_at"]
        .as_str()
        .expect("second lease expiry")
        .to_string();
    assert_eq!(second_grant["granted"], true, "{second_grant}");

    let ambiguous_request = refusal(call_workspace(
        &mut client,
        "alice",
        "ws_alpha",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:src/new.rs"], "mode": "hard"}),
    ));
    assert_eq!(ambiguous_request.code, "unknown_workspace");

    for lease_id in [&first_lease_id, &second_lease_id] {
        let ambiguous_renew = refusal(call_workspace(
            &mut client,
            "alice",
            "ws_alpha",
            LeaseIntakeOperation::Renew,
            json!({"lease_id": lease_id, "ttl_seconds": 600}),
        ));
        assert_eq!(ambiguous_renew.code, "unknown_workspace");

        let ambiguous_release = refusal(call_workspace(
            &mut client,
            "alice",
            "ws_alpha",
            LeaseIntakeOperation::Release,
            json!({"lease_id": lease_id}),
        ));
        assert_eq!(ambiguous_release.code, "unknown_workspace");
    }

    for (workspace, lease_id, expected_expiry) in [
        ("alpha", first_lease_id, first_expiry),
        ("ws_ws_alpha", second_lease_id, second_expiry),
    ] {
        let shown = ok(call_workspace(
            &mut client,
            "alice",
            workspace,
            LeaseIntakeOperation::Show,
            json!({"lease_id": lease_id}),
        ));
        assert_eq!(shown["lease"]["state"], "granted", "{shown}");
        assert_eq!(shown["lease"]["expires_at"], expected_expiry, "{shown}");

        let listed = ok(call_workspace(
            &mut client,
            "alice",
            workspace,
            LeaseIntakeOperation::List,
            json!({}),
        ));
        let leases = listed["leases"].as_array().expect("lease list");
        assert_eq!(leases.len(), 1, "{listed}");
        assert_eq!(leases[0]["lease_id"], lease_id, "{listed}");
    }

    drop(client);
    tokio.shutdown_background();
}

/// A notice page over the frame limit is shortened to a prefix, with the
/// cursor at its last notice, so polling and the `events` stream deliver
/// every notice in order, exactly as the runtime pages them, and skip none
/// (design §5 "Push and poll").
#[test]
fn notice_pages_over_the_frame_limit_are_shortened_without_skipping_a_notice() {
    if !enter_isolated_child(
        module_path!(),
        "notice_pages_over_the_frame_limit_are_shortened_without_skipping_a_notice",
    ) {
        return;
    }
    let fixture = FrameLimitFixture::start();
    // Each `granted` notice names its selectors, about 18 KiB here.
    for lease in 0..6 {
        let selectors: Vec<String> = (0..90)
            .map(|index| format!("file:src/{lease}-{index:03}-{}.rs", "x".repeat(180)))
            .collect();
        lease::request(
            &fixture.runtime,
            json!({"principal": "alice", "selectors": selectors}),
            None,
            None,
        )
        .expect("seed a lease with a large granted notice");
    }
    let expected = lease::notices(&fixture.runtime, json!({"after": 0})).unwrap();
    let expected = expected["notices"].as_array().unwrap().clone();
    assert!(
        serde_json::to_vec(&expected).unwrap().len() > LEASE_INTAKE_MAX_FRAME_BYTES as usize,
        "the fixture must exceed the real frame limit"
    );

    let mut client = fixture.connect();
    let mut polled = Vec::new();
    let mut after = 0;
    while polled.len() < expected.len() {
        let page = ok(call(
            &mut client,
            "alice",
            LeaseIntakeOperation::Notices,
            json!({"after": after}),
        ));
        let notices = page["notices"].as_array().unwrap();
        assert!(
            !notices.is_empty() && notices.len() < expected.len(),
            "{page}"
        );
        assert_eq!(page["cursor"], notices.last().unwrap()["seq"]);
        after = page["cursor"].as_u64().unwrap();
        polled.extend(notices.iter().cloned());
    }
    assert_eq!(polled, expected);

    let mut stream = fixture.connect();
    let mut page = ok(call(
        &mut stream,
        "alice",
        LeaseIntakeOperation::Events,
        json!({"after": 0}),
    ));
    let mut streamed = Vec::new();
    while streamed.len() < expected.len() {
        streamed.extend(page["notices"].as_array().unwrap().iter().cloned());
        if streamed.len() < expected.len() {
            page = ok(stream.next_response().expect("next page"));
        }
    }
    assert_eq!(streamed, expected);
}

/// The intake protocol is versioned (design §7.2 "Versioned"): a listener
/// of another version is refused with a message naming both versions.
#[test]
fn a_listener_of_another_protocol_version_is_refused_naming_both_versions() {
    use std::io::{Read, Write};

    if !enter_isolated_child(
        module_path!(),
        "a_listener_of_another_protocol_version_is_refused_naming_both_versions",
    ) {
        return;
    }
    let fixture = FrameLimitFixture::start();
    let mut stream =
        std::os::unix::net::UnixStream::connect(&fixture.socket_path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let older = LEASE_INTAKE_PROTOCOL_VERSION - 1;
    let hello = serde_json::to_vec(&json!({"protocol_version": older})).unwrap();
    stream
        .write_all(&(hello.len() as u32).to_be_bytes())
        .and_then(|()| stream.write_all(&hello))
        .expect("send hello");
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).expect("reply header");
    let mut body = vec![0u8; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut body).expect("reply body");
    let reply: LeaseIntakeHello = serde_json::from_slice(&body).expect("reply hello");
    let error = reply.error.expect("a refusal");
    assert_eq!(error.code, "protocol_version_mismatch");
    for version in [LEASE_INTAKE_PROTOCOL_VERSION, older] {
        assert!(
            error.message.contains(&format!("version {version}")),
            "{}",
            error.message
        );
    }
}
