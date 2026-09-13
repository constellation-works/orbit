use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, RemoteAgentInvokeMode, ToolSessionContext};

use orbit_types::tool::CallerIdentityProof;

use super::super::callers::{
    CallersFile, DefaultGrant, RemoteCallerIdentity, SeedCaller, SessionCapabilityPolicy,
    inspect_caller_authorization, load_callers, render_callers_seed, write_callers_seed,
};
use super::super::identity::McpSessionAuthority;
use super::super::ssh_auth::{KeyObservation, ObservedKeys};

fn agent() -> BTreeSet<McpCapability> {
    BTreeSet::from([McpCapability::Agent])
}

fn operator() -> BTreeSet<McpCapability> {
    BTreeSet::from([McpCapability::Agent, McpCapability::Operator])
}

/// A caller that named itself — the Tier 1 identity every existing case here
/// is about.
fn caller(machine_id: &str) -> RemoteCallerIdentity {
    RemoteCallerIdentity::self_asserted(machine_id)
}

/// The key `ssh-keygen` printed this fingerprint for; the pair is checked in
/// the `ssh_auth` tests, so here it only has to be a consistent one.
const PINNED: &str = "SHA256:5HTlLtSRdZg7lKPho8slfRr2Q1QTPuko05+KRX/8PQw";
const OTHER_KEY: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn observed(fingerprint: &str) -> Option<ObservedKeys> {
    Some(ObservedKeys {
        fingerprints: vec![fingerprint.to_string()],
        observation: KeyObservation::AuthInfoFile,
    })
}

/// Write a callers file the way an operator must hold it: private to the
/// account that serves sessions from it. `std::fs::write` alone would land at
/// the ambient umask, which `load_callers` refuses [ORB-12450] — the same
/// refusal the permission cases below assert.
fn write(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("mcp-callers.toml");
    std::fs::write(&path, contents).expect("write callers");
    chmod(&path, 0o600);
    (dir, path)
}

fn chmod(path: &std::path::Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

#[test]
fn a_missing_file_is_a_valid_agent_only_configuration() {
    let dir = tempfile::tempdir().expect("temp dir");

    let file = load_callers(&dir.path().join("mcp-callers.toml")).expect("missing file loads");

    assert_eq!(file, CallersFile::default());
    assert_eq!(file.default, DefaultGrant::Agent);
    assert_eq!(file.resolve(&caller("hm_alpha")).granted, agent());
}

#[test]
fn a_duplicate_machine_id_fails_the_whole_file_closed() {
    let (_dir, path) = write(
        r#"
default = "agent"

[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]

[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
"#,
    );

    let error = load_callers(&path).expect_err("duplicate machine_id must fail closed");

    assert!(
        matches!(error, OrbitError::AmbiguousCaller(ref message) if message.contains("hm_alpha")),
        "expected ambiguous_caller, got {error:?}"
    );
}

#[test]
fn runner_is_not_a_grantable_capability() {
    let (_dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "runner"]
"#,
    );

    let error = load_callers(&path).expect_err("runner must not be grantable over a transport");

    assert!(
        matches!(error, OrbitError::InvalidInput(ref message) if message.contains("runner")),
        "expected an invalid-input refusal naming runner, got {error:?}"
    );
}

#[test]
fn a_caller_can_be_denied_with_an_explicit_or_empty_capability_list() {
    for capabilities in [r#"["deny"]"#, "[]"] {
        let (_dir, path) = write(&format!(
            r#"
default = "agent"

[[callers]]
machine_id = "hm_denied"
capabilities = {capabilities}
"#
        ));
        let file = load_callers(&path).expect("a denied row must parse");
        let grant = file.resolve(&caller("hm_denied"));

        assert!(grant.matched);
        assert!(grant.granted.is_empty());
        assert!(
            SessionCapabilityPolicy::from_grant(McpSessionAuthority::Operator, grant)
                .effective_for(None)
                .is_empty(),
            "a denied row must override the agent default"
        );
    }
}

#[test]
fn deny_cannot_be_combined_with_a_grant_capability() {
    let (_dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_denied"
capabilities = ["deny", "agent"]
"#,
    );

    let error = load_callers(&path).expect_err("mixed deny and grant must be rejected");

    assert!(error.to_string().contains("deny"), "{error}");
}

#[test]
fn a_malformed_file_is_never_served_as_if_absent() {
    for contents in [
        // Unknown key.
        "[[callers]]\nmachine_id = \"hm_alpha\"\ncapabilities = [\"agent\"]\nallow = true\n",
        // Unknown default.
        "default = \"operator\"\n",
        // Malformed machine_id.
        "[[callers]]\nmachine_id = \"not a machine\"\ncapabilities = [\"agent\"]\n",
        // Narrowing that is not a logical workspace ID.
        "[[callers]]\nmachine_id = \"hm_alpha\"\ncapabilities = [\"agent\"]\nworkspaces = [\"orbit\"]\n",
    ] {
        let (_dir, path) = write(contents);

        assert!(
            load_callers(&path).is_err(),
            "a malformed file must fail closed: {contents}"
        );
    }
}

#[test]
fn the_file_can_only_lower_what_argv_asked_for() {
    let (_dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
"#,
    );
    let file = load_callers(&path).expect("callers");

    let agent_request = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Agent,
        file.resolve(&caller("hm_alpha")),
    );

    assert_eq!(
        agent_request.effective_for(None),
        agent(),
        "a caller granted operator that did not ask for it must still resolve to agent"
    );
}

