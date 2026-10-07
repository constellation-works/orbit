//! Delivery observation follows `origin/<branch>` [ORB-14118].
//!
//! A pull request merged on the remote never moves the checkout's local
//! branch. The consumer has to fetch that one ref and read the remote-tracking
//! head. These fixtures keep the objects on a local bare repo so the pass is
//! the real fetch, not a stubbed revision.

use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{Duration, Utc};
use orbit_core::application::automation::evaluate_auto_task;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::automation::{
    AutomationState, BatchAttempt, BatchState, EVIDENCE_AUTHORITY_ARTIFACT, ExaminationCheck,
};
use serde_json::{Value, json};

use crate::auto_task_lifecycle_cli::{git, publish_origin_if_configured};
use crate::isolated_cli_fixture::Fixture;

#[cfg(unix)]
mod tick;

const BRANCH: &str = "fixture-delivery";
const REPOSITORY: &str = "owner/repository";
const CONSUMER: &str = "remote-deliveries";
const REVIEW_CONSUMER: &str = "delivery-code-review";
const REVIEW_CREW: &str = "sonnet";

fn in_isolated_child(test: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_DELIVERY_REMOTE_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(test) {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    false
}

fn open_runtime(fixture: &Fixture) -> orbit_core::OrbitRuntime {
    use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
    use orbit_core::ActorIdentity;

    let roots = RegisteredRuntimeFactory::resolve_roots_for_cwd(&fixture.repo, Some(&fixture.root))
        .unwrap();
    assert!(roots.global_root.starts_with(fixture._temp.path()));
    RegisteredRuntimeFactory::open_resolved_roots(roots)
        .unwrap()
        .with_actor(ActorIdentity::human("fixture"))
}

fn git_at(dir: &Path, home: &Path, args: &[&str]) -> String {
    git_at_env(dir, home, args, &[])
}

fn git_at_env(dir: &Path, home: &Path, args: &[&str], extra: &[(&str, &str)]) -> String {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM");
    for (key, value) in extra {
        command.env(key, value);
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).trim().to_string()
}

fn trigger(max_wait_minutes: u32) -> Value {
    trigger_with_retries(max_wait_minutes, 0)
}

fn trigger_with_retries(max_wait_minutes: u32, retries: u32) -> Value {
    json!({
        "branch": BRANCH,
        "threshold": 1,
        "max_wait_minutes": max_wait_minutes,
        "coverage": "landed_code_review_v1",
        "max_items": 20,
        "retries": retries,
    })
}

fn add_github_origin(fixture: &Fixture) {
    git(
        fixture,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{REPOSITORY}.git"),
        ],
    );
}

fn baselined_remote_consumer(fixture: &Fixture) -> orbit_core::OrbitRuntime {
    baselined_remote_consumer_retries(fixture, 0)
}

/// Baselined delivery consumer whose origin is a bare repo reached through
/// the GitHub URL. The local branch is the baseline commit. `retries` is the
/// trigger's retry budget (`max_attempts` is one more).
fn baselined_remote_consumer_retries(fixture: &Fixture, retries: u32) -> orbit_core::OrbitRuntime {
    git(fixture, &["checkout", "-b", BRANCH]);
    fs::write(fixture.repo.join("fixture.txt"), "baseline\n").unwrap();
    git(fixture, &["add", "fixture.txt"]);
    git(fixture, &["commit", "-m", "Disposable baseline"]);
    add_github_origin(fixture);
    publish_origin_if_configured(fixture);
    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        CONSUMER,
        "--deliveries-landed",
        &trigger_with_retries(60, retries).to_string(),
        "--title",
        "Review remote deliveries",
        "--json",
    ]);
    fixture.json(&["auto-task", "toggle", CONSUMER, "on", "--json"]);
    let runtime = open_runtime(fixture);
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).expect("baseline");
    runtime
}

fn observed_commit(runtime: &orbit_core::OrbitRuntime) -> String {
    let consumer =
        orbit_core::application::automation::consumer_key(runtime, "auto-task", CONSUMER).unwrap();
    runtime
        .automation_store()
        .unwrap()
        .automation_state(&consumer)
        .unwrap()
        .expect("baselined state")
        .observed
        .commit
}

