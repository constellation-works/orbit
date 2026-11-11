//! A timed-out push is confirmed once at the publication destination.
//!
//! The substitute `receivepack` applies the update, or leaves the ref as it
//! is, and then stalls. The supervision clock expires the production deadline
//! when that stall marker appears, so the check does not wait out the 60s
//! push timeout. The fixture repository is SHA-1: the wrapper reads the new
//! commit at a fixed offset in the receive-pack command.

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::test_env::{
    SupervisorSeam, canonical_temp_dir, with_supervision_clock, with_supervisor_seam,
};
use serde_json::{Value, json};

use super::super::{CANDIDATE_REF_PUSH, EVIDENCE_REF_PUSH};

const TARGET_REF: &str = "refs/orbit/evidence/ORB-23936";
const CHILD_ENV: &str = "ORBIT_PUSH_RETRY_TEST_CHILD";

#[test]
fn timed_out_evidence_push_lands_when_the_ref_names_the_commit() {
    isolated(
        "timed_out_evidence_push_lands_when_the_ref_names_the_commit",
        || {
            let fixture = PushFixture::open("landed");
            fixture.install_gh();
            let tree = empty_tree(&fixture.client);
            let value = with_stall_clock(&fixture.marker, || fixture.run_evidence(&tree))
                .unwrap_or_else(|error| panic!("landed push failed: {error}"));
            let attempted = fixture.attempted_sha();
            assert_eq!(
                value["commit_sha"].as_str(),
                Some(attempted.as_str()),
                "evidence ref did not return the pushed commit: {value}"
            );
            assert_eq!(value["reused"], json!(false));
            assert_eq!(
                value["repository_url"].as_str(),
                Some("https://example.test/owner/repo")
            );
            let tip = remote_tip(&fixture.bare, TARGET_REF);
            assert!(
                tip.starts_with(&attempted),
                "publication ref {tip:?} does not name {attempted}"
            );
            assert_eq!(fixture.push_count(), 1, "the landed push was sent again");
        },
    );
}

#[test]
fn timed_out_evidence_push_fails_when_the_ref_is_absent() {
    isolated(
        "timed_out_evidence_push_fails_when_the_ref_is_absent",
        || {
            let fixture = PushFixture::open("absent");
            let tree = empty_tree(&fixture.client);
            let error = with_stall_clock(&fixture.marker, || fixture.run_evidence(&tree))
                .expect_err("an absent ref was reported as landed");
            assert_timeout_failure(&fixture, &error, "confirmation absent");
            assert!(
                remote_tip(&fixture.bare, TARGET_REF).is_empty(),
                "the absent ref was published"
            );
        },
    );
}

#[test]
fn timed_out_evidence_push_fails_when_the_ref_names_another_sha() {
    isolated(
        "timed_out_evidence_push_fails_when_the_ref_names_another_sha",
        || {
            let fixture = PushFixture::open("other");
            let other = write_other_tip(&fixture);
            let tree = empty_tree(&fixture.client);
            let error = with_stall_clock(&fixture.marker, || fixture.run_evidence(&tree))
                .expect_err("a ref at another sha was reported as landed");
            assert_timeout_failure(
                &fixture,
                &error,
                &format!("confirmation at another sha {other}"),
            );
            let tip = remote_tip(&fixture.bare, TARGET_REF);
            assert!(
                tip.starts_with(&other),
                "the other sha moved during the refused push: {tip}"
            );
        },
    );
}

#[test]
fn timed_out_evidence_push_fails_when_confirmation_is_unreadable() {
    isolated(
        "timed_out_evidence_push_fails_when_confirmation_is_unreadable",
        || {
            let fixture = PushFixture::open("unreadable");
            let tree = empty_tree(&fixture.client);
            let error = with_stall_clock(&fixture.marker, || fixture.run_evidence(&tree))
                .expect_err("an unreadable confirmation was reported as landed");
            assert_timeout_failure(&fixture, &error, "confirmation unreadable");
            assert_eq!(
                fixture.push_count(),
                1,
                "an unreadable confirmation sent another push"
            );
        },
    );
}

#[test]
fn permanent_push_refusal_fails_without_confirmation_success() {
    isolated(
        "permanent_push_refusal_fails_without_confirmation_success",
        || {
            let fixture = PushFixture::open("permanent");
            let tree = empty_tree(&fixture.client);
            let error = fixture
                .run_evidence(&tree)
                .expect_err("a permanent refusal succeeded from the remote ref");
            let message = error.to_string();
            assert!(
                message.contains("non-fast-forward"),
                "permanent refusal lost its diagnostic: {message}"
            );
            assert!(
                !message.contains("confirmation absent")
                    && !message.contains("confirmation at another sha")
                    && !message.contains("confirmation unreadable"),
                "a permanent refusal was accepted or rejected by confirmation: {message}"
            );
            assert_eq!(fixture.push_count(), 1, "a permanent refusal was retried");
            let attempted = fixture.attempted_sha();
            let tip = remote_tip(&fixture.bare, TARGET_REF);
            assert!(
                tip.starts_with(&attempted),
                "the ref {tip:?} no longer names the pushed commit {attempted}, so this does not \
                 show that confirmation was ignored"
            );
        },
    );
}

