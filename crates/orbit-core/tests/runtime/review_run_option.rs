//! A run's `--review` option adds a review layer before landing that
//! workspace policy does not apply, and can never remove or swap one
//! [ORB-16572].
//!
//! The layer is captured in the run's review admission with `run-option` as
//! its source, a drain's children inherit it, and a resume keeps it. A clash
//! with the other workspace layer, a missing reviewer crew, the local route
//! and a pull drain are refused before any run exists.

use std::path::Path;

use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, TaskComplexity, TaskStatus,
    WorkspaceRuntimeBinding,
};
use orbit_engine::RuntimeHost;
use orbit_tools::{ReservationOwnerContext, ToolContext};
use orbit_types::workflow::{JobRunTrigger, PipelineState, ReviewAdmission, ReviewLayer};
use serde_json::{Value, json};
use tempfile::TempDir;

const CREW: &str = "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"reviewers\"\n[workflow.purpose_crews]\nreview = \"reviewers\"\n";

fn git(cwd: &Path, args: &[&str]) {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A workspace bound to `ship_mode` with `config` as its workspace layer.
/// `shipped_jobs` deploys the catalog and a worker that cannot start, so an
/// admitted submission persists its run and then fails at startup without
/// launching anything.
pub(super) fn open(
    ship_mode: ShipMode,
    config: &str,
    shipped_jobs: bool,
) -> (TempDir, OrbitRuntime) {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "seed"]);
    std::fs::write(repo.join(".orbit/config.toml"), config).unwrap();
    if shipped_jobs {
        orbit_core::bootstrap::init::init_workspace_at_root(
            &global,
            orbit_core::bootstrap::init::InitOptions {
                global_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        orbit_core::test_support::install_substitute_pipeline_worker([root
            .path()
            .join("worker-must-not-start")
            .to_string_lossy()
            .into_owned()]);
    }
    let binding = WorkspaceRuntimeBinding {
        logical_workspace_id: "ws_review_option".to_string(),
        task_partition_id: "ws_review_option".to_string(),
        owner_machine_id: None,
        checkout_role: None,
        repo_root: repo.clone(),
        ship_mode,
        base_branch: Some("main".to_string()),
    };
    let runtime = OrbitRuntime::from_roots_with_binding(&global, &repo.join(".orbit"), binding)
        .unwrap()
        .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm());
    (root, runtime)
}

fn ship(
    runtime: &OrbitRuntime,
    mode: ShipMode,
    review: Option<ReviewLayer>,
) -> Result<orbit_core::PipelineInvokeResult, OrbitError> {
    runtime.submit_ship_run_with_containment(
        mode,
        Some("main"),
        &[],
        CompletionPolicy::Review,
        &[],
        Some("test"),
        None,
        JobRunTrigger::cli(),
        false,
        review,
    )
}

fn drain(
    runtime: &OrbitRuntime,
    review: Option<ReviewLayer>,
) -> Result<orbit_core::PipelineInvokeResult, OrbitError> {
    runtime.submit_workspace_auto_run_with_mode(
        Some(600),
        Some(1),
        CompletionPolicy::Review,
        &[],
        &Default::default(),
        Some("test"),
        None,
        JobRunTrigger::cli(),
        false,
        false,
        false,
        None,
        review,
    )
}

fn refusal(result: Result<orbit_core::PipelineInvokeResult, OrbitError>) -> String {
    match result {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected an invalid-input refusal, got {other:?}"),
    }
}

/// The newest run of `job`: an admitted submission whose worker could not
/// start still persisted it.
fn submitted(runtime: &OrbitRuntime, job: &str) -> String {
    runtime
        .list_job_runs(JobRunListParams {
            job_id: Some(job.to_string()),
            limit: Some(1),
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no {job} run was persisted"))
        .run_id
}

fn layer(runtime: &OrbitRuntime, run_id: &str) -> Value {
    runtime.run_review_layer(run_id).unwrap()
}

fn runs(runtime: &OrbitRuntime) -> usize {
    runtime
        .list_job_runs(JobRunListParams::default())
        .unwrap()
        .len()
}

/// A running drain carrying `input`, as a live window's coordinator does.
fn running_drain(runtime: &OrbitRuntime, input: Value) -> String {
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(input),
            None,
        )
        .unwrap();
    jobs.write_run_state(
        &run.run_id,
        &PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({})),
    )
    .unwrap();
    jobs.mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .unwrap();
    run.run_id
}