#[test]
fn an_over_asking_caller_is_capped_by_the_destination() {
    let (_dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]
"#,
    );
    let file = load_callers(&path).expect("callers");

    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&caller("hm_alpha")),
    );

    assert_eq!(policy.effective_for(None), agent());
}

#[test]
fn an_unmatched_caller_falls_to_the_default_not_to_argv() {
    let (_dir, path) = write(
        r#"
default = "deny"

[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
"#,
    );
    let file = load_callers(&path).expect("callers");

    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&caller("hm_beta")),
    );

    assert!(
        policy.effective_for(None).is_empty(),
        "an unmatched caller under `default = deny` holds nothing"
    );
}

#[test]
fn a_workspaces_narrowing_is_evaluated_per_call() {
    let (_dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_beta"
capabilities = ["agent", "operator"]
workspaces = ["ws_orbit"]
"#,
    );
    let file = load_callers(&path).expect("callers");
    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&caller("hm_beta")),
    );

    assert_eq!(policy.effective_for(Some("ws_orbit")), operator());
    assert_eq!(
        policy.effective_for(Some("ws_other")),
        agent(),
        "the same session holds only agent outside the workspaces its row lists"
    );
}

#[test]
fn a_narrowed_row_still_denies_elsewhere_under_a_deny_default() {
    let (_dir, path) = write(
        r#"
default = "deny"

[[callers]]
machine_id = "hm_beta"
capabilities = ["agent", "operator"]
workspaces = ["ws_orbit"]
"#,
    );
    let file = load_callers(&path).expect("callers");
    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&caller("hm_beta")),
    );

    assert!(
        policy.effective_for(Some("ws_other")).is_empty(),
        "narrowing falls back to the file default, which a deny default must not raise"
    );
}

