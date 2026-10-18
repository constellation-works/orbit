//! A drain on a host that requires a Claude worker credential and holds none
//! excludes the Claude crews from its window, names the remediation once, and
//! refuses an explicit `--allow-crew` that names only Claude crews [ORB-16663].
//! With the credential in the drain's environment, the window is unchanged.
//!
//! macOS only: the requirement is the host's, and only macOS requires it.

use orbit_core::application::job::{JobRunListParams, SubmissionKind};
use orbit_core::{CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, WorkspacePullRequest};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

use super::review_run_option::open;

const CONFIG: &str = "[crews.writer]\nmodel = \"writer-model\"\nprovider = \"claude\"\nbackend = \"cli\"\n\n\
     [crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\n\
     [workflow]\ndefault_crew = \"reviewers\"\n\n\
     [execution.env]\npass = [\"HOME\", \"PATH\", \"CLAUDE_CODE_OAUTH_TOKEN\"]\n";

const REMEDY_MARK: &str = "setup-token";

fn submit(runtime: &OrbitRuntime, allowed: &[String]) -> Result<(), OrbitError> {
    runtime
        .submit_workspace_auto_run_with_mode(
            Some(600),
            Some(1),
            CompletionPolicy::Review,
            allowed,
            &Default::default(),
            Some("test"),
            None,
            JobRunTrigger::cli(),
            false,
            false,
            false,
            None,
            None,
        )
        .map(|_| ())
}

/// Admit a drain that must not be refused. The shipped worker cannot start in
/// this fixture, so an admitted submission persists its run and then reports
/// that spawn failure.
fn admit(runtime: &OrbitRuntime, allowed: &[String]) {
    match submit(runtime, allowed) {
        Ok(()) => {}
        Err(OrbitError::Execution(message)) if message.contains("spawn pipeline worker") => {}
        Err(error) => panic!("the drain was refused: {error}"),
    }
}

/// The input of the newest drain run, which an admitted submission persists
/// even though its worker never starts.
fn drain_input(runtime: &OrbitRuntime) -> Value {
    runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("workspace_auto_pipeline".to_string()),
            limit: Some(1),
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .next()
        .expect("the drain was persisted")
        .input
        .expect("the drain carries its input")
}

fn remedy_warnings(runtime: &OrbitRuntime) -> usize {
    runtime
        .submission_notices(SubmissionKind::Drain)
        .warnings()
        .iter()
        .filter(|warning| warning.contains(REMEDY_MARK))
        .count()
}

