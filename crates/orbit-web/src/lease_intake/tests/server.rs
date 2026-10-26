use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use orbit_auth::scope::{
    HardLeasePermission, LeasePriorityCeiling, LeaseTokenScope, LeaseTokenVerb, PrincipalClass,
};
use orbit_core::OrbitRuntime;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use serde_json::{Value, json};

use super::super::{
    IntakeLimits, LeaseIntakeAuthority, LeaseIntakeClient, LeaseIntakeError, LeaseIntakeOperation,
    LeaseIntakeRequest, LeaseIntakeResponse, LeaseIntakeSocket,
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