#[test]
fn agent_invoke_is_an_explicit_key_bound_workspace_grant() {
    let (_dir, path) = write(&format!(
        r#"
default = "deny"

[[callers]]
machine_id = "hm_beta"
capabilities = ["agent", "operator"]
workspaces = ["ws_orbit"]
ssh_key_fingerprint = "{PINNED}"
agent_invoke = true
"#,
    ));
    let file = load_callers(&path).expect("scoped remote invocation grant");
    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&RemoteCallerIdentity::key_bound(
            "hm_beta",
            observed(PINNED),
        )),
    );

    let on_workspace = policy
        .grant_for(Some("ws_orbit"))
        .expect("remote grant on workspace");
    assert!(on_workspace.agent_invoke);
    assert_eq!(on_workspace.identity, CallerIdentityProof::KeyBound);
    assert_eq!(
        on_workspace.agent_invoke_mode,
        Some(RemoteAgentInvokeMode::KeyBound),
        "omitting the new mode must preserve the original strict behavior"
    );

    let elsewhere = policy
        .grant_for(Some("ws_other"))
        .expect("remote grant outside narrowing");
    assert!(!elsewhere.agent_invoke);
    assert!(policy.effective_for(Some("ws_other")).is_empty());
}

#[test]
fn cooperative_agent_invoke_is_explicit_and_workspace_scoped() {
    let (_dir, path) = write(
        r#"
default = "deny"

[[callers]]
machine_id = "hm_beta"
capabilities = ["agent", "operator"]
workspaces = ["ws_orbit"]
agent_invoke = true
agent_invoke_mode = "cooperative"
"#,
    );
    let file = load_callers(&path).expect("cooperative invocation grant");
    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&caller("hm_beta")),
    );

    let on_workspace = policy.grant_for(Some("ws_orbit")).expect("remote grant");
    assert_eq!(on_workspace.identity, CallerIdentityProof::SelfAsserted);
    assert_eq!(
        on_workspace.agent_invoke_mode,
        Some(RemoteAgentInvokeMode::Cooperative)
    );

    let elsewhere = policy.grant_for(Some("ws_other")).expect("remote grant");
    assert!(!elsewhere.agent_invoke);
    assert_eq!(elsewhere.agent_invoke_mode, None);
    assert!(policy.effective_for(Some("ws_other")).is_empty());
}

#[test]
fn invocation_workspace_scope_does_not_narrow_ordinary_operator_access() {
    let (_dir, path) = write(
        r#"
default = "deny"

[[callers]]
machine_id = "hm_beta"
capabilities = ["agent", "operator"]
agent_invoke = true
agent_invoke_mode = "cooperative"
agent_invoke_workspaces = ["ws_orbit"]
"#,
    );
    let file = load_callers(&path).expect("independent invocation scope");
    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&caller("hm_beta")),
    );

    assert_eq!(policy.effective_for(Some("ws_orbit")), operator());
    assert_eq!(policy.effective_for(Some("ws_other")), operator());
    assert!(
        policy
            .grant_for(Some("ws_orbit"))
            .expect("remote grant")
            .agent_invoke
    );
    assert!(
        !policy
            .grant_for(Some("ws_other"))
            .expect("remote grant")
            .agent_invoke
    );
}