fn local_branch(fixture: &Fixture) -> String {
    git(fixture, &["rev-parse", &format!("refs/heads/{BRANCH}")])
}

fn install_pull(fixture: &Fixture, sha: &str, pr: u64) {
    let pulls = fixture._temp.path().join("pulls");
    fs::create_dir_all(&pulls).unwrap();
    let response = json!([{
        "number": pr,
        "html_url": format!("https://github.com/{REPOSITORY}/pull/{pr}"),
        "merge_commit_sha": sha,
        "merged_at": "2026-10-04T00:00:00Z",
        "base": {"ref": BRANCH, "repo": {"full_name": REPOSITORY}},
    }]);
    fs::write(pulls.join(format!("{sha}.json")), response.to_string()).unwrap();
    let bin = fixture._temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\nsha=${{2#*/commits/}}\nexec cat \"{}/pulls/${{sha%%/*}}.json\"\n",
        fixture._temp.path().display()
    );
    let gh = bin.join("gh");
    fs::write(&gh, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn gh_path(fixture: &Fixture) -> String {
    let bin = fixture._temp.path().join("bin");
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
    .to_string_lossy()
    .into_owned()
}

fn publisher(fixture: &Fixture) -> std::path::PathBuf {
    let bare = fixture.repo.with_file_name("origin.git");
    let publisher = fixture._temp.path().join("publisher");
    git_at(
        &fixture.repo,
        &fixture.home,
        &[
            "clone",
            "-q",
            &bare.display().to_string(),
            &publisher.display().to_string(),
        ],
    );
    publisher
}

/// A commit that lands only on the bare remote mints a batch, and the
/// checkout's branch, index and worktree stay where they were.
#[test]
fn remote_only_landing_mints_a_batch_without_moving_the_checkout() {
    const TEST: &str =
        "delivery_remote_source::remote_only_landing_mints_a_batch_without_moving_the_checkout";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = baselined_remote_consumer(&fixture);
    let local = local_branch(&fixture);
    fs::write(fixture.repo.join("fixture.txt"), "dirty local edit\n").unwrap();
    // `git` trims, so porcelain's leading space is not part of this snapshot.
    // Equality before and after is what shows observation left the index and
    // worktree alone. An empty cached diff is the unstaged edit.
    let status = git(&fixture, &["status", "--porcelain"]);
    let cached = git(&fixture, &["diff", "--cached", "--name-only"]);
    let unstaged = git(&fixture, &["diff", "--name-only"]);
    assert!(cached.is_empty(), "the fixture stages nothing: {cached}");
    assert_eq!(unstaged, "fixture.txt", "{unstaged}");

    let publisher = publisher(&fixture);
    fs::write(publisher.join("fixture.txt"), "landed on the remote only\n").unwrap();
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "-am", "Squash-merge #42"],
    );
    let remote_sha = git_at(&publisher, &fixture.home, &["rev-parse", "HEAD"]);
    git_at(
        &publisher,
        &fixture.home,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    install_pull(&fixture, &remote_sha, 42);
    let path = gh_path(&fixture);
    let _gh = orbit_common::test_env::scoped([("PATH", Some(path.as_str()))]);

    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let diagnostic =
        evaluate_auto_task(&runtime, &definition, false, Utc::now()).expect("remote observation");
    let state = diagnostic
        .state
        .clone()
        .unwrap_or_else(|| panic!("no state: {diagnostic:#?}"));
    let batch = state
        .active
        .as_ref()
        .unwrap_or_else(|| panic!("no batch minted: {state:#?}"))
        .batch
        .clone();
    assert!(
        batch.commits.contains(&remote_sha),
        "the remote commit is the delivery: {batch:#?}"
    );
    assert_eq!(batch.through_inclusive.commit, remote_sha);
    assert_eq!(
        local_branch(&fixture),
        local,
        "observation moved the local branch"
    );
    assert_eq!(git(&fixture, &["status", "--porcelain"]), status);
    assert_eq!(git(&fixture, &["diff", "--cached", "--name-only"]), cached);
    assert_eq!(git(&fixture, &["diff", "--name-only"]), unstaged);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("fixture.txt")).unwrap(),
        "dirty local edit\n"
    );
    assert_eq!(
        git(
            &fixture,
            &["rev-parse", &format!("refs/remotes/origin/{BRANCH}")]
        ),
        remote_sha
    );
    assert_ne!(remote_sha, local);
}