#[test]
fn invalid_evidence_ref_and_head_are_refused_before_push() {
    isolated(
        "invalid_evidence_ref_and_head_are_refused_before_push",
        || {
            let fixture = PushFixture::open("absent");
            let tree = empty_tree(&fixture.client);
            let bad_tree = with_stall_clock(&fixture.marker, || {
                super::super::dispatch::run(
                    EVIDENCE_REF_PUSH,
                    &json!({
                        "repo_root": fixture.client,
                        "tree_sha": "not-a-tree",
                        "target_ref": TARGET_REF,
                        "message": "Screenshots for ORB-23936",
                        "forge_retry": forge_retry(),
                    }),
                )
            })
            .expect_err("an invalid tree sha was pushed");
            assert!(
                bad_tree
                    .to_string()
                    .contains("40- or 64-character tree_sha"),
                "{bad_tree}"
            );

            let bad_ref = with_stall_clock(&fixture.marker, || {
                super::super::dispatch::run(
                    EVIDENCE_REF_PUSH,
                    &json!({
                        "repo_root": fixture.client,
                        "tree_sha": tree,
                        "target_ref": "refs/heads/main",
                        "message": "Screenshots for ORB-23936",
                        "forge_retry": forge_retry(),
                    }),
                )
            })
            .expect_err("an invalid evidence ref was pushed");
            assert!(
                bad_ref
                    .to_string()
                    .contains("plain ref under 'refs/orbit/evidence/'"),
                "{bad_ref}"
            );

            let bad_head = with_stall_clock(&fixture.marker, || {
                super::super::dispatch::run(
                    CANDIDATE_REF_PUSH,
                    &json!({
                        "repo_root": fixture.client,
                        "head_sha": "abcd",
                        "target_ref": "refs/orbit/candidates/ORB-23936",
                        "forge_retry": forge_retry(),
                    }),
                )
            })
            .expect_err("an invalid head sha was pushed");
            assert!(
                bad_head
                    .to_string()
                    .contains("40- or 64-character head_sha"),
                "{bad_head}"
            );
            assert_eq!(
                fixture.push_count(),
                0,
                "invalid input reached the substitute remote"
            );
        },
    );
}

fn isolated(test_name: &str, body: impl FnOnce()) {
    let qualified = qualified_test(test_name);
    if std::env::var(CHILD_ENV).as_deref() != Ok(qualified.as_str()) {
        let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        child
            .args(["--exact", &qualified, "--nocapture"])
            .current_dir(root.path())
            .env(CHILD_ENV, &qualified)
            .env("HOME", root.path())
            .env("USERPROFILE", root.path());
        let output = orbit_common::test_env::run_child_test(&mut child, &qualified, root.path());
        orbit_common::test_env::assert_child_test_passed(
            &qualified,
            output.status,
            output.stdout,
            output.stderr,
        );
        return;
    }
    body();
}

fn qualified_test(test_name: &str) -> String {
    let module = module_path!().split_once("::").unwrap().1;
    format!("{module}::{test_name}")
}

fn with_stall_clock<T>(marker: &Path, body: impl FnOnce() -> T) -> T {
    let now = Rc::new(Cell::new(Instant::now()));
    let clock_now = Rc::clone(&now);
    let marker = marker.to_path_buf();
    let seam = SupervisorSeam {
        wait: Some(Box::new(move |child| {
            if marker.is_file() {
                let _ = fs::remove_file(&marker);
                now.set(now.get() + Duration::from_secs(61));
            }
            child.try_wait()
        })),
        ..SupervisorSeam::default()
    };
    with_supervision_clock(move || clock_now.get(), || with_supervisor_seam(seam, body))
}

fn assert_timeout_failure(fixture: &PushFixture, error: &OrbitError, outcome: &str) {
    let message = error.to_string();
    let attempted = fixture.attempted_sha();
    assert!(
        message.contains("evidence ref push"),
        "operation missing: {message}"
    );
    assert!(
        message.contains(TARGET_REF),
        "target ref missing: {message}"
    );
    assert!(
        message.contains(&attempted),
        "pushed commit missing: {message}"
    );
    assert!(
        message.contains("process timed out"),
        "timeout missing: {message}"
    );
    assert!(message.contains(outcome), "outcome missing: {message}");
    assert_eq!(fixture.push_count(), 1, "push count: {message}");
    let failure = error
        .private_vcs_failure()
        .unwrap_or_else(|| panic!("not a private VCS failure: {message}"));
    assert!(
        !failure.class.is_transient(),
        "unconfirmed timeout stayed retryable: {message}"
    );
}

fn forge_retry() -> Value {
    json!({
        "max_attempts": 3,
        "initial_backoff_ms": 0,
        "backoff_cap_ms": 0
    })
}

struct PushFixture {
    client: PathBuf,
    bare: PathBuf,
    pushes: PathBuf,
    attempted: PathBuf,
    marker: PathBuf,
}