#[test]
fn incomplete_agent_invoke_grants_fail_the_callers_file_closed() {
    for (contents, expected) in [
        (
            format!(
                r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]
workspaces = ["ws_orbit"]
ssh_key_fingerprint = "{PINNED}"
agent_invoke = true
"#,
            ),
            "operator",
        ),
        (
            format!(
                r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{PINNED}"
agent_invoke = true
"#,
            ),
            "workspaces",
        ),
        (
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
agent_invoke = true
agent_invoke_mode = "cooperative"
agent_invoke_workspaces = []
"#
            .to_string(),
            "agent_invoke_workspaces",
        ),
        (
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
agent_invoke_workspaces = ["ws_orbit"]
"#
            .to_string(),
            "without enabling `agent_invoke`",
        ),
        (
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
agent_invoke = true
agent_invoke_mode = "cooperative"
agent_invoke_workspaces = ["orbit"]
"#
            .to_string(),
            "agent_invoke_workspaces",
        ),
        (
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
agent_invoke_workspcaes = ["ws_orbit"]
"#
            .to_string(),
            "unknown field",
        ),
        (
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
workspaces = ["ws_orbit"]
agent_invoke = true
"#
            .to_string(),
            "ssh_key_fingerprint",
        ),
        (
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
workspaces = ["ws_orbit"]
agent_invoke_mode = "cooperative"
"#
            .to_string(),
            "without enabling `agent_invoke`",
        ),
        (
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
workspaces = ["ws_orbit"]
agent_invoke = true
agent_invoke_mode = "invented"
"#
            .to_string(),
            "unknown variant",
        ),
    ] {
        let (_dir, path) = write(&contents);
        let error = load_callers(&path).expect_err("incomplete grant must fail closed");

        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn a_local_session_keeps_argv_authority_and_stamps_no_grant() {
    let policy = SessionCapabilityPolicy::local(McpSessionAuthority::Operator);
    let mut context = ToolSessionContext::default();

    policy.stamp(&mut context, Some("ws_orbit"));

    assert!(!policy.is_granted());
    assert_eq!(context.effective_capabilities, operator());
    assert_eq!(
        context.remote_caller_grant, None,
        "a local session has no destination-side statement to record"
    );
}

#[test]
fn a_granted_session_records_the_grant_beside_the_effective_set() {
    let (_dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]
"#,
    );
    let file = load_callers(&path).expect("callers");
    let policy = SessionCapabilityPolicy::from_grant(
        McpSessionAuthority::Operator,
        file.resolve(&caller("hm_alpha")),
    );
    let mut context = ToolSessionContext::default();

    policy.stamp(&mut context, None);

    let grant = context
        .remote_caller_grant
        .expect("a remote-originated session records its grant");
    assert_eq!(grant.caller_machine_id, "hm_alpha");
    assert_eq!(grant.granted_capabilities, agent());
    assert_eq!(context.effective_capabilities, agent());
    assert!(grant.source.contains("mcp-callers.toml"));
}

#[test]
fn the_seeder_never_writes_an_operator_grant() {
    let seeded = render_callers_seed(&[
        SeedCaller {
            machine_id: "hm_alpha".to_string(),
            label: Some("daniels-mac-mini".to_string()),
        },
        SeedCaller {
            machine_id: "hm_beta".to_string(),
            label: None,
        },
    ]);

    assert!(!seeded.contains("capabilities = [\"operator\"]"));
    assert!(seeded.contains("machine_id   = \"hm_alpha\""));
    assert!(seeded.contains("label        = \"daniels-mac-mini\""));
    assert!(seeded.contains("capabilities = [\"agent\"]"));
}

#[test]
fn a_seeded_file_loads_and_grants_agent_only() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("mcp-callers.toml");
    let seeded = render_callers_seed(&[SeedCaller {
        machine_id: "hm_alpha".to_string(),
        label: None,
    }]);

    write_callers_seed(&path, &seeded).expect("seed writes");
    let file = load_callers(&path).expect("a seeded file must be valid");

    assert_eq!(file.resolve(&caller("hm_alpha")).granted, agent());
    assert!(
        write_callers_seed(&path, &seeded).is_err(),
        "re-seeding must not overwrite an operator's statement"
    );
}

/// [ORB-11053] The pin is enforced where the key is observable, and a mismatch
/// is a refusal rather than a downgrade: a caller presenting somebody else's
/// key must not be quietly served at the file default, which would look
/// exactly like a caller that legitimately holds a smaller grant.
#[test]
fn a_key_mismatch_refuses_the_session_instead_of_lowering_it() {
    let (dir, _path) = write(&format!(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{PINNED}"
"#
    ));
    let identity = RemoteCallerIdentity::key_bound("hm_alpha", observed(OTHER_KEY));

    let error =
        SessionCapabilityPolicy::resolve(dir.path(), McpSessionAuthority::Operator, &identity)
            .expect_err("a key mismatch must refuse at session establishment");

    assert!(
        matches!(error, OrbitError::UnauthorizedCaller(ref message) if message.contains("hm_alpha")),
        "expected an unauthorized-caller refusal naming the caller, got {error:?}"
    );
}

