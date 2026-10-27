use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenScope, LeaseTokenVerb, PrincipalClass,
};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::task::locks::lease;
use orbit_types::task::TaskStatus;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use serde_json::{Value, json};

use super::super::api::tests::test_support::enter_isolated_child;
use super::super::lease_intake::{
    IntakeLimits, LEASE_INTAKE_MAX_FRAME_BYTES, LEASE_INTAKE_PROTOCOL_VERSION,
    LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeError, LeaseIntakeHello,
    LeaseIntakeOperation, LeaseIntakeRequest, LeaseIntakeResponse, LeaseIntakeSocket,
};
use super::super::state::{DashboardState, RegistrySource, SINGLE_WORKSPACE_ID};

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

/// Retrying selectors already queued by a hard token lease is a no-op within
/// the token's 64-selector surface (ORB-18776 regression).
#[test]
fn token_queued_selector_expansion_retries_do_not_double_count() {
    if !enter_isolated_child(
        module_path!(),
        "token_queued_selector_expansion_retries_do_not_double_count",
    ) {
        return;
    }
    let fixture = FrameLimitFixture::start();
    let blocked_dir = fixture._tmp.path().join("ws_frame_limit/blocked");
    std::fs::create_dir_all(&blocked_dir).expect("create blocked selector directory");
    for index in 0..64 {
        std::fs::write(blocked_dir.join(format!("{index:02}.rs")), "")
            .expect("create blocked selector file");
    }
    std::fs::create_dir_all(fixture._tmp.path().join("ws_frame_limit/free"))
        .expect("create free selector directory");
    std::fs::write(fixture._tmp.path().join("ws_frame_limit/free/own.rs"), "")
        .expect("create free selector file");
    std::fs::write(fixture._tmp.path().join("ws_frame_limit/free/carol.rs"), "")
        .expect("create Carol selector file");

    lease::request(
        &fixture.runtime,
        json!({"principal": "alice", "selectors": ["dir:blocked"], "mode": "hard"}),
        None,
        None,
    )
    .expect("Alice holds the blocked directory");
    let mut client = fixture.connect();
    let bob = ok(call_scoped(
        &mut client,
        "bob",
        "tok-bob-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:free/own.rs"], "mode": "hard"}),
    ));
    let bob_lease_id = bob["lease"]["lease_id"]
        .as_str()
        .expect("Bob lease id")
        .to_string();
    let bob_held = bob["lease"]["selectors"].clone();
    let bob_queued: Vec<String> = (0..63)
        .map(|index| format!("file:blocked/{index:02}.rs"))
        .collect();
    let expanded = ok(call_scoped(
        &mut client,
        "bob",
        "tok-bob-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Expand,
        json!({"lease_id": bob_lease_id, "selectors": bob_queued.clone()}),
    ));
    assert_eq!(expanded["queued"], json!(bob_queued));
    assert_eq!(
        expanded["queued"].as_array().unwrap().len(),
        63,
        "{expanded}"
    );
    assert_eq!(expanded["lease"]["selectors"], bob_held);
    let bob_queued_surface = expanded["lease"]["queued_selectors"].clone();
    assert_eq!(
        bob_queued_surface.as_array().unwrap().len(),
        63,
        "{expanded}"
    );

    let retry_one = ok(call_scoped(
        &mut client,
        "bob",
        "tok-bob-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Expand,
        json!({"lease_id": bob_lease_id, "selectors": ["file:blocked/00.rs"]}),
    ));
    assert_eq!(retry_one["added"], json!([]));
    assert_eq!(retry_one["queued"], json!([]));
    assert_eq!(retry_one["lease"]["selectors"], bob_held);
    assert_eq!(retry_one["lease"]["queued_selectors"], bob_queued_surface);

    let retry_batch = ok(call_scoped(
        &mut client,
        "bob",
        "tok-bob-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Expand,
        json!({
            "lease_id": bob_lease_id,
            "selectors": (0..63)
                .map(|index| format!("file:blocked/{index:02}.rs"))
                .collect::<Vec<_>>(),
        }),
    ));
    assert_eq!(retry_batch["added"], json!([]));
    assert_eq!(retry_batch["queued"], json!([]));
    assert_eq!(retry_batch["lease"]["selectors"], bob_held);
    assert_eq!(retry_batch["lease"]["queued_selectors"], bob_queued_surface);

    let before_new_selector = retry_batch["lease"].clone();
    let notices_before_new_selector = all_notices(&fixture.runtime);
    let too_many = refusal(call_scoped(
        &mut client,
        "bob",
        "tok-bob-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Expand,
        json!({"lease_id": bob_lease_id, "selectors": ["file:blocked/63.rs"]}),
    ));
    assert_eq!(too_many.code, "invalid_input", "{too_many:?}");
    let bob_after_refusal =
        lease::show(&fixture.runtime, json!({"lease_id": bob_lease_id})).expect("show Bob lease");
    assert_eq!(bob_after_refusal["lease"], before_new_selector);
    assert_eq!(all_notices(&fixture.runtime), notices_before_new_selector);

    let carol = ok(call_scoped(
        &mut client,
        "carol",
        "tok-carol-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:free/carol.rs"], "mode": "hard"}),
    ));
    let carol_lease_id = carol["lease"]["lease_id"].as_str().expect("Carol lease id");
    let carol_queued: Vec<String> = (0..60)
        .map(|index| format!("file:blocked/{index:02}.rs"))
        .collect();
    let carol_expanded = ok(call_scoped(
        &mut client,
        "carol",
        "tok-carol-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Expand,
        json!({"lease_id": carol_lease_id, "selectors": carol_queued.clone()}),
    ));
    assert_eq!(carol_expanded["queued"], json!(carol_queued));
    assert_eq!(carol_expanded["queued"].as_array().unwrap().len(), 60);

    let mut mixed = vec![
        "file:blocked/00.rs".to_string(),
        "file:blocked/01.rs".to_string(),
        "file:blocked/02.rs".to_string(),
    ];
    mixed.extend((60..63).map(|index| format!("file:blocked/{index:02}.rs")));
    let mixed_result = ok(call_scoped(
        &mut client,
        "carol",
        "tok-carol-hard",
        LeasePriorityCeiling::Low,
        HardLeasePermission::Allow,
        LeaseIntakeOperation::Expand,
        json!({"lease_id": carol_lease_id, "selectors": mixed}),
    ));
    let newly_queued = mixed_result["queued"]
        .as_array()
        .unwrap()
        .iter()
        .map(|selector| selector.as_str().unwrap().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        newly_queued,
        (60..63)
            .map(|index| format!("file:blocked/{index:02}.rs"))
            .collect()
    );
    assert_eq!(
        mixed_result["lease"]["selectors"],
        json!(["file:free/carol.rs"])
    );
    let final_carol_queued = mixed_result["lease"]["queued_selectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|selector| selector.as_str().unwrap().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        final_carol_queued,
        (0..63)
            .map(|index| format!("file:blocked/{index:02}.rs"))
            .collect()
    );
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