/// With workspace review off, `--review` captures its layer with the run
/// option as its origin; without it nothing is captured. A drain's children
/// inherit its layer through the parent path, a local-route task is held for
/// the window, a resume keeps the layer, and the layer the workspace already
/// applies is accepted unchanged.
#[test]
fn a_run_option_adds_review_that_children_and_resumes_keep() {
    if !super::dispatch_admission::isolated(
        "review_run_option::a_run_option_adds_review_that_children_and_resumes_keep",
    ) {
        return;
    }
    let (_root, runtime) = open(ShipMode::Pr, CREW, true);

    for review in [ReviewLayer::BeforeLanding, ReviewLayer::BeforePr] {
        // The substitute worker cannot start, so admission is the last thing
        // that succeeds.
        ship(&runtime, ShipMode::Pr, Some(review)).expect_err("the worker never starts");
        let run = submitted(&runtime, "task_auto_pipeline");
        assert_eq!(
            layer(&runtime, &run),
            json!({"timing": review.as_str(), "source": "run-option"}),
        );
    }
    let resumed_from = submitted(&runtime, "task_auto_pipeline");
    ship(&runtime, ShipMode::Pr, None).expect_err("the worker never starts");
    let plain = layer(&runtime, &submitted(&runtime, "task_auto_pipeline"));
    assert_eq!(plain["timing"], "none", "{plain}");
    assert_ne!(plain["source"], "run-option", "{plain}");

    // A resume carries its persisted admission forward unchanged.
    let _ = runtime.submit_resume_run(&resumed_from, Some("test"), None);
    let resumed = submitted(&runtime, "task_auto_pipeline");
    assert_ne!(resumed, resumed_from, "the resume persisted a new run");
    assert_eq!(
        layer(&runtime, &resumed),
        json!({"timing": "before-pr", "source": "run-option"}),
    );

    drain(&runtime, Some(ReviewLayer::BeforeLanding)).expect_err("the worker never starts");
    let window = submitted(&runtime, "workspace_auto_pipeline");
    let expected = json!({"timing": "before-landing", "source": "run-option"});
    assert_eq!(layer(&runtime, &window), expected);

    // A live window with that admission: its child inherits it, and a task
    // routed to local delivery is held rather than admitted to be refused.
    let live = running_drain(
        &runtime,
        runtime.show_job_run(&window).unwrap().input.unwrap(),
    );
    let context = ToolContext {
        reservation_owner: Some(ReservationOwnerContext {
            owner_run_id: live.clone(),
            owner_metadata_json: Some(
                json!({"pipeline_child_admission": {"action": "ship", "blocking": false}})
                    .to_string(),
            ),
        }),
        session_context: orbit_types::tool::ToolSessionContext {
            effective_capabilities: std::collections::BTreeSet::from([
                orbit_types::tool::McpCapability::Operator,
            ]),
            ..Default::default()
        },
        ..ToolContext::default()
    };
    // The child is admitted, then its worker cannot start.
    let _ = runtime.run_tool_with_context_and_role(
        "orbit.pipeline.invoke",
        json!({"job_name": "task_auto_pipeline", "input": {"task_ids": [], "mode": "pr"}}),
        orbit_types::policy::Role::Admin,
        context,
    );
    // Workspace policy alone would capture none for this child.
    let child = submitted(&runtime, "task_auto_pipeline");
    assert_eq!(layer(&runtime, &child), expected);

    let local = runtime
        .add_task(TaskAddParams {
            title: "routed locally".into(),
            description: "Fixture task.".into(),
            acceptance_criteria: vec!["The task is observable.".into()],
            plan: "Fixture plan.".into(),
            complexity: TaskComplexity::Medium,
            context_files: vec!["dir:.".into()],
            task_type: Some(orbit_core::TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            tags: vec!["delivery:task_local_pipeline".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let listed = runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({"run_id": live}),
            ToolContext::default(),
        )
        .unwrap();
    let held = listed["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == local)
        .unwrap_or_else(|| panic!("the local-route task was admitted: {listed}"));
    assert_eq!(held["reason"], "local_route_before_landing", "{held}");
    assert!(
        held["detail"]
            .as_str()
            .unwrap()
            .starts_with("--review before-landing reviews an open pull request"),
        "{held}"
    );

    // A matching workspace layer remains accepted with its provenance when
    // a reviewer is configured, on both ship and auto.
    for (policy, review, timing) in [
        ("before_pr", ReviewLayer::BeforePr, "before-pr"),
        (
            "before_landing",
            ReviewLayer::BeforeLanding,
            "before-landing",
        ),
    ] {
        let (_root, applied) = open(
            ShipMode::Pr,
            &format!("{CREW}[review]\ntiming = \"{policy}\"\n"),
            true,
        );
        ship(&applied, ShipMode::Pr, Some(review)).expect_err("the worker never starts");
        assert_eq!(
            layer(&applied, &submitted(&applied, "task_auto_pipeline")),
            json!({"timing": timing, "source": "workspace"}),
        );
        drain(&applied, Some(review)).expect_err("the worker never starts");
        assert_eq!(
            layer(&applied, &submitted(&applied, "workspace_auto_pipeline")),
            json!({"timing": timing, "source": "workspace"}),
        );
    }
}

/// A same-layer request still checks the reviewer preflight on both ship and
/// auto, before either path persists a run.
fn same_layer_requests_require_a_reviewer_before_any_run() {
    let crewless = CREW.replace(
        "[workflow.purpose_crews]\nreview = \"reviewers\"",
        "[workflow.purpose_crews]\nreview = []",
    );
    assert_ne!(crewless, CREW, "the fixture removes only the reviewer crew");

    for (policy, review) in [
        ("before_pr", ReviewLayer::BeforePr),
        ("before_landing", ReviewLayer::BeforeLanding),
    ] {
        let config = format!("{crewless}[review]\ntiming = \"{policy}\"\n");
        let (_root, runtime) = open(ShipMode::Pr, &config, true);
        for refused in [
            refusal(ship(&runtime, ShipMode::Pr, Some(review))),
            refusal(drain(&runtime, Some(review))),
        ] {
            assert!(
                refused.contains(
                    "workflow.purpose_crews.pre_landing_review and \
                     workflow.purpose_crews.review are both unset"
                ) && refused.contains(
                    "configure workflow.purpose_crews.pre_landing_review or \
                     workflow.purpose_crews.review"
                ) && refused.contains(&format!("review.timing = {policy} (workspace)")),
                "{refused}"
            );
            assert_eq!(runs(&runtime), 0, "a refusal persists no run");
        }
    }
}

/// The other workspace layer, a missing reviewer crew and the local route
/// are refused before a run exists, each naming why.
fn conflicting_or_unrunnable_requests_are_refused_before_any_run() {
    let (_root, before_pr) = open(
        ShipMode::Pr,
        &format!("{CREW}[review]\ntiming = \"before_pr\"\n"),
        false,
    );
    for refused in [
        refusal(ship(
            &before_pr,
            ShipMode::Pr,
            Some(ReviewLayer::BeforeLanding),
        )),
        refusal(drain(&before_pr, Some(ReviewLayer::BeforeLanding))),
    ] {
        assert!(
            refused.contains(
                "--review before-landing conflicts with review.timing = before_pr (workspace)",
            ) && refused.contains("one review layer before landing"),
            "{refused}"
        );
    }

    let (_root, crewless) = open(ShipMode::Pr, "", false);
    for refused in [
        refusal(ship(&crewless, ShipMode::Pr, Some(ReviewLayer::BeforePr))),
        refusal(drain(&crewless, Some(ReviewLayer::BeforeLanding))),
    ] {
        assert!(
            refused.contains(
                "workflow.purpose_crews.pre_landing_review and workflow.purpose_crews.review \
                 are both unset"
            ),
            "{refused}"
        );
    }

    let (_root, local) = open(ShipMode::Local, CREW, false);
    for refused in [
        refusal(ship(&local, ShipMode::Local, Some(ReviewLayer::BeforePr))),
        refusal(drain(&local, Some(ReviewLayer::BeforeLanding))),
    ] {
        assert!(
            refused.contains("has no meaning on the local-only delivery route")
                && refused.contains("submit without --review"),
            "{refused}"
        );
    }
    for runtime in [&before_pr, &crewless, &local] {
        assert_eq!(runs(runtime), 0, "a refusal persists no run");
    }
}

/// Ordinary input still cannot write the reserved admission, and a request
/// reaches only a delivery run: never a pull drain, whose owner decides its
/// claims' review, nor a job that captures no admission.
fn requests_outside_a_delivery_run_are_refused() {
    let (_root, runtime) = open(ShipMode::Pr, CREW, false);
    let submit =
        |job: &str, input: Value| match runtime.submit_pipeline_run(job, input, None, Some("test"))
        {
            Err(OrbitError::InvalidInput(message)) => message,
            other => panic!("{job} must be refused, got {other:?}"),
        };
    let reserved = submit(
        "task_auto_pipeline",
        json!({"task_ids": [], "review": {"timing": "before-pr"}}),
    );
    assert!(reserved.contains("reserved `review` field"), "{reserved}");
    let pull = submit(
        "workspace_pull_pipeline",
        json!({"review_request": "before-landing"}),
    );
    assert!(pull.contains("the owner decides the review"), "{pull}");
    let unadmitted = submit(
        "ci_failure_sweep_pipeline",
        json!({"review_request": "before-pr"}),
    );
    assert!(
        unadmitted.contains("captures no review admission"),
        "{unadmitted}"
    );
    let malformed = submit(
        "task_auto_pipeline",
        json!({"task_ids": [], "review_request": "after-landing"}),
    );
    assert!(
        malformed.contains("expected before-pr or before-landing"),
        "{malformed}"
    );
    assert_eq!(runs(&runtime), 0, "a refusal persists no run");
}

/// A run's before-landing or before-PR reviewer is captured from
/// `workflow.purpose_crews.pre_landing_review`, and from
/// `workflow.purpose_crews.review` exactly as before while it is unset.
fn the_pre_landing_pool_names_the_captured_reviewer() {
    let split = CREW.replace(
        "[workflow.purpose_crews]\nreview = \"reviewers\"",
        "[crews.fast]\nmodel = \"fast-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\
         [workflow.purpose_crews]\nreview = \"reviewers\"\npre_landing_review = \"fast\"",
    );
    assert_ne!(split, CREW, "the fixture adds only the pre-landing pool");

    for (config, reviewer) in [(split.as_str(), "fast"), (CREW, "reviewers")] {
        let (_root, runtime) = open(ShipMode::Pr, config, true);
        for review in [ReviewLayer::BeforeLanding, ReviewLayer::BeforePr] {
            ship(&runtime, ShipMode::Pr, Some(review)).expect_err("the worker never starts");
            let run = submitted(&runtime, "task_auto_pipeline");
            let input = runtime.show_job_run(&run).unwrap().input.unwrap();
            let admission = ReviewAdmission::from_run_input(&input)
                .unwrap()
                .expect("the run captures a review admission");
            assert_eq!(admission.crew.as_deref(), Some(reviewer), "{input}");
            assert_eq!(admission.crew_source, "workspace", "{input}");
        }
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    review_run_option_refusals,
    conflicting_or_unrunnable_requests_are_refused_before_any_run,
    same_layer_requests_require_a_reviewer_before_any_run,
    requests_outside_a_delivery_run_are_refused,
    the_pre_landing_pool_names_the_captured_reviewer
);
