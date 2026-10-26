use std::sync::Arc;
use std::time::Duration;

use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenScope, LeaseTokenVerb, PrincipalClass,
};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use super::super::{
    IntakeLimits, LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeError, LeaseIntakeOperation,
    LeaseIntakeRequest, LeaseIntakeResponse, LeaseIntakeSocket,
};
use crate::api::tests::test_support::enter_isolated_child;
use crate::state::{DashboardState, SINGLE_WORKSPACE_ID};

fn call(
    client: &mut LeaseIntakeClient,
    principal: &str,
    operation: LeaseIntakeOperation,
    input: Value,
) -> LeaseIntakeResponse {
    let request = LeaseIntakeRequest {
        operation,
        workspace: SINGLE_WORKSPACE_ID.to_string(),
        authority: LeaseIntakeAuthority {
            token_id: format!("tok-{principal}"),
            scope: LeaseTokenScope {
                principal: principal.to_string(),
                principal_class: PrincipalClass::Human,
                workspaces: vec![SINGLE_WORKSPACE_ID.to_string()],
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