/// A hard token request or expansion blocked by more holders than its frame
/// can name answers, and writes its `queued` notice with, only the prefix that
/// fits (ORB-19006, ORB-19048), while every holder, shown or not, still keeps
/// it waiting.
#[test]
fn token_hard_holder_lists_are_bounded_while_every_holder_still_blocks() {
    if !enter_isolated_child(
        module_path!(),
        "token_hard_holder_lists_are_bounded_while_every_holder_still_blocks",
    ) {
        return;
    }
    const FRAME: usize = 4 * 1024;
    // Past the frame's conflict count bound as well as its byte budget, so a
    // count overflow must not hide the prefix that the bytes still fit.
    const HOLDERS: usize = 100;
    let fixture = FrameLimitFixture::start_with(IntakeLimits {
        max_frame_bytes: FRAME as u32,
        ..IntakeLimits::default()
    });
    let repo = fixture._tmp.path().join("ws_frame_limit");
    std::fs::create_dir_all(repo.join("own")).expect("create own directory");
    std::fs::write(repo.join("own/carol.rs"), "").expect("create Carol's file");
    let mut held: Vec<String> = (0..HOLDERS)
        .map(|index| {
            let granted = lease::request(
                &fixture.runtime,
                json!({
                    "principal": "alice",
                    "selectors": [format!("file:src/{index:02}.rs")],
                    "mode": "hard",
                }),
                None,
                None,
            )
            .expect("seed a disjoint hard lease");
            assert_eq!(granted["lease"]["mode"], "hard", "{granted}");
            granted["lease"]["lease_id"].as_str().unwrap().to_string()
        })
        .collect();
    // Conflicts on one selector are ordered by holder id.
    held.sort();
    let shown_prefix = |answer: &Value| -> Vec<String> {
        let conflicts = answer["conflicts"].as_array().expect("conflicts");
        let ids: Vec<String> = conflicts
            .iter()
            .map(|conflict| {
                assert_eq!(conflict["file"], "dir:src", "{conflict}");
                assert_eq!(conflict["held_by"], "lease", "{conflict}");
                conflict["held_by_id"].as_str().unwrap().to_string()
            })
            .collect();
        assert!(!ids.is_empty() && ids.len() < held.len(), "{answer}");
        assert_eq!(ids, held[..ids.len()], "the conflicts are a prefix");
        ids
    };
    let queued_notice = |lease_id: &str| -> Value {
        let notices: Vec<_> = all_notices(&fixture.runtime)
            .into_iter()
            .filter(|notice| notice["lease_id"] == lease_id && notice["kind"] == "queued")
            .collect();
        assert_eq!(notices.len(), 1, "{notices:?}");
        notices[0].clone()
    };
    let state = |lease_id: &str| {
        lease::show(&fixture.runtime, json!({"lease_id": lease_id})).expect("show lease")["lease"]
            .clone()
    };
    let mut client = fixture.connect();
    let mut hard_token = |principal: &str, operation, input: Value| {
        let response = call(&mut client, principal, operation, input);
        let size = serde_json::to_vec(&response).unwrap().len();
        assert!(size <= FRAME, "the answer must fit the frame: {size} bytes");
        ok(response)
    };

    // A queued expansion names a prefix of the holders, and so does its
    // persisted notice.
    let carol = hard_token(
        "carol",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["file:own/carol.rs"], "mode": "hard"}),
    );
    let carol_id = carol["lease"]["lease_id"].as_str().unwrap().to_string();
    let expanded = hard_token(
        "carol",
        LeaseIntakeOperation::Expand,
        json!({"lease_id": carol_id, "selectors": ["dir:src"]}),
    );
    assert_eq!(expanded["added"], json!([]), "{expanded}");
    assert_eq!(expanded["queued"], json!(["dir:src"]), "{expanded}");
    assert_eq!(expanded["truncated"], true, "{expanded}");
    shown_prefix(&expanded);
    let notice = queued_notice(&carol_id);
    assert_eq!(
        notice["payload"]["holders"], expanded["conflicts"],
        "{notice}"
    );
    assert_eq!(notice["payload"]["truncated"], true, "{notice}");

    // So does a queued request; nothing is left for `queued_ahead` once the
    // conflicts are cut, though Carol's expansion still ranks ahead.
    let bob = hard_token(
        "bob",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["dir:src"], "mode": "hard"}),
    );
    assert_eq!(bob["granted"], false, "{bob}");
    assert_eq!(bob["queued"], true, "{bob}");
    assert_eq!(bob["queue_position"], 2, "{bob}");
    assert_eq!(bob["truncated"], true, "{bob}");
    assert_eq!(bob["queued_ahead"], json!([]), "{bob}");
    shown_prefix(&bob);
    let bob_id = bob["lease"]["lease_id"].as_str().unwrap().to_string();
    let notice = queued_notice(&bob_id);
    assert_eq!(notice["payload"]["holders"], bob["conflicts"], "{notice}");
    assert_eq!(notice["payload"]["queued_ahead"], json!([]), "{notice}");
    assert_eq!(notice["payload"]["truncated"], true, "{notice}");

    // A request that may not wait is refused with the same bounded answer
    // and writes nothing.
    let leases_before = active_lease_ids(&fixture.runtime);
    let notices_before = all_notices(&fixture.runtime);
    let refused = hard_token(
        "dave",
        LeaseIntakeOperation::Request,
        json!({"selectors": ["dir:src"], "mode": "hard", "wait": false}),
    );
    assert_eq!(refused["granted"], false, "{refused}");
    assert_eq!(refused["queued"], false, "{refused}");
    assert_eq!(refused["truncated"], true, "{refused}");
    shown_prefix(&refused);
    assert_eq!(active_lease_ids(&fixture.runtime), leases_before);
    assert_eq!(all_notices(&fixture.runtime), notices_before);

    // Holders past the shown prefix still block: releasing all but the last
    // grants nothing.
    let (last, rest) = held.split_last().unwrap();
    for lease_id in rest {
        let released = lease::release(&fixture.runtime, json!({"lease_id": lease_id}), None, None)
            .expect("release a holder");
        assert_eq!(released["released"], true, "{released}");
    }
    assert_eq!(state(&carol_id)["queued_selectors"], json!(["dir:src"]));
    assert_eq!(state(&bob_id)["state"], "queued");

    // Once the last goes, the queue grants in order: Carol's expansion, which
    // then holds the paths Bob waits for.
    lease::release(&fixture.runtime, json!({"lease_id": last}), None, None)
        .expect("release the last holder");
    let carol = state(&carol_id);
    assert_eq!(
        carol["selectors"],
        json!(["dir:src", "file:own/carol.rs"]),
        "{carol}"
    );
    assert_eq!(carol["queued_selectors"], json!([]), "{carol}");
    assert_eq!(state(&bob_id)["state"], "queued");
}

