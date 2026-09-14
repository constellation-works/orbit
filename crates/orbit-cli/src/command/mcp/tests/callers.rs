//! Argument surface and seed inputs for `orbit mcp callers`.

use clap::Parser;

use super::*;

/// Minimal parser so this subcommand's arguments can be exercised without the
/// whole CLI tree.
#[derive(Parser)]
struct CallersCli {
    #[command(subcommand)]
    command: CallersSubcommand,
}

fn parse(argv: &[&str]) -> CallersSubcommand {
    CallersCli::parse_from(argv).command
}

#[test]
fn check_takes_the_machine_id_positionally() {
    match parse(&["callers", "check", "hm_alpha"]) {
        CallersSubcommand::Check(args) => assert_eq!(args.machine_id, "hm_alpha"),
        _ => panic!("expected `check`"),
    }
}

#[test]
fn check_requires_a_machine_id() {
    assert!(
        CallersCli::try_parse_from(["callers", "check"]).is_err(),
        "checking nothing would print the file default and read as an answer about a caller"
    );
}

#[test]
fn init_takes_no_grant_writing_flags() {
    // Granting operator is a hand edit by design; a flag for it here would
    // re-create the caller-authored grant the file replaces [ORB-11052].
    for argv in [
        ["callers", "init", "--operator"].as_slice(),
        ["callers", "init", "--grant", "operator"].as_slice(),
    ] {
        assert!(
            CallersCli::try_parse_from(argv).is_err(),
            "unexpected grant-writing flag accepted: {argv:?}"
        );
    }
}

#[test]
fn a_workspace_root_override_is_refused() {
    let args = CallersArgs {
        command: parse(&["callers", "list"]),
    };

    let error = args
        .execute_without_runtime(Some(Path::new("/tmp")))
        .expect_err("the callers file is machine-global");

    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");
}

#[test]
fn callers_list_redacts_identity_and_key_material_but_keeps_policy_shape() {
    let file = orbit_mcp::CallersFile {
        default: DefaultGrant::Deny,
        callers: vec![orbit_mcp::CallerRow {
            machine_id: "hm_sensitive_machine".to_string(),
            capabilities: vec!["agent".to_string(), "operator".to_string()],
            label: Some("sensitive host label".to_string()),
            workspaces: Some(vec!["ws_sensitive".to_string()]),
            ssh_key_fingerprint: Some("SHA256:sensitive-fingerprint".to_string()),
            agent_invoke: true,
            agent_invoke_workspaces: Some(vec!["ws_sensitive".to_string()]),
            agent_invoke_mode: Some(RemoteAgentInvokeMode::KeyBound),
        }],
    };

    let rendered = render_callers_list(Path::new("/tmp/mcp-callers.toml"), &file);

    assert!(rendered.contains("caller #1: [identity redacted]"));
    assert!(rendered.contains("capabilities: agent=true, operator=true, denied=false"));
    assert!(rendered.contains("workspaces: scoped (IDs redacted)"));
    assert!(rendered.contains("ssh_key_fingerprint: configured (redacted)"));
    assert!(rendered.contains("agent_invoke: enabled (key-bound)"));
    assert!(!rendered.contains("hm_sensitive_machine"));
    assert!(!rendered.contains("sensitive host label"));
    assert!(!rendered.contains("ws_sensitive"));
    assert!(!rendered.contains("SHA256:sensitive-fingerprint"));
}

#[test]
fn seeding_reads_registry_owners_and_configured_destinations() {
    let root = tempfile::tempdir().expect("global root");
    orbit_registry::ensure_host_identity(root.path(), || {
        Ok(orbit_registry::NewHostIdentity {
            host_id: "this-host".to_string(),
            task_prefix: "TH".to_string(),
        })
    })
    .expect("host identity");
    let local_machine_id =
        match orbit_registry::inspect_host_identity(root.path()).expect("host identity state") {
            orbit_registry::HostIdentityState::Present(identity) => identity.machine_id,
            other => panic!("expected a present host identity, got {other:?}"),
        };
    std::fs::write(
        root.path().join("mcp-destinations.toml"),
        "[[destinations]]\nssh = \"alpha-box\"\nmachine_id = \"hm_alpha\"\n",
    )
    .expect("write destinations");

    let seeded = known_callers(root.path()).expect("seed inputs");

    let machine_ids: Vec<_> = seeded
        .iter()
        .map(|caller| caller.machine_id.as_str())
        .collect();
    assert!(machine_ids.contains(&"hm_alpha"));
    assert!(
        !machine_ids.contains(&local_machine_id.as_str()),
        "a row for this machine would authorize nothing it does not already resolve locally"
    );
    assert_eq!(
        seeded
            .iter()
            .find(|caller| caller.machine_id == "hm_alpha")
            .and_then(|caller| caller.label.as_deref()),
        Some("alpha-box"),
        "the configured SSH target is the operator-facing name already written down"
    );
}

#[test]
fn authorize_names_the_caller_key_and_protected_launcher() {
    match parse(&[
        "callers",
        "authorize",
        "--machine-id",
        "hm_alpha",
        "--key",
        "/home/op/.ssh/id_ed25519.pub",
        "--launcher",
        "/usr/local/libexec/orbit-mcp-ssh",
    ]) {
        CallersSubcommand::Authorize(args) => {
            assert_eq!(args.machine_id, "hm_alpha");
            assert_eq!(args.key, Path::new("/home/op/.ssh/id_ed25519.pub"));
            assert_eq!(args.launcher, Path::new("/usr/local/libexec/orbit-mcp-ssh"));
        }
        _ => panic!("expected `authorize`"),
    }
}

#[test]
fn authorize_needs_a_key_and_protected_launcher() {
    // Without a key there is nothing to bind, and a line with only a machine
    // ID would read as a grant while authenticating nobody [ORB-11053].
    for argv in [
        ["callers", "authorize", "--machine-id", "hm_alpha"].as_slice(),
        [
            "callers",
            "authorize",
            "--machine-id",
            "hm_alpha",
            "--key",
            "/tmp/k.pub",
        ]
        .as_slice(),
        [
            "callers",
            "authorize",
            "--machine-id",
            "hm_alpha",
            "--launcher",
            "/usr/local/libexec/orbit-mcp-ssh",
        ]
        .as_slice(),
        ["callers", "authorize"].as_slice(),
    ] {
        assert!(
            CallersCli::try_parse_from(argv).is_err(),
            "unexpected partial authorize accepted: {argv:?}"
        );
    }
}

#[test]
fn authorize_refuses_a_machine_id_that_is_not_one() {
    let key = tempfile::NamedTempFile::new().expect("temp key");
    std::fs::write(
        key.path(),
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINMX3zk7E9dEvV0tMWx6b+FKAWBcQiweXKgUOc0AqkKH op@box\n",
    )
    .expect("write key");
    let args = CallersAuthorizeArgs {
        machine_id: "daniels-mac-mini".to_string(),
        key: key.path().to_path_buf(),
        launcher: PathBuf::from("/usr/local/libexec/orbit-mcp-ssh"),
    };

    let error = authorize(
        Path::new("/nonexistent"),
        Path::new("/nonexistent/mcp-callers.toml"),
        &args,
    )
    .expect_err("a row is keyed by machine_id, so a host label cannot select one");

    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");
}