/// A broken origin defers the pass and leaves the cursor on the last observed
/// remote head, including when the local branch has since moved.
#[test]
fn fetch_failure_defers_without_falling_back_to_the_local_branch() {
    const TEST: &str =
        "delivery_remote_source::fetch_failure_defers_without_falling_back_to_the_local_branch";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = baselined_remote_consumer(&fixture);
    let observed = observed_commit(&runtime);
    let bare = fixture.repo.with_file_name("origin.git");
    let key = format!("url.{}.insteadOf", bare.display());
    git(&fixture, &["config", "--unset", &key]);
    git(
        &fixture,
        &["remote", "set-url", "origin", "/no/such/remote.git"],
    );
    fs::write(fixture.repo.join("fixture.txt"), "local only\n").unwrap();
    git(&fixture, &["commit", "-am", "Not on the remote"]);
    let local = local_branch(&fixture);
    assert_ne!(local, observed);

    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let error = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .expect_err("a failed fetch defers");
    assert!(error.to_string().contains("source_fetch_failed"), "{error}");
    assert_eq!(
        observed_commit(&runtime),
        observed,
        "a failed fetch must not advance observation onto the local branch"
    );
}

/// A force-push still fails the ancestry check against the fetched head.
#[test]
fn force_pushed_remote_history_defers_as_history_diverged() {
    const TEST: &str =
        "delivery_remote_source::force_pushed_remote_history_defers_as_history_diverged";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = baselined_remote_consumer(&fixture);
    let observed = observed_commit(&runtime);
    let local = local_branch(&fixture);
    let publisher = publisher(&fixture);
    git_at(
        &publisher,
        &fixture.home,
        &["checkout", "--orphan", "diverged"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "--allow-empty", "-m", "unrelated history"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &[
            "push",
            "--force",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{BRANCH}"),
        ],
    );

    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    match evaluate_auto_task(&runtime, &definition, false, Utc::now()) {
        Ok(diagnostic) => {
            assert!(
                diagnostic.reason.contains("history_diverged"),
                "{diagnostic:#?}"
            );
            let state = diagnostic.state.expect("state");
            assert_eq!(state.observed.commit, observed);
        }
        Err(error) => {
            assert!(error.to_string().contains("history_diverged"), "{error}");
            assert_eq!(observed_commit(&runtime), observed);
        }
    }
    assert_eq!(local_branch(&fixture), local);
}

fn consumer_state(runtime: &orbit_core::OrbitRuntime) -> AutomationState {
    let consumer =
        orbit_core::application::automation::consumer_key(runtime, "auto-task", CONSUMER).unwrap();
    runtime
        .automation_store()
        .unwrap()
        .automation_state(&consumer)
        .unwrap()
        .expect("consumer state")
}

fn evaluate_consumer(
    runtime: &orbit_core::OrbitRuntime,
) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, orbit_common::OrbitError> {
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    evaluate_auto_task(runtime, &definition, false, Utc::now())
}

fn with_pull_lookup<T>(fixture: &Fixture, body: impl FnOnce() -> T) -> T {
    let path = gh_path(fixture);
    let _gh = orbit_common::test_env::scoped([("PATH", Some(path.as_str()))]);
    body()
}

fn break_origin(fixture: &Fixture) {
    let bare = fixture.repo.with_file_name("origin.git");
    let key = format!("url.{}.insteadOf", bare.display());
    git(fixture, &["config", "--unset", &key]);
    git(
        fixture,
        &["remote", "set-url", "origin", "/no/such/remote.git"],
    );
}

/// The publisher clone from admission is already on disk. A second clone into
/// the same path would fail, so the rewrite runs there.
fn force_push_unrelated_history(fixture: &Fixture) {
    let publisher = fixture._temp.path().join("publisher");
    assert!(
        publisher.join(".git").is_dir(),
        "admission did not leave a publisher clone"
    );
    git_at(
        &publisher,
        &fixture.home,
        &["checkout", "--orphan", "diverged"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "--allow-empty", "-m", "unrelated history"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &[
            "push",
            "--force",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{BRANCH}"),
        ],
    );
}

fn automation_consumers_message(fixture: &Fixture) -> String {
    let output = fixture.command(&["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor json: {error}; stderr {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    rows.as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["check"] == "automation-consumers")
                .and_then(|row| row["message"].as_str().map(str::to_string))
        })
        .unwrap_or_else(|| panic!("no automation-consumers message: {rows}"))
}

/// Lands one remote commit, admits its batch, attaches coverage from that
/// batch's executor run, and rejects the review task. `retries` is the
/// trigger budget, so a spent retry is visible when it is at least 1.
fn stopped_action_with_coverage(
    fixture: &Fixture,
    retries: u32,
) -> (orbit_core::OrbitRuntime, BatchAttempt) {
    let runtime = baselined_remote_consumer_retries(fixture, retries);
    let publisher = publisher(fixture);
    fs::write(publisher.join("fixture.txt"), "landed on the remote only\n").unwrap();
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "-am", "Squash-merge #42"],
    );
    let remote_sha = git_at(&publisher, &fixture.home, &["rev-parse", "HEAD"]);
    git_at(
        &publisher,
        &fixture.home,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    install_pull(fixture, &remote_sha, 42);
    with_pull_lookup(fixture, || {
        evaluate_consumer(&runtime).expect("admit the remote delivery");
    });

    let attempt = consumer_state(&runtime).active.expect("admitted batch");
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.state, BatchState::Admitted);
    let action_id = attempt.action_id.clone().expect("admitted action");

    let run = RuntimeHost::insert_job_run(
        &runtime,
        "delivery-evidence",
        1,
        Utc::now(),
        Some(json!({ "task_id": action_id })),
        None,
    )
    .expect("insert the executor run");
    RuntimeHost::apply_task_automation_update(
        &runtime,
        &action_id,
        TaskAutomationUpdate {
            job_run_id: Some(run.run_id.clone()),
            ..TaskAutomationUpdate::default()
        },
    )
    .expect("bind the executor run");

    let mut evidence = orbit_types::workflow::automation::evidence_template(&attempt);
    evidence.examination_complete = true;
    evidence.checks = vec![ExaminationCheck {
        subject: "frozen range".into(),
        method: "review".into(),
        observation: "examined the commits named by the batch".into(),
    }];
    fs::write(
        fixture.repo.join("automation-coverage.json"),
        serde_json::to_vec(&evidence).expect("coverage serializes"),
    )
    .unwrap();
    let input = json!({
        "id": action_id,
        "source_path": "automation-coverage.json",
        "path": "automation-coverage.json",
        "model": "grok",
    })
    .to_string();
    fixture
        .command(&["tool", "run", "orbit.task.artifact.put", "--input", &input])
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", &run.run_id)
        .assert()
        .success();
    assert!(
        runtime
            .get_task_artifact(&action_id, EVIDENCE_AUTHORITY_ARTIFACT)
            .expect("read authority")
            .is_some(),
        "coverage put did not record the executor run"
    );
    fixture.json(&[
        "task", "update", &action_id, "--status", "rejected", "--force", "--json",
    ]);
    (runtime, attempt)
}