impl PushFixture {
    fn open(mode: &str) -> Self {
        let root = std::env::current_dir().unwrap().join("fixture");
        let client = root.join("client");
        let bare = root.join("bare");
        fs::create_dir_all(&client).unwrap();
        git(&[
            "init",
            "-q",
            "--template=",
            "-b",
            "main",
            client.to_str().unwrap(),
        ]);
        git(&["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
        git(&[
            "-C",
            client.to_str().unwrap(),
            "remote",
            "add",
            "origin",
            bare.to_str().unwrap(),
        ]);
        let alternates = bare.join("objects/info/alternates");
        fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        fs::write(
            &alternates,
            format!("{}\n", client.join(".git/objects").display()),
        )
        .unwrap();
        let pushes = root.join("pushes");
        let attempted = root.join("attempted");
        let marker = root.join("stall");
        let fixture = Self {
            client,
            bare,
            pushes,
            attempted,
            marker,
        };
        fixture.install_receivepack(mode);
        fixture
    }

    fn install_receivepack(&self, mode: &str) {
        let wrapper = self.client.join("../receive-pack");
        let wrapper = wrapper
            .canonicalize()
            .unwrap_or_else(|_| self.bare.parent().unwrap().join("receive-pack"));
        let script = format!(
            r#"#!/bin/sh
unset GIT_DIR GIT_WORK_TREE GIT_PREFIX GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT
repo="$1"
printf '1\n' >> '{pushes}'
git receive-pack --advertise-refs "$repo" || exit 1
hdr=$(dd bs=4 count=1 2>/dev/null || true)
len=$((0x$hdr))
payload_len=$((len - 4))
dd bs="$payload_len" count=1 of='{pkt}' 2>/dev/null || true
dd if='{pkt}' bs=1 skip=41 count=40 of='{attempted}' 2>/dev/null || true
new=$(cat '{attempted}')
if [ "{mode}" = "landed" ] || [ "{mode}" = "unreadable" ] || [ "{mode}" = "permanent" ]; then
    git --git-dir="$repo" update-ref '{target_ref}' "$new" || exit 1
fi
if [ "{mode}" = "permanent" ]; then
    echo 'non-fast-forward' >&2
    echo 'protected branch hook declined' >&2
    exit 1
fi
if [ "{mode}" = "unreadable" ]; then
    mv "$repo" "$repo.unreadable"
    : > "$repo"
fi
: > '{marker}'
sleep 120
"#,
            pushes = self.pushes.display(),
            pkt = self.pushes.with_extension("pkt").display(),
            attempted = self.attempted.display(),
            mode = mode,
            target_ref = TARGET_REF,
            marker = self.marker.display(),
        );
        fs::write(&wrapper, script).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        git(&[
            "-C",
            self.client.to_str().unwrap(),
            "config",
            "remote.origin.receivepack",
            wrapper.to_str().unwrap(),
        ]);
    }

    fn install_gh(&self) {
        let bin = self.bare.parent().unwrap().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let gh = bin.join("gh");
        fs::write(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' '{\"url\":\"https://example.test/owner/repo\"}'\n",
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        // SAFETY: this runs inside the isolated child process, before any
        // other thread reads PATH. The parent keeps its own environment.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    }

    fn run_evidence(&self, tree: &str) -> Result<Value, OrbitError> {
        super::super::dispatch::run(
            EVIDENCE_REF_PUSH,
            &json!({
                "repo_root": self.client,
                "tree_sha": tree,
                "target_ref": TARGET_REF,
                "message": "Screenshots for ORB-23936",
                "forge_retry": forge_retry(),
            }),
        )
    }

    fn push_count(&self) -> usize {
        fs::read_to_string(&self.pushes)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .count()
    }

    fn attempted_sha(&self) -> String {
        fs::read_to_string(&self.attempted)
            .unwrap_or_else(|error| panic!("read attempted sha: {error}"))
            .trim()
            .to_string()
    }
}

fn write_other_tip(fixture: &PushFixture) -> String {
    let blob = git_stdin(
        &fixture.client,
        &["hash-object", "-w", "--stdin"],
        b"other\n",
    );
    let spec = format!("100644 blob {blob}\tother.txt\n");
    let tree = git_stdin(&fixture.client, &["mktree"], spec.as_bytes());
    let commit = git_in(
        &fixture.client,
        &[
            "-c",
            "user.name=Orbit",
            "-c",
            "user.email=orbit-evidence@noreply.invalid",
            "commit-tree",
            &tree,
            "-m",
            "other",
        ],
    );
    git(&[
        "--git-dir",
        fixture.bare.to_str().unwrap(),
        "update-ref",
        TARGET_REF,
        &commit,
    ]);
    commit
}

fn empty_tree(client: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(client)
        .arg("mktree")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "mktree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn remote_tip(bare: &Path, target_ref: &str) -> String {
    let output = Command::new("git")
        .args(["ls-remote", "--refs"])
        .arg(bare)
        .arg(target_ref)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ls-remote failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn git(args: &[&str]) {
    let output = Command::new("git").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_in(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git -C {} {args:?} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn git_stdin(repo: &Path, args: &[&str], stdin: &[u8]) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git -C {} {args:?} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