/// A byte-derived task-conflict pre-bound keeps one extra witness so a
/// fitting prefix cannot be mistaken for the complete holder list.
#[test]
fn token_task_holder_prebound_reports_truncation_for_requests_and_expansions() {
    if !enter_isolated_child(
        module_path!(),
        "token_task_holder_prebound_reports_truncation_for_requests_and_expansions",
    ) {
        return;
    }
    const FRAME: usize = 4 * 1024;
    const BLOCKED: &str = "file:a";
    let fixture = FrameLimitFixture::start_with(IntakeLimits {
        max_frame_bytes: FRAME as u32,
        ..IntakeLimits::default()
    });
    let repo = fixture._tmp.path().join("ws_frame_limit");
    std::fs::write(repo.join("a"), "").expect("create shared task context file");
    let mut task_ids: Vec<String> = (0..3)
        .map(|index| {
            fixture
                .runtime
                .add_task(TaskAddParams {
                    title: format!("task context holder {index}"),
                    description: "lease holder fixture".to_string(),
                    context_files: vec![BLOCKED.to_string()],
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                })
                .expect("add task context holder")
                .id
                .to_string()
        })
        .collect();
    task_ids.sort();

    // The operator path remains unbounded and confirms all three tasks block
    // this selector. Its first conflict also sizes a budget that fits exactly
    // one conflict while leaving room for the runtime's omission witness.
    let unbounded = lease::request(
        &fixture.runtime,
        json!({
            "principal": "operator",
            "selectors": [BLOCKED],
            "mode": "hard",
            "wait": false,
        }),
        None,
        None,
    )
    .expect("operator request against task holders");
    assert_eq!(unbounded["granted"], false, "{unbounded}");
    assert_eq!(unbounded["queued"], false, "{unbounded}");
    assert_eq!(unbounded["truncated"], false, "{unbounded}");
    let unbounded_conflicts = unbounded["conflicts"].as_array().expect("conflicts");
    assert_eq!(unbounded_conflicts.len(), task_ids.len(), "{unbounded}");
    let unbounded_ids: Vec<_> = unbounded_conflicts
        .iter()
        .map(|conflict| {
            assert_eq!(conflict["file"], BLOCKED, "{conflict}");
            assert_eq!(conflict["held_by"], "task", "{conflict}");
            conflict["held_by_id"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(unbounded_ids, task_ids, "operator conflicts stay unbounded");

    let first_conflict_bytes = serde_json::to_vec(&unbounded_conflicts[0])
        .expect("serialize first task conflict")
        .len();
    let holder_slack = first_conflict_bytes + 1;
    let smallest_conflict_bytes = serde_json::to_vec(&json!({
        "file": "",
        "held_by": "task",
        "held_by_id": "",
    }))
    .expect("serialize smallest task conflict")
    .len()
        + 1;
    assert_eq!(
        holder_slack / smallest_conflict_bytes,
        1,
        "the reproduction requires a pre-bound of exactly one holder"
    );

    let mut client = fixture.connect();
    let grant = lease::LeaseTokenGrant {
        principal: "bob".to_string(),
        principal_class: lease::LeasePrincipalClass::Human,
        token_id: "tok-bob".to_string(),
        max_priority: lease::LeasePriority::Normal,
        hard_allowed: true,
        client_address: "192.0.2.7".to_string(),
        max_response_bytes: FRAME,
    };
    let request_probe = lease::request_with_token(
        &fixture.runtime,
        json!({
            "selectors": ["file:src/new.rs"],
            "mode": "hard",
            "tags": ["x".repeat(FRAME)],
        }),
        &grant,
    )
    .expect_err("the fixed request answer with a maximum tag exceeds the frame");
    let request_needed = preflight_needed_bytes(&request_probe.to_string());
    let request_tag_len = 2 * FRAME - holder_slack - request_needed;
    assert!((1..FRAME).contains(&request_tag_len), "{request_tag_len}");
    let request = ok(call(
        &mut client,
        "bob",
        LeaseIntakeOperation::Request,
        json!({
            "selectors": [BLOCKED],
            "mode": "hard",
            "tags": ["x".repeat(request_tag_len)],
        }),
    ));
    assert!(
        serde_json::to_vec(&LeaseIntakeResponse::Ok {
            result: request.clone(),
        })
        .expect("encode bounded request answer")
        .len()
            <= FRAME,
        "the request answer must fit the frame"
    );
    assert_eq!(request["granted"], false, "{request}");
    assert_eq!(request["queued"], true, "{request}");
    assert_eq!(request["truncated"], true, "{request}");
    let request_conflicts = request["conflicts"].as_array().expect("request conflicts");
    assert_eq!(request_conflicts.len(), 1, "{request}");
    assert_eq!(
        request_conflicts[0]["held_by_id"], task_ids[0],
        "the response preserves the first holder prefix"
    );
    let request_id = request["lease"]["lease_id"].as_str().unwrap();
    let queued_notice = |lease_id: &str| -> Value {
        let notices: Vec<_> = all_notices(&fixture.runtime)
            .into_iter()
            .filter(|notice| notice["lease_id"] == lease_id && notice["kind"] == "queued")
            .collect();
        assert_eq!(notices.len(), 1, "{notices:?}");
        notices[0].clone()
    };
    let request_notice = queued_notice(request_id);
    assert_eq!(request_notice["payload"]["holders"], request["conflicts"]);
    assert_eq!(
        request_notice["payload"]["truncated"], true,
        "{request_notice}"
    );
    lease::release(
        &fixture.runtime,
        json!({"lease_id": request_id}),
        None,
        None,
    )
    .expect("release queued request fixture");

    // Calibrate expansion independently because its fixed answer includes a
    // different set of fields and the existing lease record. The oversized
    // operator-created lease is only a fixture for the public token preflight;
    // that refusal occurs before it can change the lease.
    let expansion_probe = lease::request(
        &fixture.runtime,
        json!({
            "principal": "carol",
            "selectors": ["file:src/new.rs"],
            "mode": "hard",
            "tags": ["x".repeat(FRAME)],
        }),
        None,
        None,
    )
    .expect("create expansion budget probe");
    let expansion_probe_id = expansion_probe["lease"]["lease_id"].as_str().unwrap();
    let expansion_error = refusal(call(
        &mut client,
        "carol",
        LeaseIntakeOperation::Expand,
        json!({"lease_id": expansion_probe_id, "selectors": [BLOCKED]}),
    ));
    assert_eq!(expansion_error.code, "invalid_input", "{expansion_error:?}");
    let expansion_needed = preflight_needed_bytes(&expansion_error.message);
    let expansion_tag_len = 2 * FRAME - holder_slack - expansion_needed;
    assert!(
        (1..FRAME).contains(&expansion_tag_len),
        "{expansion_tag_len}"
    );
    lease::release(
        &fixture.runtime,
        json!({"lease_id": expansion_probe_id}),
        None,
        None,
    )
    .expect("release expansion budget probe");

    let expandable = lease::request(
        &fixture.runtime,
        json!({
            "principal": "carol",
            "selectors": ["file:src/new.rs"],
            "mode": "hard",
            "tags": ["x".repeat(expansion_tag_len)],
        }),
        None,
        None,
    )
    .expect("create lease for token expansion");
    let expandable_id = expandable["lease"]["lease_id"].as_str().unwrap();
    let expanded = ok(call(
        &mut client,
        "carol",
        LeaseIntakeOperation::Expand,
        json!({"lease_id": expandable_id, "selectors": [BLOCKED]}),
    ));
    assert!(
        serde_json::to_vec(&LeaseIntakeResponse::Ok {
            result: expanded.clone(),
        })
        .expect("encode bounded expansion answer")
        .len()
            <= FRAME,
        "the expansion answer must fit the frame"
    );
    assert_eq!(expanded["added"], json!([]), "{expanded}");
    assert_eq!(expanded["queued"], json!([BLOCKED]), "{expanded}");
    assert_eq!(expanded["truncated"], true, "{expanded}");
    let expansion_conflicts = expanded["conflicts"].as_array().expect("conflicts");
    assert_eq!(expansion_conflicts.len(), 1, "{expanded}");
    assert_eq!(
        expansion_conflicts[0]["held_by_id"], task_ids[0],
        "the expansion preserves the first holder prefix"
    );
    let expansion_notice = queued_notice(expandable_id);
    assert_eq!(
        expansion_notice["payload"]["holders"], expanded["conflicts"],
        "{expansion_notice}"
    );
    assert_eq!(
        expansion_notice["payload"]["truncated"], true,
        "{expansion_notice}"
    );
    let queued_lease = lease::show(&fixture.runtime, json!({"lease_id": expandable_id}))
        .expect("show expanded lease");
    assert_eq!(queued_lease["lease"]["queued_selectors"], json!([BLOCKED]));
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

fn lease_record(runtime: &OrbitRuntime, lease_id: &str) -> Value {
    lease::show(runtime, json!({"lease_id": lease_id})).expect("show lease")["lease"].clone()
}

fn preflight_needed_bytes(message: &str) -> usize {
    const MARKER: &str = "would need about ";
    let start = message
        .find(MARKER)
        .unwrap_or_else(|| panic!("expected a fixed-answer preflight refusal, got {message}"))
        + MARKER.len();
    let rest = &message[start..];
    let end = rest
        .find(" bytes")
        .unwrap_or_else(|| panic!("expected a byte count in {message}"));
    rest[..end]
        .parse()
        .unwrap_or_else(|_| panic!("expected a byte count in {message}"))
}

/// A token expansion that closes a multi-lease deadlock returns the whole
/// cycle when the answer fits. The same cycle, on a lease whose record
/// already fills the frame, is refused before that expansion writes a notice
/// or changes any queued surface. The fixed answer is sized with
/// `deadlock: null`; only the completed cycle pushes it over the frame.
#[test]
fn token_expansion_accounts_for_a_near_limit_deadlock_cycle() {
    if !enter_isolated_child(
        module_path!(),
        "token_expansion_accounts_for_a_near_limit_deadlock_cycle",
    ) {
        return;
    }
    const FRAME: usize = 16 * 1024;
    /// Room left under the frame after the fixed answer and the transport
    /// margin. A few bytes of lease-id difference from the probe fit here;
    /// a 32-member cycle does not.
    const SLACK: usize = 128;
    const CYCLE: usize = 32;
    let fixture = FrameLimitFixture::start_with(IntakeLimits {
        max_frame_bytes: FRAME as u32,
        ..IntakeLimits::default()
    });
    let repo = fixture._tmp.path().join("ws_frame_limit");
    let touch = |relative: &str| {
        let path = repo.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create selector directory");
        }
        std::fs::write(&path, "").expect("create selector file");
    };
    let request = |selector: &str, priority: &str, tag: Option<&str>| -> String {
        let mut input = json!({
            "principal": "alice",
            "selectors": [selector],
            "mode": "hard",
            "priority": priority,
        });
        if let Some(tag) = tag {
            input["tags"] = json!([tag]);
        }
        let granted = lease::request(&fixture.runtime, input, None, None).expect("seed hard lease");
        assert_eq!(granted["granted"], true, "{granted}");
        assert_eq!(granted["lease"]["mode"], "hard", "{granted}");
        granted["lease"]["lease_id"]
            .as_str()
            .expect("lease id")
            .to_string()
    };
    let expand_operator = |lease_id: &str, selector: &str| {
        lease::expand(
            &fixture.runtime,
            json!({"lease_id": lease_id, "selectors": [selector]}),
            None,
            None,
        )
        .expect("operator expand")
    };

    // A small cycle still returns the full deadlock and keeps the older waiter.
    for index in 0..3 {
        touch(&format!("fit/{index}.rs"));
    }
    let fit: Vec<String> = (0..3)
        .map(|index| {
            let priority = if index == 2 { "low" } else { "normal" };
            request(&format!("file:fit/{index}.rs"), priority, None)
        })
        .collect();
    expand_operator(&fit[0], "file:fit/1.rs");
    expand_operator(&fit[1], "file:fit/2.rs");
    let mut client = fixture.connect();
    let fitting = call(
        &mut client,
        "alice",
        LeaseIntakeOperation::Expand,
        json!({"lease_id": fit[2], "selectors": ["file:fit/0.rs"]}),
    );
    let fitting_size = serde_json::to_vec(&fitting).expect("encode").len();
    assert!(
        fitting_size <= FRAME,
        "a deadlock that fits must be delivered, not refused: {fitting_size} bytes"
    );
    let fitting = ok(fitting);
    assert_eq!(fitting["deadlock"]["lease_id"], fit[2], "{fitting}");
    let cycle = fitting["deadlock"]["cycle"]
        .as_array()
        .expect("deadlock cycle");
    assert_eq!(cycle.len(), 3, "{fitting}");
    for id in &fit {
        assert!(
            cycle.iter().any(|member| member == &json!(id)),
            "the cycle names every lease: {fitting}"
        );
    }
    assert_eq!(
        fitting["deadlock"]["selectors"],
        json!(["file:fit/0.rs"]),
        "{fitting}"
    );
    assert_eq!(fitting["lease"]["queued_selectors"], json!([]), "{fitting}");
    assert_eq!(
        fitting["lease"]["selectors"],
        json!(["file:fit/2.rs"]),
        "{fitting}"
    );
    assert_eq!(
        lease_record(&fixture.runtime, &fit[0])["queued_selectors"],
        json!(["file:fit/1.rs"]),
        "the older expansion keeps waiting"
    );
    let deadlock_notices: Vec<_> = all_notices(&fixture.runtime)
        .into_iter()
        .filter(|notice| notice["lease_id"] == fit[2] && notice["kind"] == "deadlock")
        .collect();
    assert_eq!(deadlock_notices.len(), 1, "{deadlock_notices:?}");
    assert_eq!(
        deadlock_notices[0]["payload"]["cycle"], fitting["deadlock"]["cycle"],
        "{deadlock_notices:?}"
    );
    assert_eq!(
        deadlock_notices[0]["payload"]["selectors"],
        json!(["file:fit/0.rs"]),
        "{deadlock_notices:?}"
    );

    // Size a victim tag from a fixed-answer refusal, then close a longer cycle
    // with that record. The probe is not part of the cycle.
    for index in 0..CYCLE {
        touch(&format!("near/{index:02}.rs"));
    }
    touch("near/pp.rs");
    let probe = request("file:near/pp.rs", "low", Some(&"x".repeat(FRAME)));
    let probe_error = refusal(call(
        &mut client,
        "alice",
        LeaseIntakeOperation::Expand,
        json!({"lease_id": probe, "selectors": ["file:near/00.rs"]}),
    ));
    assert_eq!(probe_error.code, "invalid_input", "{probe_error:?}");
    let needed = preflight_needed_bytes(&probe_error.message);
    let tag_len = (2 * FRAME)
        .checked_sub(SLACK + needed)
        .unwrap_or_else(|| panic!("probe needed {needed} bytes, over the frame"));
    assert!(
        (1..FRAME).contains(&tag_len),
        "tag length {tag_len} from a probe that needed {needed} bytes"
    );
    assert_eq!(
        lease_record(&fixture.runtime, &probe)["queued_selectors"],
        json!([]),
        "the sizing probe must not queue"
    );

    let holders: Vec<String> = (0..CYCLE - 1)
        .map(|index| request(&format!("file:near/{index:02}.rs"), "normal", None))
        .collect();
    let victim = request(
        &format!("file:near/{:02}.rs", CYCLE - 1),
        "low",
        Some(&"y".repeat(tag_len)),
    );
    for (index, holder) in holders.iter().enumerate() {
        let next = format!("file:near/{:02}.rs", index + 1);
        let expanded = expand_operator(holder, &next);
        assert_eq!(expanded["queued"], json!([next]), "{expanded}");
        assert!(expanded["deadlock"].is_null(), "{expanded}");
    }

    let watched = holders
        .iter()
        .chain(std::iter::once(&victim))
        .chain(std::iter::once(&probe));
    let records_before: Vec<Value> = watched
        .clone()
        .map(|id| lease_record(&fixture.runtime, id))
        .collect();
    let notices_before = all_notices(&fixture.runtime);
    let error = refusal(call(
        &mut client,
        "alice",
        LeaseIntakeOperation::Expand,
        json!({"lease_id": victim, "selectors": ["file:near/00.rs"]}),
    ));
    assert_eq!(error.code, "invalid_input", "{error:?}");
    assert!(
        error.message.contains("deadlock cycle"),
        "a near-limit cycle must be refused on the deadlock budget, not the fixed-answer \
         preflight or a frame error after the write: {error:?}"
    );
    let records_after: Vec<Value> = watched
        .map(|id| lease_record(&fixture.runtime, id))
        .collect();
    assert_eq!(
        records_after, records_before,
        "a refused expansion must not change the cycle's leases"
    );
    assert_eq!(
        all_notices(&fixture.runtime),
        notices_before,
        "a refused expansion must not write a queued or deadlock notice"
    );
}
