use orbit_common::OrbitError;

use super::super::config::{
    Destination, MachineQualifiedSelector, destinations_path, federated_membership,
    load_destinations,
};

#[test]
fn machine_qualified_selector_accepts_only_machine_and_workspace_ids() {
    let selector: MachineQualifiedSelector = "hm_alpha-1/ws_orbit"
        .parse()
        .expect("host-qualified selector");

    assert_eq!(selector.machine_id(), "hm_alpha-1");
    assert_eq!(selector.workspace_id(), "ws_orbit");
    assert_eq!(selector.to_string(), "hm_alpha-1/ws_orbit");
}

#[test]
fn machine_qualified_selector_rejects_bare_workspace_id() {
    assert!(matches!(
        "ws_orbit".parse::<MachineQualifiedSelector>(),
        Err(OrbitError::UnknownSelector(_))
    ));
}

#[test]
fn machine_qualified_selector_rejects_display_machine_name() {
    assert!(matches!(
        "orbit-linux/ws_orbit".parse::<MachineQualifiedSelector>(),
        Err(OrbitError::UnknownSelector(_))
    ));
}

#[test]
fn machine_qualified_selector_rejects_paths() {
    for token in [
        "/srv/orbit/ws_orbit",
        "hm_alpha/../ws_orbit",
        "hm_alpha/C:\\orbit",
    ] {
        assert!(
            matches!(
                token.parse::<MachineQualifiedSelector>(),
                Err(OrbitError::UnknownSelector(_))
            ),
            "unexpectedly accepted {token}"
        );
    }
}

#[test]
fn distinct_machine_destinations_load() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = destinations_path(root.path());
    std::fs::write(
        &path,
        r#"
[[destinations]]
ssh = "orbit-a"
machine_id = "hm_alpha"

[[destinations]]
ssh = "operator@orbit-b"
machine_id = "hm_beta"
"#,
    )
    .expect("write destinations");

    let loaded = load_destinations(&path).expect("load destinations");
    assert_eq!(loaded.destinations.len(), 2);
    assert_eq!(loaded.destinations[0].ssh, "orbit-a");
    assert_eq!(loaded.destinations[0].machine_id, "hm_alpha");
    assert_eq!(loaded.destinations[1].machine_id, "hm_beta");
}

#[test]
fn duplicate_machine_destinations_fail_at_load() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = destinations_path(root.path());
    std::fs::write(
        &path,
        r#"
[[destinations]]
ssh = "orbit-a"
machine_id = "hm_duplicate"

[[destinations]]
ssh = "orbit-b"
machine_id = "hm_duplicate"
"#,
    )
    .expect("write destinations");

    let error = load_destinations(&path).expect_err("duplicate must fail at config load");
    assert!(matches!(error, OrbitError::AmbiguousDestination(_)));
}

#[test]
fn destinations_require_machine_id() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = destinations_path(root.path());
    std::fs::write(
        &path,
        r#"
[[destinations]]
ssh = "orbit-a"
"#,
    )
    .expect("write destinations");

    let error = load_destinations(&path).expect_err("machine_id is required");
    assert!(matches!(error, OrbitError::InvalidInput(_)));
}

#[test]
fn a_missing_destinations_file_is_an_empty_remote_list() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = destinations_path(root.path());

    let loaded = load_destinations(&path).expect("missing file is local-only");
    assert!(loaded.destinations.is_empty());
}

#[test]
fn an_empty_destinations_list_is_valid() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = destinations_path(root.path());
    std::fs::write(&path, "destinations = []\n").expect("write empty destinations");

    let loaded = load_destinations(&path).expect("empty list is local-only");
    assert!(loaded.destinations.is_empty());
}

#[test]
fn destinations_require_ssh() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = destinations_path(root.path());
    std::fs::write(
        &path,
        r#"
[[destinations]]
machine_id = "hm_alpha"
"#,
    )
    .expect("write destinations");

    let error = load_destinations(&path).expect_err("ssh is required on remote rows");
    assert!(matches!(error, OrbitError::InvalidInput(_)));
    assert!(
        error.to_string().contains("ssh") || error.to_string().contains("missing"),
        "the error must name the missing ssh field: {error}"
    );
}

#[test]
fn a_blank_ssh_target_fails_closed() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = destinations_path(root.path());
    std::fs::write(
        &path,
        r#"
[[destinations]]
ssh = "   "
machine_id = "hm_alpha"
"#,
    )
    .expect("write destinations");

    let error = load_destinations(&path).expect_err("blank ssh is invalid");
    assert!(matches!(error, OrbitError::InvalidInput(_)));
    assert!(
        error.to_string().contains("blank ssh"),
        "the error must be actionable: {error}"
    );
}

#[test]
fn membership_prepends_local_and_keeps_distinct_remotes() {
    let remotes = super::super::config::DestinationsFile {
        destinations: vec![super::super::config::RemoteDestination {
            ssh: "orbit-remote".to_string(),
            machine_id: "hm_remote".to_string(),
        }],
    };

    let destinations = federated_membership("hm_local", "local-host", remotes);

    assert_eq!(
        destinations,
        vec![
            Destination::local("hm_local", "local-host"),
            Destination::ssh("orbit-remote", "hm_remote"),
        ]
    );
}

#[test]
fn membership_collapses_an_explicit_row_for_the_local_machine() {
    let remotes = super::super::config::DestinationsFile {
        destinations: vec![
            super::super::config::RemoteDestination {
                ssh: "localhost".to_string(),
                machine_id: "hm_local".to_string(),
            },
            super::super::config::RemoteDestination {
                ssh: "orbit-remote".to_string(),
                machine_id: "hm_remote".to_string(),
            },
        ],
    };

    let destinations = federated_membership("hm_local", "local-host", remotes);

    assert_eq!(
        destinations
            .iter()
            .map(|destination| destination.machine_id.as_str())
            .collect::<Vec<_>>(),
        ["hm_local", "hm_remote"]
    );
    assert!(destinations[0].is_local());
    assert_eq!(destinations[0].machine_name_display(), "local-host");
    assert_eq!(destinations[1].ssh_target(), Some("orbit-remote"));
}

#[test]
fn empty_remotes_yield_a_local_only_membership() {
    let destinations = federated_membership(
        "hm_local",
        "local-host",
        super::super::config::DestinationsFile {
            destinations: Vec::new(),
        },
    );

    assert_eq!(
        destinations,
        vec![Destination::local("hm_local", "local-host")]
    );
}
