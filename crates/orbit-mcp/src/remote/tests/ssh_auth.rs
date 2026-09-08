use base64::Engine as _;

use super::super::ssh_auth::{
    FORCED_COMMAND_RESTRICTIONS, KeyObservation, ObservedKeys, SSH_ACCEPTANCE_ENV, SshAcceptance,
    auth_info_fingerprints, fingerprint_defect, issue_ssh_acceptance, parse_public_key,
    read_auth_info, verify_ssh_acceptance,
};

/// A real `ssh-keygen -t ed25519` key, kept beside the fingerprint
/// `ssh-keygen -l` printed for it. The pair is the whole point of the fixture:
/// an operator compares Orbit's output with their own tools, so a fingerprint
/// Orbit merely computes self-consistently would be worthless.
const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINMX3zk7E9dEvV0tMWx6b+FKAWBcQiweXKgUOc0AqkKH caller@daniels-mac-mini";
const KEY_FINGERPRINT: &str = "SHA256:5HTlLtSRdZg7lKPho8slfRr2Q1QTPuko05+KRX/8PQw";

#[test]
fn a_fingerprint_is_the_one_ssh_keygen_prints() {
    let key = parse_public_key(KEY).expect("a public key parses");

    assert_eq!(key.algorithm, "ssh-ed25519");
    assert_eq!(key.comment.as_deref(), Some("caller@daniels-mac-mini"));
    assert_eq!(
        key.fingerprint().expect("fingerprint"),
        KEY_FINGERPRINT,
        "Orbit's fingerprint must equal `ssh-keygen -l -f <key>.pub`, or an operator cannot \
         transcribe one into the other"
    );
}

#[test]
fn a_comment_and_trailing_whitespace_do_not_change_the_key() {
    let bare = parse_public_key(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINMX3zk7E9dEvV0tMWx6b+FKAWBcQiweXKgUOc0AqkKH\n",
    )
    .expect("a comment-less key parses");

    assert_eq!(bare.comment, None);
    assert_eq!(bare.fingerprint().expect("fingerprint"), KEY_FINGERPRINT);
}

#[test]
fn the_rendered_line_forces_this_machines_own_argv() {
    let key = parse_public_key(KEY).expect("a public key parses");

    let line = key.authorized_keys_line(
        "/usr/local/libexec/orbit-mcp-ssh",
        "hm_alpha",
        ".orbit-ssh-destination-capability",
    );

    assert!(
        line.starts_with(
            "environment=\"ORBIT_MCP_SSH_ACCEPTANCE=.orbit-ssh-destination-capability\",\
             command=\"/usr/local/libexec/orbit-mcp-ssh mcp serve --accept-ssh --caller hm_alpha \
             --operator\","
        ),
        "the destination composes the whole operator request, absolute path included: {line}"
    );
    assert_eq!(SSH_ACCEPTANCE_ENV, "ORBIT_MCP_SSH_ACCEPTANCE");
    assert!(
        !line
            .split_once("command=\"")
            .map_or(line.as_str(), |(_, command)| command)
            .contains(".orbit-ssh-destination-capability"),
        "the acceptance bearer must not appear in the forced command argv: {line}"
    );
    assert!(line.contains(FORCED_COMMAND_RESTRICTIONS), "{line}");
    assert!(line.contains("no-pty"), "{line}");
    assert!(
        line.ends_with(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINMX3zk7E9dEvV0tMWx6b+FKAWBcQiweXKgUOc0AqkKH \
             caller@daniels-mac-mini"
        ),
        "{line}"
    );
    assert!(line.contains("--operator"), "{line}");
}

#[test]
fn a_private_key_or_an_options_line_is_refused() {
    for contents in [
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n",
        // An authorized_keys line, options first: composing a new grant out of
        // an old one must not silently drop the old one's restrictions.
        &format!("command=\"/bin/false\",no-pty {KEY}"),
        "",
        "# only a comment\n",
        "ssh-ed25519\n",
    ] {
        assert!(
            parse_public_key(contents).is_err(),
            "must refuse: {contents}"
        );
    }
}

#[test]
fn only_a_sha256_fingerprint_may_pin_a_row() {
    assert_eq!(fingerprint_defect(KEY_FINGERPRINT), None);
    assert_eq!(
        fingerprint_defect(&format!("{KEY_FINGERPRINT}=")),
        None,
        "padding an operator's paste may carry is not a defect"
    );

    // The plausible mistake: `ssh-keygen -E md5` prints this shape.
    assert!(fingerprint_defect("MD5:ab:cd:ef").is_some());
    assert!(fingerprint_defect("5HTlLtSRdZg7lKPho8slfRr2Q1QTPuko05+KRX/8PQw").is_some());
    assert!(fingerprint_defect("SHA256:not base64 at all!").is_some());
    assert!(
        fingerprint_defect("SHA256:c2hvcnQ").is_some(),
        "a digest that is not 32 bytes cannot be a SHA-256 one"
    );
}

