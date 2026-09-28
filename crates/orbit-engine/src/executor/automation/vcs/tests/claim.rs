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
    MAX_CAPTURED_OUTPUT_BYTES, MAX_HANDOFF_SUMMARY_BYTES, capture, claim_handoff, claim_validate,
    delivery, observe_candidate, pull_request_number, slug,
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

fn local_candidate() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let temp = tempdir().expect("candidate tempdir");
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, ".gitignore", "build/\n");
    git(&repo, &["checkout", "-b", "candidate"]);
    let head = commit_file(&repo, "input.txt", "fail\n");
    (temp, repo, head)
}

fn local_input(repo: &Path) -> serde_json::Value {
    json!({"workspace_path": repo.to_string_lossy()})
}

#[test]
fn dirty_tracked_edit_that_alone_makes_the_check_pass_is_refused() {
    for staged in [false, true] {
        let (_temp, repo, head) = local_candidate();
        fs::write(repo.join("input.txt"), "pass\n").expect("edit candidate input");
        if staged {
            git(&repo, &["add", "input.txt"]);
        }
        assert!(
            Command::new("/bin/sh")
                .args(["-c", "test \"$(cat input.txt)\" = pass"])
                .current_dir(&repo)
                .status()
                .expect("required command")
                .success(),
            "the uncommitted edit alone must make the required command pass"
        );
        let host = ClaimHost::local_with_commands(&["test \"$(cat input.txt)\" = pass"]);
        let error = claim_validate(&host, &local_input(&repo))
            .expect_err("a passing dirty worktree cannot certify clean HEAD");
        assert!(error.to_string().contains("staged, tracked, or untracked"));
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(host.log_count(), 0);
    }
}

#[test]
fn relevant_untracked_candidate_input_is_refused() {
    let (_temp, repo, _) = local_candidate();
    fs::write(repo.join("proof.txt"), "pass\n").expect("untracked input");
    assert!(
        Command::new("/bin/sh")
            .args(["-c", "test -f proof.txt"])
            .current_dir(&repo)
            .status()
            .expect("required command")
            .success()
    );
    let host = ClaimHost::local_with_commands(&["test -f proof.txt"]);
    let error = claim_validate(&host, &local_input(&repo))
        .expect_err("untracked input cannot certify a clean HEAD");
    assert!(error.to_string().contains("staged, tracked, or untracked"));
    assert_eq!(host.log_count(), 0);
}

#[test]
fn required_command_that_changes_source_or_head_emits_no_passing_logs() {
    for mutating_command in [
        "printf pass > input.txt",
        "git commit --allow-empty -m advanced",
        "git checkout -b other",
    ] {
        let (_temp, repo, head) = local_candidate();
        let host = ClaimHost::local_with_commands(&["true", mutating_command]);
        let error = claim_validate(&host, &local_input(&repo))
            .expect_err("a command that changes the candidate must be refused");
        let message = error.to_string();
        assert!(
            message.contains("staged, tracked, or untracked")
                || message.contains("checked-out HEAD moved")
                || message.contains("checked-out source branch moved"),
            "unexpected refusal: {message}"
        );
        assert_eq!(host.log_count(), 0, "earlier passing logs are withheld");
        assert!(host.handoff.lock().expect("handoff lock").is_none());
        if mutating_command.starts_with("git commit") {
            assert_ne!(git(&repo, &["rev-parse", "HEAD"]), head);
        } else {
            assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        }
    }
}

