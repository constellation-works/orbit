//! Pure helpers of the claimed-leaf handoff steps. Execution-level behavior is
//! covered by the owner-local claimed fixture in orbit-core.
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use crate::context::{ClaimExecutionContext, RuntimeHost};
use orbit_common::OrbitError;
use orbit_types::workflow::handoff::{HandoffDelivery, TaskHandoff};
use serde_json::json;
use tempfile::tempdir;

use super::super::claim::{
    MAX_CAPTURED_OUTPUT_BYTES, capture, claim_handoff, claim_validate, delivery, observe_candidate,
    pull_request_number, slug,
};

fn claim_context(ship_mode: &str) -> ClaimExecutionContext {
    ClaimExecutionContext {
        workspace_id: "ws".into(),
        task_id: "T1".into(),
        claim_id: "c1".into(),
        machine_id: "m1".into(),
        run_id: "r1".into(),
        ship_mode: ship_mode.into(),
        base_branch: "agent-main".into(),
        landing_branch: "agent-main".into(),
        required_commands: vec!["true".into()],
    }
}

#[test]
fn remote_urls_reduce_to_owner_and_name_and_a_bare_name_has_none() {
    assert_eq!(
        slug("git@github.com:owner/repo.git").as_deref(),
        Some("owner/repo")
    );
    assert_eq!(
        slug("https://github.com/owner/repo").as_deref(),
        Some("owner/repo")
    );
    assert_eq!(slug("/srv/git/bare.git/").as_deref(), Some("git/bare"));
    assert_eq!(slug("repo"), None);
}

#[test]
fn truncated_capture_reports_that_it_was_truncated() {
    let long = "x".repeat(MAX_CAPTURED_OUTPUT_BYTES + 10);
    let captured = capture(&long, "");
    assert!(captured.starts_with("[truncated to"));
    assert!(captured.len() < long.len() + 64);
}

#[test]
fn capture_joins_both_streams_without_padding_an_empty_one() {
    assert_eq!(capture("out\n", "err\n"), "out\nerr");
    assert_eq!(capture("out\n", "  "), "out");
    assert_eq!(capture("", ""), "");
}

/// [ORB-12617] `pr_open` publishes its number as a string and an exact
/// step-output template forwards that type unchanged, so a pr-mode claim
/// receives `"4242"`, not `4242`. Both shapes name the same pull request.
#[test]
fn a_pull_request_number_is_read_from_either_shape_the_run_can_produce() {
    use serde_json::json;
    assert_eq!(pull_request_number(&json!(4242)), Some(4242));
    assert_eq!(pull_request_number(&json!("4242")), Some(4242));
    assert_eq!(pull_request_number(&json!(" 4242 ")), Some(4242));
    assert_eq!(pull_request_number(&json!("")), None);
    assert_eq!(pull_request_number(&json!("pr-4242")), None);
    assert_eq!(pull_request_number(&json!(null)), None);
}

/// [ORB-12640] `pr_open` emits `"pr_number": "2345"` (a JSON string). The
/// claimed PR pipeline forwards that value into `pull_request` unchanged, so
/// `delivery()` must build `HandoffDelivery::PullRequest` from a string.
#[test]
fn pr_mode_delivery_accepts_the_string_pr_open_emits() {
    let context = claim_context("pr");
    assert_eq!(
        delivery(&context, &json!({"pull_request": "2345"})).expect("string PR number"),
        HandoffDelivery::PullRequest { number: 2345 }
    );
    assert_eq!(
        delivery(&context, &json!({"pull_request": 2345})).expect("integer PR number"),
        HandoffDelivery::PullRequest { number: 2345 }
    );
    let missing = delivery(&context, &json!({}))
        .expect_err("a pr-mode claim without a number is missing, not local");
    assert!(
        missing.to_string().contains("pull_request is required"),
        "{missing}"
    );
}

/// Owner-local claims never read `pull_request`, including when a string is
/// present by accident.
#[test]
fn local_mode_delivery_ignores_pull_request() {
    let context = claim_context("local");
    assert_eq!(
        delivery(&context, &json!({"pull_request": "2345"})).expect("local"),
        HandoffDelivery::LocalCandidate
    );
    assert_eq!(
        delivery(&context, &json!({})).expect("local without the field"),
        HandoffDelivery::LocalCandidate
    );
}

/// [ORB-12642] Remote-sync observation must read `origin/<base>`, not a
/// lagging local `refs/heads/<base>`. Before the fix this recorded the local
/// tip, so a claimed PR leaf refused its own `sync_base` checkpoint.
#[test]
fn remote_sync_observation_reads_origin_base_when_local_lags() {
    let fixture = LaggingBaseFixture::new();
    let observed = observe_candidate(
        &fixture.local,
        Some("candidate"),
        "agent-main",
        "agent-main",
        HandoffDelivery::PullRequest { number: 1 },
        "ws",
        "remote",
    )
    .expect("remote-sync observation");
    assert_eq!(observed.base.commit, fixture.remote_tip);
    assert_ne!(observed.base.commit, fixture.local_tip);
    assert_eq!(observed.base_branch, "agent-main");
    assert_eq!(observed.candidate.commit, fixture.candidate_tip);
}