/// Evidence verification fetches origin. A fetch failure while a stopped
/// action holds coverage defers the pass: the attempt and covered cursor stay
/// put, and doctor does not report the action as wedged.
#[test]
fn fetch_failure_during_evidence_verification_defers_without_spending_a_retry() {
    const TEST: &str = "delivery_remote_source::fetch_failure_during_evidence_verification_defers_without_spending_a_retry";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let (runtime, _) = stopped_action_with_coverage(&fixture, 1);
    let before = consumer_state(&runtime);
    break_origin(&fixture);

    let error = with_pull_lookup(&fixture, || evaluate_consumer(&runtime))
        .expect_err("a fetch failure defers evidence verification");
    assert!(error.to_string().contains("source_fetch_failed"), "{error}");
    let after = consumer_state(&runtime);
    assert_eq!(after.generation, before.generation, "{after:#?}");
    assert_eq!(after.active, before.active);
    assert_eq!(after.covered, before.covered);

    let message = automation_consumers_message(&fixture);
    assert!(
        !message.contains("wedged"),
        "origin outage reports a wedged action: {message}"
    );
    let still = consumer_state(&runtime);
    assert_eq!(still.generation, before.generation);
    assert_eq!(still.active, before.active);
}

/// A force-push is a real batch mismatch, so evidence is rejected and one
/// retry is spent. Observation then sees the same divergence and may stall.
/// The stall write only moves the marker, so the settled attempt remains.
#[test]
fn diverged_history_rejects_stopped_evidence_as_source_unverifiable() {
    const TEST: &str =
        "delivery_remote_source::diverged_history_rejects_stopped_evidence_as_source_unverifiable";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let (runtime, _) = stopped_action_with_coverage(&fixture, 1);
    let before = consumer_state(&runtime);
    force_push_unrelated_history(&fixture);

    let outcome = with_pull_lookup(&fixture, || evaluate_consumer(&runtime));
    let after = consumer_state(&runtime);
    let active = after.active.as_ref().unwrap_or_else(|| {
        panic!("mismatch retired the batch: outcome={outcome:#?} state={after:#?}")
    });
    assert!(
        active
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("source_unverifiable")),
        "outcome={outcome:#?} active={active:#?}"
    );
    assert_eq!(
        active.attempt,
        before.active.as_ref().expect("admitted attempt").attempt + 1
    );
    assert_eq!(active.action_id, None);
    assert_eq!(active.state, BatchState::Claimed);
    assert_eq!(after.covered, before.covered);
}