#[test]
fn clean_candidate_succeeds_with_ignored_build_output() {
    let (_temp, repo, head) = local_candidate();
    let host = ClaimHost::local_with_commands(&[
        "mkdir -p build && printf artifact > build/output",
        "test -f build/output && test \"$(cat input.txt)\" = fail",
    ]);
    let input = local_input(&repo);
    let validated = claim_validate(&host, &input).expect("clean candidate validation");
    assert_eq!(validated["tested_head"], head);
    assert_eq!(host.log_count(), 2);
    assert!(repo.join("build/output").exists());
    assert_eq!(
        git(&repo, &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );

    let mut handoff_input = input;
    handoff_input["candidate"] = validated["candidate"].clone();
    handoff_input["validation"] = validated["validation"].clone();
    let result = claim_handoff(&host, &handoff_input).expect("clean handoff");
    assert_eq!(result["handed_off"], true);
    assert!(host.handoff.lock().expect("handoff lock").is_some());
}

#[test]
fn handoff_refuses_a_dirty_tree_after_validation() {
    let (_temp, repo, _) = local_candidate();
    let host = ClaimHost::local_with_commands(&["true"]);
    let input = local_input(&repo);
    let validated = claim_validate(&host, &input).expect("initial validation");
    fs::write(repo.join("input.txt"), "changed\n").expect("change candidate after validation");
    let mut handoff_input = input;
    handoff_input["candidate"] = validated["candidate"].clone();
    handoff_input["validation"] = validated["validation"].clone();
    let error = claim_handoff(&host, &handoff_input)
        .expect_err("dirty candidate must not reach typed handoff");
    assert!(error.to_string().contains("staged, tracked, or untracked"));
    assert!(host.handoff.lock().expect("handoff lock").is_none());
}

/// Validate the checked-out candidate and hand it off with `extra` merged
/// into the handoff input, returning the typed handoff's summary or the
/// handoff's refusal.
fn hand_off(repo: &Path, extra: serde_json::Value) -> (ClaimHost, Result<String, OrbitError>) {
    let host = ClaimHost::local_with_commands(&["true"]);
    let mut input = local_input(repo);
    let validated = claim_validate(&host, &input).expect("clean candidate validation");
    input["candidate"] = validated["candidate"].clone();
    input["validation"] = validated["validation"].clone();
    if let serde_json::Value::Object(extra) = extra {
        for (key, value) in extra {
            input[key.as_str()] = value;
        }
    }
    let result = claim_handoff(&host, &input).map(|_| {
        host.handoff
            .lock()
            .expect("handoff lock")
            .clone()
            .expect("typed handoff recorded")
            .execution_summary
    });
    (host, result)
}

/// The typed handoff is what writes the owner's `execution_summary`, and a
/// claimed-mode implementer writes no owner task state: its summary arrives
/// as the implement step's output and has to travel in the handoff.
#[test]
fn handoff_carries_the_claimed_implementers_output_summary() {
    let (_temp, repo, head) = local_candidate();
    let summary = "Outcome: success\nChanges:\n- wrote input.txt\nAssessment: done";
    let (_host, handed) = hand_off(
        &repo,
        json!({"implementation": {
            "summary": "short result",
            "execution_summary": summary,
            "comment": "left a note for the owner",
            "context_files_added": ["file:docs/new.md", "  "],
        }}),
    );
    let handed = handed.expect("an implementer summary hands off");

    assert!(
        handed.starts_with(summary),
        "the implementer's words come first: {handed}"
    );
    assert!(handed.contains("left a note for the owner"), "{handed}");
    assert!(handed.contains("- file:docs/new.md"), "{handed}");
    assert!(
        !handed.contains("short result"),
        "the full summary wins over the short one: {handed}"
    );
    assert!(
        handed.ends_with(&format!(
            "Claimed execution delivered candidate {head} on base {}; required validation passed \
             on the exact candidate and the owner holds every captured log.",
            git(&repo, &["rev-parse", "agent-main"])
        )),
        "the delivery line names the candidate: {handed}"
    );
}

#[test]
fn handoff_falls_back_to_the_short_summary_then_to_the_delivery_statement() {
    let (_temp, repo, head) = local_candidate();
    let (_host, handed) = hand_off(
        &repo,
        json!({"implementation": {"summary": "short result"}}),
    );
    assert!(handed.expect("short summary").starts_with("short result"));

    // A deterministic stand-in reports neither; so does an absent output.
    for extra in [json!({"implementation": {"stdout": ""}}), json!({})] {
        let (_host, handed) = hand_off(&repo, extra);
        let handed = handed.expect("no summary still hands off");
        assert!(
            handed.starts_with(&format!("Claimed execution delivered candidate {head}")),
            "{handed}"
        );
    }
}

#[test]
fn an_explicit_summary_input_wins_over_the_implementer_output() {
    let (_temp, repo, _) = local_candidate();
    let (_host, handed) = hand_off(
        &repo,
        json!({
            "execution_summary": "Outcome: success\nfrom the input",
            "implementation": {"execution_summary": "Outcome: success\nfrom the output"},
        }),
    );
    let handed = handed.expect("explicit summary hands off");
    assert!(
        handed.starts_with("Outcome: success\nfrom the input"),
        "{handed}"
    );
    assert!(!handed.contains("from the output"), "{handed}");
}

/// The owner refuses a handoff whose summary opens with `Outcome: failed`;
/// refusing it here keeps a failed implementation from becoming a durable
/// settlement the owner can only reject.
#[test]
fn an_implementer_summary_reporting_failure_is_not_handed_off() {
    let (_temp, repo, _) = local_candidate();
    let (host, handed) = hand_off(
        &repo,
        json!({"implementation": {"execution_summary": "\n  Outcome: failed\nincomplete"}}),
    );
    let error = handed.expect_err("a failed summary is refused");
    assert!(error.to_string().contains("Outcome: failed"), "{error}");
    assert!(host.handoff.lock().expect("handoff lock").is_none());
}

#[test]
fn an_oversized_implementer_summary_is_truncated_and_says_so() {
    let (_temp, repo, _) = local_candidate();
    let (_host, handed) = hand_off(
        &repo,
        json!({"implementation": {"execution_summary": "y".repeat(MAX_HANDOFF_SUMMARY_BYTES + 100)}}),
    );
    let handed = handed.expect("oversized summary hands off");
    assert!(
        handed.contains("[summary truncated to"),
        "truncation is reported"
    );
    assert!(handed.len() < MAX_HANDOFF_SUMMARY_BYTES + 1024);
}

/// Host that records claim validation logs and the typed handoff so the
/// activities can be driven without a full runtime.
struct ClaimHost {
    context: ClaimExecutionContext,
    logs: Mutex<Vec<String>>,
    handoff: Mutex<Option<TaskHandoff>>,
}

impl ClaimHost {
    fn pr_mode() -> Self {
        Self {
            context: claim_context("pr"),
            logs: Mutex::new(Vec::new()),
            handoff: Mutex::new(None),
        }
    }

    fn local_with_commands(commands: &[&str]) -> Self {
        let mut context = claim_context("local");
        context.required_commands = commands.iter().map(|command| (*command).into()).collect();
        Self {
            context,
            logs: Mutex::new(Vec::new()),
            handoff: Mutex::new(None),
        }
    }

    fn log_count(&self) -> usize {
        self.logs.lock().expect("validation logs lock").len()
    }
}

impl RuntimeHost for ClaimHost {
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn attach_claim_validation_log(&self, path: &str, _content: Vec<u8>) -> Result<(), OrbitError> {
        self.logs
            .lock()
            .expect("validation logs lock")
            .push(path.into());
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