/// [ORB-11053] The key that the row names is served, and the trail records
/// that the identity was proved rather than claimed.
#[test]
fn a_matching_key_is_served_and_recorded_as_key_bound() {
    let (dir, _path) = write(&format!(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{PINNED}"
"#
    ));
    let identity = RemoteCallerIdentity::key_bound("hm_alpha", observed(PINNED));

    let policy =
        SessionCapabilityPolicy::resolve(dir.path(), McpSessionAuthority::Operator, &identity)
            .expect("a matching key is served");
    let mut context = ToolSessionContext::default();
    policy.stamp(&mut context, None);

    assert_eq!(policy.effective_for(None), operator());
    let grant = context.remote_caller_grant.expect("a grant is recorded");
    assert_eq!(grant.caller_machine_id, "hm_alpha");
    assert_eq!(
        grant.identity,
        CallerIdentityProof::KeyBound,
        "a Tier 1 and a Tier 2 destination produce identical grants; only this field \
         distinguishes them in the trail"
    );
}

/// [ORB-11712] Observing that no key authenticated is evidence, not the absence
/// of it. A pinned row exists so a key mismatch is refused, and a password or
/// keyboard-interactive login under that row is the plainest mismatch it can
/// have.
#[test]
fn a_session_that_authenticated_without_a_key_refuses_a_pinned_row() {
    let (dir, _path) = write(&format!(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{PINNED}"
"#
    ));
    let identity = RemoteCallerIdentity::key_bound(
        "hm_alpha",
        Some(ObservedKeys {
            fingerprints: Vec::new(),
            observation: KeyObservation::AuthInfoFile,
        }),
    );

    let error =
        SessionCapabilityPolicy::resolve(dir.path(), McpSessionAuthority::Operator, &identity)
            .expect_err("a pinned row must not be served to a session that used no key");

    assert!(
        matches!(
            error,
            OrbitError::UnauthorizedCaller(ref message)
                if message.contains("hm_alpha")
                    && message.contains("authenticated without a public key")
        ),
        "expected a refusal naming the caller and the missing key, got {error:?}"
    );
}

/// [ORB-11053] Verification being unavailable is not a mismatch. `ExposeAuthInfo`
/// is off in a stock sshd, and refusing every pinned caller there would make
/// the field unusable for the destinations most likely to set it.
#[test]
fn an_unobservable_key_serves_the_session_rather_than_refusing_it() {
    let (dir, _path) = write(&format!(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{PINNED}"
"#
    ));
    let identity = RemoteCallerIdentity::key_bound("hm_alpha", None);

    let policy =
        SessionCapabilityPolicy::resolve(dir.path(), McpSessionAuthority::Operator, &identity)
            .expect("an unobservable key is not evidence of a mismatch");

    assert_eq!(policy.effective_for(None), operator());
}

/// [ORB-11053] A pin is enforced under either tier. The operator wrote the
/// fingerprint to have it checked, and a Tier 1 destination that happens to
/// expose auth info can check it just as well — what Tier 2 adds is that the
/// identity itself stops being the caller's to choose.
#[test]
fn a_pin_is_enforced_even_when_the_identity_was_self_asserted() {
    let (dir, _path) = write(&format!(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]
ssh_key_fingerprint = "{PINNED}"
"#
    ));
    let identity = caller("hm_alpha").observing(observed(OTHER_KEY));

    assert!(
        SessionCapabilityPolicy::resolve(dir.path(), McpSessionAuthority::Agent, &identity)
            .is_err()
    );
}

