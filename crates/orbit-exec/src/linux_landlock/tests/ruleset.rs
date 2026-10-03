//! The ruleset descriptor and the descriptors the child inherits share one
//! number space, and the child rewrites its half of that space between `fork`
//! and `exec`.
//!
//! These drive the collision on purpose — the inherited target *is* the number
//! the ruleset was handed — so the contract is pinned by construction rather
//! than by which numbers happened to be free when the suite ran.

use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;

use super::*;
use crate::process::InheritedFd;
use crate::runner::{EnvironmentMode, ExecRequest, StdinMode};

#[test]
fn requested_rights_have_independent_abi_requirements() {
    let read_only = RulesetScope::default();
    assert!(read_only.require_abi(2).is_ok(), "ABI 2 can confine reads");

    let writes = RulesetScope {
        confine_writes: true,
        deny_tcp: false,
    };
    let error = writes
        .require_abi(2)
        .expect_err("ABI 2 cannot confine truncate");
    assert!(error.to_string().contains("truncate"), "{error}");
    assert!(error.to_string().contains("ABI 3"), "{error}");
    assert!(writes.require_abi(3).is_ok(), "ABI 3 can confine writes");

    let network = RulesetScope {
        confine_writes: false,
        deny_tcp: true,
    };
    let error = network.require_abi(3).expect_err("ABI 3 cannot deny TCP");
    assert!(error.to_string().contains("TCP"), "{error}");
    assert!(error.to_string().contains("ABI 4"), "{error}");
    assert!(network.require_abi(4).is_ok(), "ABI 4 can deny TCP");

    let both = RulesetScope {
        confine_writes: true,
        deny_tcp: true,
    };
    let error = both.require_abi(3).expect_err("ABI 3 cannot deny TCP");
    assert!(error.to_string().contains("ABI 4"), "{error}");
    assert!(both.require_abi(4).is_ok());
}

#[test]
fn plugin_boundary_rejects_abi_two_before_compiling_grants() {
    let probe = super::super::probe::LandlockProbeOutcome {
        available: true,
        abi: 2,
        detail: "Landlock ABI 2".to_string(),
    };
    let boundary = super::super::LandlockBoundary::default();
    let error = super::super::boundary::plugin_scope(&probe, &boundary)
        .expect_err("a plugin boundary must refuse missing truncate confinement");
    assert!(error.to_string().contains("truncate"), "{error}");
    assert!(error.to_string().contains("ABI 3"), "{error}");
}

#[test]
fn ruleset_creation_rechecks_write_capability_before_the_syscall() {
    let scope = RulesetScope {
        confine_writes: true,
        deny_tcp: false,
    };
    let result = Ruleset::create(scope, 2);
    assert!(matches!(result, Err(OrbitError::PolicyDenied(_))));
}

/// Read-only everywhere: enough for `/bin/sh` to start, and nothing else, so
/// a ruleset that stopped being applied shows up as a write that succeeds.
fn readable_root_ruleset() -> Ruleset {
    let ruleset = Ruleset::create(
        RulesetScope {
            confine_writes: true,
            deny_tcp: false,
        },
        abi_version(),
    )
    .expect("this host enforces Landlock; the Linux sandbox gate requires it");
    ruleset
        .add_path(&LandlockPathGrant {
            path: PathBuf::from("/"),
            grant: LandlockGrant::ReadTree,
        })
        .expect("read the whole filesystem");
    ruleset
}

/// A confined `/bin/sh` running `script`. The ruleset grants no write
/// anywhere, so the child cannot even open `/dev/null` to silence a
/// diagnostic; stderr is reported by the assertions instead of suppressed.
fn request(script: &str, env: Vec<(String, String)>) -> ExecRequest {
    ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        current_dir: None,
        timeout_ms: Some(10_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(env),
        debug: false,
    }
}

/// The child reports two things on one line: the first line of whatever is on
/// the callback number, and whether it could create a file under `DENIED`.
/// `dash` has no variable descriptor redirection, hence the `eval`, and `:` is
/// a special builtin whose redirection failure exits the shell, hence the
/// subshell.
const REPORT: &str = r#"
eval "read -r credential <&$FD" || credential=no-credential
if ( : > "$DENIED" ); then verdict=wrote; else verdict=denied; fi
echo "$credential $verdict"
"#;

/// A ruleset sitting on the number the child remaps used to be replaced by the
/// credential mid-`pre_exec`, and `landlock_restrict_self` refused the regular
/// file it found there with `EBADFD` — every plugin backend spawn failed. The
/// spawn must survive that collision, hand the credential over, *and* still
/// confine the child.
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_ruleset_on_a_remapped_number_still_confines_the_child_and_hands_over_the_credential() {
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join("record");
    let mut file = std::fs::File::create(&record).expect("create");
    file.write_all(b"session-token\n").expect("write");
    drop(file);
    let credential = OwnedFd::from(std::fs::File::open(&record).expect("open"));

    let ruleset = readable_root_ruleset();
    // The collision, by construction: hand the child the credential on the
    // exact number the ruleset is holding.
    let collision = ruleset.as_raw_fd();
    assert_ne!(
        collision,
        credential.as_raw_fd(),
        "two live descriptors cannot share a number"
    );
    let inherited = [InheritedFd {
        source: credential.as_raw_fd(),
        target: collision,
    }];

    let env = vec![
        ("FD".to_string(), collision.to_string()),
        (
            "DENIED".to_string(),
            dir.path().join("denied").display().to_string(),
        ),
    ];
    let child = spawn_with_ruleset(&request(REPORT, env), ruleset, &inherited)
        .expect("the ruleset moves clear of the remapped number instead of being overwritten");
    let output = child.wait_with_output().expect("collect the child");
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();

    assert_eq!(
        reported,
        "session-token denied",
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The relocation must not quietly drop confinement to make the spawn succeed:
/// with no inherited descriptor at all the same grants deny the same write.
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_spawn_that_inherits_nothing_is_confined_exactly_as_one_that_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = r#"if ( : > "$DENIED" ); then echo wrote; else echo denied; fi"#;
    let env = vec![(
        "DENIED".to_string(),
        dir.path().join("denied").display().to_string(),
    )];

    let child =
        spawn_with_ruleset(&request(script, env), readable_root_ruleset(), &[]).expect("spawn");
    let output = child.wait_with_output().expect("collect the child");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "denied",
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
