use crate::workspace::{
    DEFAULT_PUBLICATION_BRANCH, WORKSPACE_REGISTRY_SCHEMA_VERSION, Workspace,
    WorkspacePublicationBinding, WorkspaceRegistry, canonicalize_publication_branch,
    git_remote_identity, git_remotes_equivalent, redact_git_remote, validate_publication_branch,
    validate_publication_id, validate_publication_remote,
};
use chrono::{TimeZone, Utc};

fn binding() -> WorkspacePublicationBinding {
    WorkspacePublicationBinding {
        workspace_id: "ws_orbit".to_string(),
        source_repository_fingerprint: "git@github.com:example/source.git".to_string(),
        publication_remote: "git@github.com:example/tasks.git".to_string(),
        publication_branch: DEFAULT_PUBLICATION_BRANCH.to_string(),
        publication_id: "tp_orbit_tasks".to_string(),
        authority_machine_id: "hm_owner".to_string(),
        last_success_generation: Some(42),
        last_success_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    }
    .validated()
    .expect("valid binding")
}

#[test]
fn publication_binding_round_trips_all_lineage_fields() {
    let now = Utc
        .with_ymd_and_hms(2026, 8, 29, 1, 2, 3)
        .single()
        .expect("fixed timestamp");
    let registry = WorkspaceRegistry {
        workspaces: vec![Workspace {
            id: "ws_orbit".to_string(),
            name: "orbit".to_string(),
            owner_machine_id: Some("hm_owner".to_string()),
            git_remote: Some("git@github.com:example/source.git".to_string()),
            ship_mode: None,
            base_branch: "agent-main".to_string(),
            status: crate::workspace::WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        }],
        publication_bindings: vec![binding()],
        ..WorkspaceRegistry::default()
    };

    let value = serde_json::to_value(&registry).expect("serialize");
    assert_eq!(value["schema_version"], WORKSPACE_REGISTRY_SCHEMA_VERSION);
    let encoded = &value["publication_bindings"][0];
    assert_eq!(encoded["workspace_id"], "ws_orbit");
    assert_eq!(
        encoded["source_repository_fingerprint"],
        "git@github.com:example/source.git"
    );
    assert_eq!(
        encoded["publication_remote"],
        "git@github.com:example/tasks.git"
    );
    assert_eq!(encoded["publication_branch"], "refs/heads/main");
    assert_eq!(encoded["publication_id"], "tp_orbit_tasks");
    assert_eq!(encoded["authority_machine_id"], "hm_owner");
    assert_eq!(encoded["last_success_generation"], 42);
    assert_eq!(
        encoded["last_success_commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );

    let decoded: WorkspaceRegistry = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.publication_bindings, registry.publication_bindings);
}

#[test]
fn registry_without_publication_bindings_omits_the_field() {
    let registry = WorkspaceRegistry::default();
    let value = serde_json::to_value(&registry).expect("serialize");
    assert!(value.get("publication_bindings").is_none());

    let decoded: WorkspaceRegistry = serde_json::from_value(serde_json::json!({
        "schema_version": WORKSPACE_REGISTRY_SCHEMA_VERSION,
        "workspaces": [],
        "checkouts": []
    }))
    .expect("legacy document without publication_bindings");
    assert!(decoded.publication_bindings.is_empty());
}

#[test]
fn git_remote_identity_treats_ssh_and_https_forms_as_equal() {
    let identity = git_remote_identity("git@github.com:Org/Repo.git").expect("scp");
    assert_eq!(identity, "github.com/org/repo");
    assert!(
        git_remotes_equivalent(
            "git@github.com:Org/Repo.git",
            "https://github.com/Org/Repo.git"
        )
        .expect("compare")
    );
    assert!(
        git_remotes_equivalent(
            "ssh://git@github.com/Org/Repo",
            "https://github.com/Org/Repo/"
        )
        .expect("compare")
    );
}

#[test]
fn publication_remote_rejects_credentials_aliases_paths_and_source_equivalents() {
    let secret = "https://x-access-token:ghp_s3cret@github.com/example/tasks.git";
    let error = validate_publication_remote(secret)
        .expect_err("credential URL")
        .to_string();
    assert!(error.contains("must not contain credentials"), "{error}");
    assert!(!error.contains("ghp_s3cret"), "{error}");
    assert_eq!(
        redact_git_remote(secret),
        "https://***@github.com/example/tasks.git"
    );

    assert!(
        validate_publication_remote("/repos/orbit")
            .expect_err("path")
            .to_string()
            .contains("local checkout path")
    );
    assert!(
        validate_publication_remote("origin")
            .expect_err("alias")
            .to_string()
            .contains("checkout-local alias")
    );
    assert!(
        WorkspacePublicationBinding {
            workspace_id: "ws_orbit".to_string(),
            source_repository_fingerprint: "git@github.com:example/source.git".to_string(),
            publication_remote: "https://github.com/example/source.git".to_string(),
            publication_branch: "refs/heads/main".to_string(),
            publication_id: "tp_dup".to_string(),
            authority_machine_id: "hm_owner".to_string(),
            last_success_generation: None,
            last_success_commit: None,
        }
        .validated()
        .expect_err("source-equivalent remote")
        .to_string()
        .contains("equivalent to the workspace source remote")
    );
}

#[test]
fn malformed_credential_urls_are_rejected_with_redacted_diagnostics() {
    let cases = [
        (
            "https://review-user:invalid-host-secret@[invalid/tasks.git",
            "https://***@[invalid/tasks.git",
            "review-user",
            "invalid-host-secret",
        ),
        (
            "https://review-user:invalid-port-secret@github.com:not-a-port/tasks.git",
            "https://***@github.com:not-a-port/tasks.git",
            "review-user",
            "invalid-port-secret",
        ),
        (
            "ssh://deploy-user:invalid-host-secret@[invalid/tasks.git",
            "ssh://***@[invalid/tasks.git",
            "deploy-user",
            "invalid-host-secret",
        ),
        (
            "ssh://deploy-user:invalid-port-secret@github.com:not-a-port/tasks.git",
            "ssh://***@github.com:not-a-port/tasks.git",
            "deploy-user",
            "invalid-port-secret",
        ),
    ];

    for (remote, redacted, username, password) in cases {
        assert_eq!(redact_git_remote(remote), redacted);

        let diagnostic = validate_publication_remote(remote)
            .expect_err("malformed credential URL")
            .to_string();
        assert!(
            diagnostic.contains("is not a valid Git URL"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains(redacted), "{diagnostic}");
        assert!(!diagnostic.contains(username), "{diagnostic}");
        assert!(!diagnostic.contains(password), "{diagnostic}");
    }
}

#[test]
fn valid_ssh_credentials_are_rejected_and_credential_free_identity_is_preserved() {
    let credential_remote = "ssh://deploy-user:deploy-secret@github.com/example/tasks.git";
    let diagnostic = validate_publication_remote(credential_remote)
        .expect_err("credential URL")
        .to_string();
    assert!(
        diagnostic.contains("must not contain credentials"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("ssh://***@github.com/example/tasks.git"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("deploy-user"), "{diagnostic}");
    assert!(!diagnostic.contains("deploy-secret"), "{diagnostic}");
    assert_eq!(
        redact_git_remote(credential_remote),
        "ssh://***@github.com/example/tasks.git"
    );

    let credential_free = "ssh://github.com/example/tasks.git";
    assert!(validate_publication_remote(credential_free).is_ok());
    assert_eq!(
        git_remote_identity(credential_free).expect("credential-free identity"),
        "github.com/example/tasks"
    );
}

#[test]
fn publication_branch_and_id_reject_malformed_refs() {
    assert_eq!(
        canonicalize_publication_branch("main").expect("short name"),
        "refs/heads/main"
    );
    assert_eq!(
        canonicalize_publication_branch("").expect("default"),
        DEFAULT_PUBLICATION_BRANCH
    );
    assert!(
        validate_publication_branch("refs/tags/v1")
            .expect_err("tag")
            .to_string()
            .contains("ordinary refs/heads")
    );
    assert!(
        validate_publication_branch("refs/heads/feature..boom")
            .expect_err("dot-dot")
            .to_string()
            .contains("valid ordinary Git branch")
    );
    assert!(validate_publication_id("tp_orbit").is_ok());
    assert!(validate_publication_id("tp_path/id").is_err());
    assert!(validate_publication_id("").is_err());
}