/// [ORB-11053] A fingerprint in the wrong format would never match, so it
/// would present as a key mismatch on every session. Fail the file closed at
/// load instead, where the message can name the row.
#[test]
fn a_fingerprint_that_is_not_sha256_fails_the_file_closed() {
    let (_dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]
ssh_key_fingerprint = "MD5:ab:cd:ef"
"#,
    );

    let error = load_callers(&path).expect_err("an MD5 fingerprint cannot pin a row");

    assert!(
        matches!(error, OrbitError::InvalidInput(ref message) if message.contains("hm_alpha")),
        "expected an invalid-input refusal naming the row, got {error:?}"
    );
}

/// [ORB-11053] What `orbit doctor` reads. The two gaps are separate: nothing
/// declared on a machine that serves SSH, and the strongest grant the file can
/// make resting on a name.
#[test]
fn the_doctor_sees_an_unpinned_operator_grant_and_an_undeclared_destination() {
    let (dir, _path) = write(&format!(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]

[[callers]]
machine_id = "hm_beta"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "{PINNED}"

[[callers]]
machine_id = "hm_gamma"
capabilities = ["agent"]
"#
    ));
    let authorized_keys = dir.path().join("authorized_keys");
    std::fs::write(&authorized_keys, "# no keys, only a comment\n").expect("write");

    let health = inspect_caller_authorization(dir.path(), &authorized_keys);

    assert!(health.present);
    assert_eq!(health.row_count, 3);
    assert_eq!(
        health.unpinned_operator_callers,
        vec!["hm_alpha".to_string()],
        "an agent-only row needs no key, and a pinned operator row already has one"
    );
    assert!(
        !health.serves_ssh,
        "a commented-out authorized_keys admits nobody, so there is no gap to report"
    );

    std::fs::write(&authorized_keys, "ssh-ed25519 AAAA nobody@nowhere\n").expect("write");
    let serving = inspect_caller_authorization(&dir.path().join("elsewhere"), &authorized_keys);

    assert!(serving.serves_ssh);
    assert!(
        !serving.present,
        "a machine that accepts SSH with no callers file is the Tier 1 gap the doctor reports"
    );
}

/// A label is a peer's operator-chosen host id or an SSH destination; a
/// quote in it must be escaped, or the seed is unparseable and every remote
/// session is refused until someone hand-edits the file.
#[test]
fn seed_escapes_labels_that_would_break_the_toml_literal() {
    let seeded = render_callers_seed(&[SeedCaller {
        machine_id: "hm_0123456789abcdef".to_string(),
        label: Some("laptop\" # not a comment".to_string()),
    }]);
    assert!(
        seeded.contains("label        = \"laptop\\\" # not a comment\""),
        "{seeded}"
    );
    let parsed: toml::Value = toml::from_str(&seeded).expect("seed parses as TOML");
    assert_eq!(
        parsed["callers"][0]["label"].as_str(),
        Some("laptop\" # not a comment")
    );
}

/// [ORB-12450] A ceiling anyone else can write is not a ceiling: a principal
/// with no capability of its own would append a row granting itself `operator`
/// and `agent_invoke`. The refusal is total — the session is not served from
/// the file at a lowered grant, because a lowered grant would look exactly
/// like a legitimately small one.
///
/// The owner half of the same check (`uid != geteuid`) cannot be exercised
/// here: creating a file owned by another account needs privileges a unit test
/// must not have.
#[cfg(unix)]
#[test]
fn a_writable_ceiling_is_refused_and_serves_no_session() {
    for (mode, scope) in [(0o664, "group"), (0o666, "world"), (0o622, "world")] {
        let (dir, path) = write(
            r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent", "operator"]
"#,
        );
        chmod(&path, mode);

        let error = load_callers(&path).expect_err("a writable ceiling must fail closed");
        let message = error.to_string();
        assert!(message.contains(scope), "{mode:o}: {message}");
        assert!(
            message.contains("chmod 600 ~/.orbit/mcp-callers.toml"),
            "the denial must name the fix: {message}"
        );
        assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");

        let refused = SessionCapabilityPolicy::resolve(
            dir.path(),
            McpSessionAuthority::Operator,
            &caller("hm_alpha"),
        )
        .expect_err("no session may be established from an untrusted ceiling");
        assert!(
            refused.to_string().contains("mcp-callers.toml"),
            "{refused}"
        );
    }
}