/// [ORB-12642] Owner-local observation keeps reading the local ref even when
/// a remote-tracking ref is ahead of it.
#[test]
fn local_sync_observation_keeps_the_local_base_when_origin_is_ahead() {
    let fixture = LaggingBaseFixture::new();
    let observed = observe_candidate(
        &fixture.local,
        Some("candidate"),
        "agent-main",
        "agent-main",
        HandoffDelivery::LocalCandidate,
        "ws",
        "local",
    )
    .expect("local-sync observation");
    assert_eq!(observed.base.commit, fixture.local_tip);
    assert_ne!(observed.base.commit, fixture.remote_tip);
}

/// [ORB-12655] After the candidate is rebased onto `origin/<base>`, a later
/// fetch that advances that shared tracking ref must not change the observed
/// base: it is the merge-base the candidate sits on, not the live tip.
#[test]
fn remote_sync_observation_keeps_the_synchronized_base_when_origin_advances() {
    let fixture = AdvancedOriginFixture::rebased_then_origin_moved();
    let observed = observe_candidate(
        &fixture.local,
        Some("candidate"),
        "agent-main",
        "agent-main",
        HandoffDelivery::PullRequest { number: 1 },
        "ws",
        "remote",
    )
    .expect("a base advance after rebase is not a refusal");
    assert_eq!(observed.base.commit, fixture.synchronized_base);
    assert_ne!(observed.base.commit, fixture.origin_tip);
    assert_eq!(observed.candidate.commit, fixture.candidate_tip);
}

/// [ORB-12655] `claim_validate` / `claim_handoff` re-observe after required
/// validation, which is minutes later than `sync_base`. Origin moving in
/// that window must still produce a handoff pinned to the synchronized base.
#[test]
fn claim_validate_and_handoff_succeed_when_origin_advances_after_rebase() {
    let fixture = AdvancedOriginFixture::rebased_then_origin_moved();
    git(&fixture.local, &["checkout", "candidate"]);
    let host = ClaimHost::pr_mode();
    let input = json!({
        "workspace_path": fixture.local.to_string_lossy(),
        "base_sync": "remote",
        "base_sha": fixture.synchronized_base,
        "pull_request": "1",
    });
    let validated = claim_validate(&host, &input).expect("validate after origin advanced");
    assert_eq!(
        validated["validated_base"].as_str(),
        Some(fixture.synchronized_base.as_str()),
        "handoff evidence must pin the synchronized base, not the live origin tip"
    );
    assert_ne!(
        validated["validated_base"].as_str(),
        Some(fixture.origin_tip.as_str())
    );

    let mut handoff_input = input;
    handoff_input["candidate"] = validated["candidate"].clone();
    handoff_input["validation"] = validated["validation"].clone();
    let handed = claim_handoff(&host, &handoff_input).expect("handoff after origin advanced");
    assert_eq!(handed["handed_off"], json!(true));
    assert_eq!(
        handed["base"].as_str(),
        Some(fixture.synchronized_base.as_str())
    );
    let recorded = host
        .handoff
        .lock()
        .expect("handoff lock")
        .clone()
        .expect("typed handoff recorded");
    assert_eq!(recorded.candidate.base.commit, fixture.synchronized_base);
    assert_eq!(recorded.candidate.candidate.commit, fixture.candidate_tip);
}

/// [ORB-12655] A candidate that does not contain the declared/validated base
/// at all is still a refusal, with evidence naming both commits.
#[test]
fn claim_validate_refuses_a_candidate_that_does_not_contain_the_validated_base() {
    let fixture = AdvancedOriginFixture::diverged_from_validated_base();
    git(&fixture.local, &["checkout", "candidate"]);
    let host = ClaimHost::pr_mode();
    let error = claim_validate(
        &host,
        &json!({
            "workspace_path": fixture.local.to_string_lossy(),
            "base_sync": "remote",
            "base_sha": fixture.synchronized_base,
            "pull_request": "1",
        }),
    )
    .expect_err("a candidate missing the validated base must be refused");
    let message = error.to_string();
    assert!(
        message.contains("does not descend from validated base"),
        "genuine disagreement must be a descent refusal, got: {message}"
    );
    assert!(
        message.contains(&fixture.candidate_tip),
        "refusal must name the candidate, got: {message}"
    );
    assert!(
        message.contains(&fixture.synchronized_base),
        "refusal must name the validated base, got: {message}"
    );
}

/// Host that records claim validation logs and the typed handoff so the
/// activities can be driven without a full runtime.
struct ClaimHost {
    context: ClaimExecutionContext,
    handoff: Mutex<Option<TaskHandoff>>,
}

impl ClaimHost {
    fn pr_mode() -> Self {
        Self {
            context: claim_context("pr"),
            handoff: Mutex::new(None),
        }
    }
}