fn enable_review_crew(fixture: &Fixture) {
    for (key, value) in [
        ("crews.sonnet.enabled", "true"),
        ("workflow.default_crew", REVIEW_CREW),
        ("workflow.system_crew", REVIEW_CREW),
    ] {
        fixture
            .command(&["config", "set", "--global", key, value])
            .assert()
            .success();
    }
}

fn retarget_review(fixture: &Fixture, max_wait_minutes: u32) {
    fixture.json(&[
        "auto-task",
        "update",
        REVIEW_CONSUMER,
        "--deliveries-landed",
        &trigger(max_wait_minutes).to_string(),
        "--json",
    ]);
}

fn doctor_review(fixture: &Fixture) -> Value {
    let output = fixture.command(&["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "review")
        .cloned()
        .unwrap_or_else(|| panic!("no review row: {rows}"))
}

fn baseline_review_consumer(fixture: &Fixture, max_wait_minutes: u32) -> String {
    enable_review_crew(fixture);
    git(fixture, &["checkout", "-b", BRANCH]);
    fs::write(fixture.repo.join("fixture.txt"), "baseline\n").unwrap();
    git(fixture, &["add", "fixture.txt"]);
    git(fixture, &["commit", "-m", "Disposable baseline"]);
    retarget_review(fixture, max_wait_minutes);
    fixture
        .command(&["auto-task", "toggle", REVIEW_CONSUMER, "on"])
        .assert()
        .success();
    let runtime = open_runtime(fixture);
    let definition = runtime.auto_task_show(REVIEW_CONSUMER).unwrap().unwrap();
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).expect("baseline");
    local_branch(fixture)
}

fn bare_origin(fixture: &Fixture) -> std::path::PathBuf {
    let bare = fixture.repo.with_file_name("origin.git");
    git_at(
        &fixture.repo,
        &fixture.home,
        &["init", "--bare", "-q", &bare.display().to_string()],
    );
    git_at(
        &bare,
        &fixture.home,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{BRANCH}")],
    );
    git(
        fixture,
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    bare
}

fn fetch_remote_tracking(fixture: &Fixture) {
    git(
        fixture,
        &[
            "fetch",
            "origin",
            &format!("+refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}"),
        ],
    );
}

/// Doctor names the remote-tracking head. A missing ref is not ok once the
/// consumer has a cursor. A trail younger than `max_wait_minutes` is reported
/// and still ok. The local branch does not move.
#[test]
fn doctor_reports_the_remote_tracking_head_without_fetching() {
    const TEST: &str =
        "delivery_remote_source::doctor_reports_the_remote_tracking_head_without_fetching";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let local = baseline_review_consumer(&fixture, 60);
    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("branch `{BRANCH}` resolves")),
        "{message}"
    );
    assert!(!message.contains("origin/"), "{message}");

    let bare = bare_origin(&fixture);
    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "error", "{row}");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("has no remote-tracking ref"),
        "{row}"
    );

    git(
        &fixture,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    fetch_remote_tracking(&fixture);
    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains(&format!("matches origin/{BRANCH}")),
        "{row}"
    );

    let publisher = fixture._temp.path().join("publisher");
    git_at(
        &fixture.repo,
        &fixture.home,
        &[
            "clone",
            "-q",
            &bare.display().to_string(),
            &publisher.display().to_string(),
        ],
    );
    fs::write(publisher.join("fixture.txt"), "landed remotely\n").unwrap();
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "-am", "Remote landing"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    fetch_remote_tracking(&fixture);
    assert_eq!(local_branch(&fixture), local);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("fixture.txt")).unwrap(),
        "baseline\n"
    );

    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("trails"), "{message}");
    assert!(message.contains("by 1"), "{message}");
    assert_eq!(
        row["status"], "ok",
        "a fresh trail is inside max_wait_minutes: {row}"
    );
}