/// [ORB-12450] Read access is a disclosure, not an escalation, so it is
/// reported rather than refused: taking every remote session down over the
/// mode the conventional `umask 022` produces would trade an outage for a
/// leak. Nothing on the destination needs the group bit — the account owns the
/// file and reads it as itself even under the setgid Tier 2 launcher, which
/// drops its launch group before Orbit opens any state.
#[cfg(unix)]
#[test]
fn a_readable_ceiling_still_loads_and_is_reported_instead() {
    let (dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]
"#,
    );
    let authorized_keys = dir.path().join("authorized_keys");
    std::fs::write(&authorized_keys, "ssh-ed25519 AAAA nobody@nowhere\n").expect("write");

    for mode in [0o644, 0o640] {
        chmod(&path, mode);
        let file = load_callers(&path).expect("a readable ceiling is not a refusal");
        assert_eq!(file.resolve(&caller("hm_alpha")).granted, agent());

        let health = inspect_caller_authorization(dir.path(), &authorized_keys);
        assert!(health.defect.is_none(), "{health:?}");
        assert!(
            health.readable_beyond_owner,
            "the doctor must see mode {mode:o}"
        );
    }

    chmod(&path, 0o600);
    let health = inspect_caller_authorization(dir.path(), &authorized_keys);
    assert!(
        !health.readable_beyond_owner,
        "a private ceiling has nothing to report: {health:?}"
    );
}

/// [ORB-12450] A symlink's own mode says nothing about the file it points at,
/// so the target cannot be trusted by checking the link. Refusing names the
/// reason instead of reporting a bare `ELOOP` from the no-follow open.
#[cfg(unix)]
#[test]
fn a_symlinked_ceiling_is_refused_rather_than_followed() {
    let (dir, path) = write(
        r#"
[[callers]]
machine_id = "hm_alpha"
capabilities = ["agent"]
"#,
    );
    let link = dir.path().join("linked-callers.toml");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");

    let error = load_callers(&link).expect_err("a symlinked ceiling must fail closed");

    assert!(error.to_string().contains("symlink"), "{error}");
}

/// [ORB-12450] The seeder used to write at the ambient umask, so on a host
/// with the conventional `umask 002` `orbit mcp callers init` produced a
/// group-writable authorization file — which `load_callers` now refuses,
/// making the seeder's own output unusable if it were still umask-dependent.
/// The child process proves the mode is the kernel-set one, not an artifact of
/// whatever umask the test runner happens to have.
#[cfg(unix)]
#[test]
fn the_seeder_writes_a_private_file_under_a_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_CALLERS_SEED_UMASK";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let status = std::process::Command::new("sh")
            .args(["-c", "umask 000; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("current test executable"))
            .arg("the_seeder_writes_a_private_file_under_a_permissive_umask")
            .env(CHILD_MARKER, "1")
            .status()
            .expect("run test under permissive umask");
        assert!(status.success(), "permissive-umask child failed");
        return;
    }

    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("nested/mcp-callers.toml");
    let seeded = render_callers_seed(&[SeedCaller {
        machine_id: "hm_alpha".to_string(),
        label: None,
    }]);

    write_callers_seed(&path, &seeded).expect("seed writes");

    let mode = std::fs::metadata(&path)
        .expect("seed metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "seeded mode {mode:o}");
    let parent_mode = std::fs::metadata(path.parent().expect("callers parent"))
        .expect("callers parent metadata")
        .permissions()
        .mode();
    assert_eq!(parent_mode & 0o777, 0o700, "parent mode {parent_mode:o}");
    load_callers(&path).expect("the seeder's own output must satisfy the trust check");
}