#[test]
fn a_pin_matches_regardless_of_padding_or_prefix_transcription() {
    let observed = ObservedKeys {
        fingerprints: vec![KEY_FINGERPRINT.to_string()],
        observation: KeyObservation::AuthInfoFile,
    };

    assert!(observed.matches(KEY_FINGERPRINT));
    assert!(observed.matches(&format!("{KEY_FINGERPRINT}=")));
    assert!(observed.matches(&format!("  {KEY_FINGERPRINT}  ")));
    assert!(
        !observed.matches("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        "a different key is a different key"
    );
}

#[test]
fn only_a_destination_issued_capability_recovers_the_bound_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token = issue_ssh_acceptance(dir.path(), "hm_alpha", KEY_FINGERPRINT).expect("issue token");
    let observed = verify_ssh_acceptance(dir.path(), "hm_alpha", &token).expect("verify token");
    let record =
        std::fs::read_to_string(dir.path().join("mcp-ssh-acceptance").join("hm_alpha.toml"))
            .expect("read verifier record");

    assert_eq!(observed.observation, KeyObservation::DestinationCapability);
    assert!(observed.matches(KEY_FINGERPRINT));
    assert!(
        !record.contains(&token),
        "only the bearer digest may be persisted"
    );
    assert!(
        verify_ssh_acceptance(dir.path(), "hm_alpha", "caller-controlled").is_err(),
        "argv alone must not mint a destination capability"
    );
}

/// [ORB-11065] The capability's whole job is to be unguessable by a caller who
/// can already run an ordinary remote command on this destination. That caller
/// knows roughly when `orbit mcp callers authorize` ran and what the process
/// looked like, so a token derived from a clock- or thread-id-seeded PRNG —
/// which is what a temporary-file name generator gives you — is enumerable
/// offline. Nothing here consults a clock, so every token in this sample is
/// issued from the same logical instant: what distinguishes them can only be
/// the operating system's CSPRNG.
#[test]
fn an_acceptance_token_is_minted_from_operating_system_entropy() {
    const SAMPLE: usize = 64;
    let dir = tempfile::tempdir().expect("tempdir");

    let tokens: Vec<String> = (0..SAMPLE)
        .map(|_| {
            issue_ssh_acceptance(dir.path(), "hm_alpha", KEY_FINGERPRINT).expect("issue token")
        })
        .collect();

    let entropy: Vec<Vec<u8>> = tokens
        .iter()
        .map(|token| {
            let body = token
                .strip_prefix(".orbit-ssh-")
                .unwrap_or_else(|| panic!("a capability names itself: {token}"));
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(body.as_bytes())
                .unwrap_or_else(|error| panic!("token body is not base64: {token}: {error}"))
        })
        .collect();

    for (token, bytes) in tokens.iter().zip(&entropy) {
        assert_eq!(
            bytes.len() * 8,
            256,
            "a bearer capability must carry at least 128 bits of entropy: {token}"
        );
    }
    let distinct: std::collections::BTreeSet<&Vec<u8>> = entropy.iter().collect();
    assert_eq!(
        distinct.len(),
        SAMPLE,
        "tokens issued back to back from one process repeated, so they are not random"
    );
    // A counter, a filename sequence, or any value derived from a fixed seed
    // pins some byte position; a CSPRNG pins none. With 64 draws the odds of a
    // genuinely random position staying constant are 256^-63.
    for position in 0..entropy[0].len() {
        assert!(
            entropy
                .iter()
                .any(|bytes| bytes[position] != entropy[0][position]),
            "byte {position} never varied across {SAMPLE} tokens, so it is not drawn from entropy"
        );
    }
}

#[test]
fn sshds_auth_info_yields_the_keys_that_authenticated() {
    let fingerprints = auth_info_fingerprints(
        "publickey ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINMX3zk7E9dEvV0tMWx6b+FKAWBcQiweXKgUOc0AqkKH\n",
    );

    assert_eq!(fingerprints, vec![KEY_FINGERPRINT.to_string()]);
}

#[test]
fn a_session_that_authenticated_without_a_key_has_no_key_to_pin() {
    assert!(auth_info_fingerprints("password\n").is_empty());
    assert!(auth_info_fingerprints("").is_empty());
    assert!(
        auth_info_fingerprints("publickey ssh-ed25519 not-base64!\n").is_empty(),
        "an unparseable blob is not a key this destination observed"
    );
}

/// [ORB-11712] A readable auth-info file is an observation whatever it says.
/// A password login is a session this destination positively saw authenticate
/// without a key, and reporting it as unobservable would let a caller who knows
/// the account password be served a pinned row's grant.
#[test]
fn an_auth_info_file_naming_no_key_is_still_an_observation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("auth-info");
    std::fs::write(&path, "password\n").expect("write auth info");

    let observed = read_auth_info(&path).expect("a readable auth-info file is an observation");

    assert!(observed.fingerprints.is_empty());
    assert_eq!(observed.observation, KeyObservation::AuthInfoFile);
}

/// A file sshd already cleaned up says nothing either way, and refusing on it
/// would refuse sessions no policy meant to refuse.
#[test]
fn an_unreadable_auth_info_file_is_not_an_observation() {
    let dir = tempfile::tempdir().expect("tempdir");

    assert!(read_auth_info(&dir.path().join("absent")).is_none());
}

#[test]
fn an_ordinary_serve_cannot_name_a_caller() {
    // The rule "`--caller` is honored only under `--accept-ssh`" is carried by
    // the type: there is no Environment variant that can hold an identity, so
    // no downstream check can forget it.
    let acceptance = SshAcceptance::default();

    assert_eq!(acceptance, SshAcceptance::Environment);
}