/// Remote trail health follows the oldest pending first-parent commit, even
/// when the tip is recent. Cover both sides of the wait boundary [ORB-14142].
#[test]
fn doctor_uses_the_oldest_pending_commit_for_remote_trail_health() {
    const TEST: &str =
        "delivery_remote_source::doctor_uses_the_oldest_pending_commit_for_remote_trail_health";
    if !in_isolated_child(TEST) {
        return;
    }

    for (oldest_age_minutes, expected_status) in [(30, "ok"), (120, "error")] {
        let fixture = Fixture::new();
        let local = baseline_review_consumer(&fixture, 60);
        let bare = bare_origin(&fixture);
        git(
            &fixture,
            &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
        );
        let publisher = fixture._temp.path().join("publisher");
        git_at(
            &fixture.repo,
            &fixture.home,
            &[
                "clone",
                "-q",
                &bare.display().to_string(),
                &publisher.display().to_string(),
            ],
        );
        let now = Utc::now();
        for (contents, message, landed_at) in [
            (
                "older remote landing\n",
                "Older remote landing",
                now - Duration::minutes(oldest_age_minutes),
            ),
            ("recent remote tip\n", "Recent remote tip", now),
        ] {
            fs::write(publisher.join("fixture.txt"), contents).unwrap();
            let date = landed_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
            git_at_env(
                &publisher,
                &fixture.home,
                &["commit", "-am", message],
                &[("GIT_AUTHOR_DATE", &date), ("GIT_COMMITTER_DATE", &date)],
            );
        }
        git_at(
            &publisher,
            &fixture.home,
            &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
        );
        fetch_remote_tracking(&fixture);
        assert_eq!(local_branch(&fixture), local);

        let row = doctor_review(&fixture);
        assert_eq!(
            row["status"], expected_status,
            "oldest pending commit is {oldest_age_minutes} minutes old, tip is recent: {row}"
        );
        let message = row["message"].as_str().unwrap();
        assert!(message.contains("trails"), "{message}");
        assert!(message.contains("by 2"), "{message}");
        assert_eq!(
            message.contains("max_wait_minutes"),
            expected_status == "error",
            "{message}"
        );
    }
}