#[test]
fn a_drain_without_a_worker_credential_excludes_claude_crews_and_names_the_fix() {
    if !super::dispatch_admission::isolated(
        "claude_worker_credential::a_drain_without_a_worker_credential_excludes_claude_crews_and_names_the_fix",
    ) {
        return;
    }
    let _env = orbit_common::test_env::unset(["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
    let (_root, runtime) = open(ShipMode::Pr, CONFIG, true);

    admit(&runtime, &[]);
    assert_eq!(
        drain_input(&runtime)["allowed_crews"],
        json!(["reviewers"]),
        "every enabled non-Claude crew, and no Claude crew"
    );
    assert_eq!(
        remedy_warnings(&runtime),
        1,
        "the remediation is named once per submission"
    );

    // A window that names a Claude crew alongside one it can run keeps the
    // runnable one only.
    admit(&runtime, &["writer".to_string(), "reviewers".to_string()]);
    assert_eq!(drain_input(&runtime)["allowed_crews"], json!(["reviewers"]));
}

#[test]
fn a_claude_provider_alias_is_excluded_and_refused_like_its_canonical_provider() {
    if !super::dispatch_admission::isolated(
        "claude_worker_credential::a_claude_provider_alias_is_excluded_and_refused_like_its_canonical_provider",
    ) {
        return;
    }
    let _env = orbit_common::test_env::unset(["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
    let config = CONFIG.replace("provider = \"claude\"", "provider = \"anthropic\"");
    let (_root, runtime) = open(ShipMode::Pr, &config, true);

    admit(&runtime, &[]);
    assert_eq!(drain_input(&runtime)["allowed_crews"], json!(["reviewers"]));
    assert_eq!(remedy_warnings(&runtime), 1);

    let refusal = match submit(&runtime, &["writer".to_string()]) {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected an invalid-input refusal for a Claude alias, got {other:?}"),
    };
    assert!(refusal.contains(REMEDY_MARK), "{refusal}");

    let allowed = ["writer".to_string()];
    let refusal = match runtime.submit_workspace_pull_run(
        WorkspacePullRequest {
            selector: "owner-machine/ws_claude_credential",
            for_seconds: Some(60),
            max_active_leaf_runs: None,
            allowed_crews: &allowed,
            actor: None,
        },
        JobRunTrigger::cli(),
    ) {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected a pull refusal for a Claude alias before probing, got {other:?}"),
    };
    assert!(refusal.contains(REMEDY_MARK), "{refusal}");
}

#[test]
fn an_allow_crew_naming_only_claude_crews_is_refused_with_the_remediation() {
    if !super::dispatch_admission::isolated(
        "claude_worker_credential::an_allow_crew_naming_only_claude_crews_is_refused_with_the_remediation",
    ) {
        return;
    }
    let _env = orbit_common::test_env::unset(["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
    let (_root, runtime) = open(ShipMode::Pr, CONFIG, true);

    let refusal = match submit(&runtime, &["writer".to_string()]) {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected an invalid-input refusal, got {other:?}"),
    };
    assert!(
        refusal.contains("--allow-crew") && refusal.contains(REMEDY_MARK),
        "{refusal}"
    );
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .unwrap()
            .is_empty(),
        "a refused window persists no run"
    );
}

#[test]
fn a_worker_credential_in_the_drain_environment_leaves_the_window_unchanged() {
    if !super::dispatch_admission::isolated(
        "claude_worker_credential::a_worker_credential_in_the_drain_environment_leaves_the_window_unchanged",
    ) {
        return;
    }
    let _env = orbit_common::test_env::scoped([
        ("CLAUDE_CODE_OAUTH_TOKEN", Some("fixture-worker-token")),
        ("ANTHROPIC_API_KEY", None),
    ]);
    let (_root, runtime) = open(ShipMode::Pr, CONFIG, true);

    admit(&runtime, &[]);
    let allowed = &drain_input(&runtime)["allowed_crews"];
    assert!(
        allowed.as_array().is_none_or(Vec::is_empty),
        "no window is imposed when the credential is held: {allowed}"
    );
    assert_eq!(remedy_warnings(&runtime), 0);

    // Naming a Claude crew is allowed too, as it was before the check.
    admit(&runtime, &["writer".to_string()]);
    assert_eq!(drain_input(&runtime)["allowed_crews"], json!(["writer"]));
}

#[test]
fn a_pull_drain_without_a_worker_credential_refuses_a_claude_only_window_before_any_probe() {
    if !super::dispatch_admission::isolated(
        "claude_worker_credential::a_pull_drain_without_a_worker_credential_refuses_a_claude_only_window_before_any_probe",
    ) {
        return;
    }
    let _env = orbit_common::test_env::unset(["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
    let (_root, runtime) = open(ShipMode::Pr, CONFIG, false);
    let allowed = ["writer".to_string()];

    let refusal = match runtime.submit_workspace_pull_run(
        WorkspacePullRequest {
            selector: "owner-machine/ws_claude_credential",
            for_seconds: Some(60),
            max_active_leaf_runs: None,
            allowed_crews: &allowed,
            actor: None,
        },
        JobRunTrigger::cli(),
    ) {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected an invalid-input refusal, got {other:?}"),
    };
    assert!(
        refusal.contains("--allow-crew") && refusal.contains(REMEDY_MARK),
        "{refusal}"
    );
}