impl RuntimeHost for ClaimHost {
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn attach_claim_validation_log(
        &self,
        _path: &str,
        _content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn record_claim_handoff(&self, handoff: &TaskHandoff) -> Result<(), OrbitError> {
        *self.handoff.lock().expect("handoff lock") = Some(handoff.clone());
        Ok(())
    }
}

/// Candidate rebased onto `origin/<base>` at `synchronized_base`, then origin
/// advanced to `origin_tip`. Local `agent-main` stays at the clone's first tip.
struct AdvancedOriginFixture {
    _temp: tempfile::TempDir,
    local: std::path::PathBuf,
    synchronized_base: String,
    origin_tip: String,
    candidate_tip: String,
}

impl AdvancedOriginFixture {
    /// [ORB-12655] The failure window: rebase onto S1, then origin moves to S2.
    fn rebased_then_origin_moved() -> Self {
        let (temp, remote, seed, local) = init_remote_pair();
        let synchronized_base = commit_file(&seed, "base.txt", "s1");
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "agent-main"]);
        git(
            temp.path(),
            &[
                "clone",
                "--branch",
                "agent-main",
                remote.to_str().unwrap(),
                local.to_str().unwrap(),
            ],
        );
        git(&local, &["config", "user.name", "Orbit Test"]);
        git(&local, &["config", "user.email", "orbit-test@example.com"]);
        git(&local, &["checkout", "-b", "candidate"]);
        let candidate_tip = commit_file(&local, "work.txt", "claimed");

        let origin_tip = commit_file(&seed, "base.txt", "s2");
        git(&seed, &["push", "origin", "agent-main"]);
        assert_ne!(synchronized_base, origin_tip);
        assert_eq!(git(&local, &["rev-parse", "candidate"]), candidate_tip);

        Self {
            _temp: temp,
            local,
            synchronized_base,
            origin_tip,
            candidate_tip,
        }
    }

    /// Candidate branched from the parent of the validated base, so it does
    /// not contain that SHA at all.
    fn diverged_from_validated_base() -> Self {
        let (temp, remote, seed, local) = init_remote_pair();
        let root = commit_file(&seed, "root.txt", "root");
        let synchronized_base = commit_file(&seed, "base.txt", "validated");
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "agent-main"]);
        git(
            temp.path(),
            &[
                "clone",
                "--branch",
                "agent-main",
                remote.to_str().unwrap(),
                local.to_str().unwrap(),
            ],
        );
        git(&local, &["config", "user.name", "Orbit Test"]);
        git(&local, &["config", "user.email", "orbit-test@example.com"]);
        git(&local, &["checkout", "-b", "candidate", &root]);
        let candidate_tip = commit_file(&local, "work.txt", "diverged");

        let origin_tip = commit_file(&seed, "base.txt", "moved");
        git(&seed, &["push", "origin", "agent-main"]);
        assert_ne!(synchronized_base, origin_tip);
        assert_ne!(synchronized_base, candidate_tip);

        Self {
            _temp: temp,
            local,
            synchronized_base,
            origin_tip,
            candidate_tip,
        }
    }
}

fn init_remote_pair() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let local = temp.path().join("local");
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&seed, "agent-main");
    (temp, remote, seed, local)
}

struct LaggingBaseFixture {
    _temp: tempfile::TempDir,
    local: std::path::PathBuf,
    local_tip: String,
    remote_tip: String,
    candidate_tip: String,
}

impl LaggingBaseFixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let local = temp.path().join("local");

        git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
        init_repo(&seed, "agent-main");
        let local_tip = commit_file(&seed, "base.txt", "v1");
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "agent-main"]);
        git(
            temp.path(),
            &[
                "clone",
                "--branch",
                "agent-main",
                remote.to_str().unwrap(),
                local.to_str().unwrap(),
            ],
        );
        git(&local, &["config", "user.name", "Orbit Test"]);
        git(&local, &["config", "user.email", "orbit-test@example.com"]);

        let remote_tip = commit_file(&seed, "base.txt", "v2");
        git(&seed, &["push", "origin", "agent-main"]);

        // Fetch so the candidate can be created on the remote tip, but leave
        // the local `agent-main` branch where the clone put it.
        git(&local, &["fetch", "origin", "agent-main"]);
        git(
            &local,
            &["checkout", "-b", "candidate", "origin/agent-main"],
        );
        let candidate_tip = commit_file(&local, "work.txt", "claimed");

        assert_eq!(git(&local, &["rev-parse", "agent-main"]), local_tip);
        assert_eq!(git(&local, &["rev-parse", "origin/agent-main"]), remote_tip);
        assert_ne!(local_tip, remote_tip);

        Self {
            _temp: temp,
            local,
            local_tip,
            remote_tip,
            candidate_tip,
        }
    }
}

fn init_repo(path: &Path, branch: &str) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", branch]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
}

fn commit_file(repo: &Path, file_name: &str, contents: &str) -> String {
    fs::write(repo.join(file_name), contents).unwrap();
    git(repo, &["add", file_name]);
    git(repo, &["commit", "-m", &format!("write {file_name}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
