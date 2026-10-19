//! Who may delegate read-only exploration through `orbit.agent.invoke`, and
//! inside which bounds [ORB-16814].
//!
//! A managed implement run calls the tool with the managed-run environment
//! its MCP server inherits. That call gets a sandboxed explorer from the
//! explorer crew pool in the caller's own worktree, never a trusted-host
//! admission; every refusal is a typed error that leaves no run behind.
//! Each row runs in its own child process because it sets that environment.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::activity_job::TRUSTED_HOST_ADMISSION_KEY;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::TempDir;

use orbit_core::OrbitRuntime;
use orbit_core::application::job::{
    AgentInvocation, AgentInvokeRequest, EXPLORE_JOB_ID, ExploreSubmission,
};

use super::review_support::git;
use super::store_support::jobs;

const CREWS: &str = r#"
[workflow]
default_crew = "opus"

[workflow.purpose_crews]
explorer = ["haiku"]

[crews.haiku]
provider = "claude"
model = "claude-haiku-4-5"
backend = "cli"

[crews.opus]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"
"#;

const BUDGETS: &str = r#"
[workflow.explore]
enabled = true
max_calls_per_run = 2
max_concurrent_per_run = 1
timeout_seconds = 300
"#;

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    parent: JobRun,
    worktree: PathBuf,
}

/// A git workspace whose implement run is live in its own linked worktree,
/// with `explore` as the workspace's `[workflow.explore]` table.
fn fixture(explore: &str) -> Fixture {
    let (root, runtime, repo) =
        super::engine_support::runtime_with_workspace_config(Some(&format!("{CREWS}\n{explore}")));
    orbit_core::bootstrap::init::init_workspace_at_root(
        &runtime.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            ..Default::default()
        },
    )
    .expect("seed the shipped job catalog");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("ignore orbit store");
    std::fs::create_dir_all(repo.join("src")).expect("source dir");
    std::fs::write(repo.join("src/lib.rs"), "pub fn retry() {}\n").expect("source");
    git(&repo, &["add", ".gitignore", "src/lib.rs"]);
    git(&repo, &["commit", "-qm", "seed"]);
    let (parent, worktree) = live_run(&runtime, &repo, "T-EXPLORE-1");
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .expect("test binary")
            .to_string_lossy()
            .into_owned(),
        "--list".into(),
    ]);
    Fixture {
        _root: root,
        runtime,
        repo,
        parent,
        worktree,
    }
}

/// A running delivery of `task_id` with its worktree checked out.
fn live_run(runtime: &OrbitRuntime, repo: &Path, task_id: &str) -> (JobRun, PathBuf) {
    let run = jobs(runtime)
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": [task_id], "resource_class": "orbit-hard" })),
            None,
        )
        .expect("insert implement run");
    jobs(runtime)
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start implement run");
    let worktree = orbit_engine::run_worktree_paths(repo, &run).expect("worktree path")[0].clone();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().expect("utf-8 worktree"),
        ],
    );
    (run, worktree)
}

/// Present this process as the MCP server of `run_id`'s `activity` agent.
fn as_managed_agent(run_id: &str, activity: &str) {
    // SAFETY: each row of this table runs alone in its own child process with
    // one test thread, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("ORBIT_MANAGED_RUN_CONTEXT", "1");
        std::env::set_var("ORBIT_RUN_ID", run_id);
        std::env::set_var("ORBIT_ACTIVITY_NAME", activity);
    }
}

fn agent_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Agent].into_iter().collect(),
        ..ToolSessionContext::default()
    }
}

fn ask<'a>(cwd: &'a str, session: &'a ToolSessionContext) -> AgentInvokeRequest<'a> {
    AgentInvokeRequest {
        prompt: "where is the retry budget enforced",
        cwd,
        crew: None,
        timeout_seconds: None,
        idempotency_key: None,
        provider_sandbox: None,
        actor: Some("claude-opus-4-6"),
        session_context: session,
    }
}

fn explore(runtime: &OrbitRuntime, request: AgentInvokeRequest<'_>) -> ExploreSubmission {
    match runtime
        .submit_agent_invocation(request)
        .expect("an eligible implement run may delegate")
    {
        AgentInvocation::Explore(submission) => submission,
        AgentInvocation::TrustedHost(_) => {
            panic!("a managed run must never receive a trusted-host invocation")
        }
    }
}

fn refusal(runtime: &OrbitRuntime, request: AgentInvokeRequest<'_>) -> OrbitError {
    match runtime.submit_agent_invocation(request) {
        Ok(_) => panic!("the delegation must be refused"),
        Err(error) => error,
    }
}

fn explorer_count(runtime: &OrbitRuntime) -> usize {
    runtime
        .list_job_runs(orbit_core::application::job::JobRunListParams::default())
        .expect("list runs")
        .iter()
        .filter(|run| run.job_id == EXPLORE_JOB_ID)
        .count()
}

fn finish(runtime: &OrbitRuntime, run_id: &str) {
    jobs(runtime)
        .mark_job_run_running(run_id, Utc::now(), std::process::id())
        .expect("start explorer");
    jobs(runtime)
        .finalize_job_run(run_id, JobRunState::Success, Utc::now(), Some(10))
        .expect("finish explorer");
}

fn input_of(runtime: &OrbitRuntime, run_id: &str) -> Value {
    jobs(runtime)
        .get_job_run(run_id)
        .expect("read explorer")
        .expect("explorer exists")
        .input
        .expect("explorer input")
}

// ------------------------------------------------------------------ admission

fn an_implement_run_delegates_to_a_sandboxed_pool_explorer_in_its_worktree() {
    let fx = fixture(BUDGETS);
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.join("src");
    let cwd = cwd.display().to_string();

    let submission = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("retry-budget"),
            ..ask(&cwd, &session)
        },
    );
    assert_eq!(submission.job_id, EXPLORE_JOB_ID);
    assert_eq!(
        submission.crew, "haiku",
        "the crew comes from the explorer pool"
    );
    assert_eq!(submission.parent_run_id, fx.parent.run_id);
    assert_eq!(
        submission.timeout_seconds, 300,
        "the setting is the default"
    );
    assert_eq!((submission.calls_used, submission.calls_allowed), (1, 2));
    let worktree = fx.worktree.canonicalize().expect("canonical worktree");
    assert_eq!(PathBuf::from(&submission.workspace_path), worktree);
    assert_eq!(PathBuf::from(&submission.cwd), worktree.join("src"));

    let input = input_of(&fx.runtime, &submission.run_id);
    assert!(
        input.get(TRUSTED_HOST_ADMISSION_KEY).is_none(),
        "an explorer must never carry a trusted-host admission: that would run it outside \
         the executor sandbox"
    );
    assert!(
        input.get("task_id").is_none() && input.get("task_ids").is_none(),
        "an explorer owns no task it could change"
    );
    assert_eq!(input["parent_run_id"], json!(fx.parent.run_id));
    assert_eq!(input["parent_task_id"], json!("T-EXPLORE-1"));
    assert_eq!(input["parent_activity"], json!("agent_implement"));
    assert_eq!(
        input["resource_class"],
        json!("orbit-hard"),
        "the explorer's minutes count in its caller's work class"
    );

    // A retried call resolves the same explorer and spends no budget.
    let retried = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("retry-budget"),
            ..ask(&cwd, &session)
        },
    );
    assert!(retried.deduplicated);
    assert_eq!(retried.run_id, submission.run_id);

    // The tool itself, through the governed chokepoint the agent's MCP server
    // uses: the operator-only row does not stop a managed caller, whose call
    // reaches the explorer admission and never the trusted-host one.
    let response = fx
        .runtime
        .run_tool(
            "orbit.agent.invoke",
            json!({ "prompt": "where is the retry budget enforced", "cwd": cwd,
                    "idempotency_key": "retry-budget" }),
        )
        .expect("a managed implement agent may call the tool");
    assert_eq!(response["run_id"], json!(submission.run_id));
    assert_eq!(
        (&response["sandboxed"], &response["read_only"]),
        (&json!(true), &json!(true))
    );
    assert!(
        response.get("authorized_by").is_none(),
        "no operator authorized an explorer"
    );
    assert_eq!(explorer_count(&fx.runtime), 1);

    let listed = fx
        .runtime
        .explore_invocation_summaries(&fx.parent.run_id)
        .expect("run detail lists the caller's explorers");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].run_id, submission.run_id);
    assert_eq!(listed[0].crew.as_deref(), Some("haiku"));
    assert_eq!(
        listed[0].parent_activity.as_deref(),
        Some("agent_implement")
    );
}

fn an_out_of_pool_crew_and_a_foreign_cwd_are_refused() {
    let fx = fixture(BUDGETS);
    let (other, other_worktree) = live_run(&fx.runtime, &fx.repo, "T-EXPLORE-2");
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let own = fx.worktree.display().to_string();

    let error = refusal(
        &fx.runtime,
        AgentInvokeRequest {
            crew: Some("opus"),
            ..ask(&own, &session)
        },
    );
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message)
            if message.contains("workflow.purpose_crews.explorer")),
        "an expensive crew outside the explorer pool is refused: {error:?}"
    );

    for foreign in [
        other_worktree.display().to_string(),
        fx.repo.display().to_string(),
        fx.repo.join("src").display().to_string(),
    ] {
        let error = refusal(&fx.runtime, ask(&foreign, &session));
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)),
            "{foreign}: an explorer reads only its caller's own worktree, got {error:?}"
        );
    }
    let error = refusal(
        &fx.runtime,
        AgentInvokeRequest {
            provider_sandbox: Some("danger-full-access"),
            ..ask(&own, &session)
        },
    );
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "an explorer cannot widen its provider sandbox: {error:?}"
    );
    assert_eq!(explorer_count(&fx.runtime), 0, "a refusal leaves no run");
    let _ = other;
}

fn explorer_budgets_refuse_with_the_setting_named() {
    let fx = fixture(BUDGETS);
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();
    let denied_naming = |error: OrbitError, key: &str| match error {
        OrbitError::CapabilityDenied(message) => assert!(
            message.contains(key),
            "the refusal must name `{key}` so the agent knows why: {message}"
        ),
        other => panic!("expected a budget refusal naming {key}, got {other:?}"),
    };

    let error = refusal(
        &fx.runtime,
        AgentInvokeRequest {
            timeout_seconds: Some(301),
            ..ask(&cwd, &session)
        },
    );
    assert!(
        matches!(&error, OrbitError::InvalidInput(message)
            if message.contains("workflow.explore.timeout_seconds")),
        "a timeout above the ceiling is refused: {error:?}"
    );

    let first = explore(&fx.runtime, ask(&cwd, &session));
    denied_naming(
        refusal(&fx.runtime, ask(&cwd, &session)),
        "workflow.explore.max_concurrent_per_run",
    );
    finish(&fx.runtime, &first.run_id);
    let second = explore(&fx.runtime, ask(&cwd, &session));
    assert_eq!(second.calls_used, 2);
    finish(&fx.runtime, &second.run_id);
    denied_naming(
        refusal(&fx.runtime, ask(&cwd, &session)),
        "workflow.explore.max_calls_per_run",
    );
    assert_eq!(explorer_count(&fx.runtime), 2);
}

fn the_switch_off_refuses_every_caller() {
    let fx = fixture("[workflow.explore]\nenabled = false\n");
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();
    let error = refusal(&fx.runtime, ask(&cwd, &session));
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message)
            if message.contains("workflow.explore.enabled")),
        "with exploration off the caller is told so and continues: {error:?}"
    );
    assert_eq!(explorer_count(&fx.runtime), 0);
}

fn other_agents_and_explorers_themselves_are_refused() {
    let fx = fixture(BUDGETS);
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();

    // A managed run of any other activity, such as a reviewer, is refused.
    as_managed_agent(&fx.parent.run_id, "agent_review");
    assert!(matches!(
        refusal(&fx.runtime, ask(&cwd, &session)),
        OrbitError::CapabilityDenied(_)
    ));

    // An explorer cannot invoke another agent.
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let explorer = explore(&fx.runtime, ask(&cwd, &session));
    as_managed_agent(&explorer.run_id, "agent_implement");
    let error = refusal(&fx.runtime, ask(&cwd, &session));
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message) if message.contains("another agent")),
        "explorers never nest: {error:?}"
    );

    // A finished run delegates nothing.
    jobs(&fx.runtime)
        .finalize_job_run(
            &fx.parent.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(10),
        )
        .expect("finish implement run");
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    assert!(matches!(
        refusal(&fx.runtime, ask(&cwd, &session)),
        OrbitError::CapabilityDenied(_)
    ));
    assert_eq!(explorer_count(&fx.runtime), 1);
}

fn the_operator_path_is_unchanged_by_exploration() {
    // With exploration on, an operator still gets its trusted-host invocation
    // and an unmanaged agent session is still refused.
    let fx = fixture(BUDGETS);
    let cwd = fx.repo.display().to_string();
    let error = fx
        .runtime
        .submit_agent_invocation(ask(&cwd, &agent_session()))
        .expect_err("an unmanaged agent never invokes an agent");
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message) if message.contains("orbit.agent.invoke")),
        "{error:?}"
    );
    let operator = ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into_iter().collect(),
        ..ToolSessionContext::default()
    };
    match fx
        .runtime
        .submit_agent_invocation(ask(&cwd, &operator))
        .expect("an operator invocation is admitted")
    {
        AgentInvocation::TrustedHost(submission) => {
            assert_eq!(
                submission.job_id,
                orbit_core::application::job::AGENT_INVOKE_JOB_ID
            );
            let input = input_of(&fx.runtime, &submission.run_id);
            assert!(
                input.get(TRUSTED_HOST_ADMISSION_KEY).is_some(),
                "the operator path keeps its per-invocation admission"
            );
        }
        AgentInvocation::Explore(_) => panic!("an operator never gets an explorer"),
    }
    assert_eq!(explorer_count(&fx.runtime), 0);
}

// ---------------------------------------------------------------- denied write

/// The explorer's write boundary is the `fsProfile` its shipped activity
/// declares. Resolved for the caller's worktree, it must refuse every write
/// there that the implementer's own profile grants.
#[cfg(target_os = "linux")]
fn the_explorer_sandbox_denies_writes_the_implementer_may_make() {
    use orbit_engine::RuntimeHost;
    use orbit_types::workflow::ExecutorSandboxKind;

    let fx = fixture(BUDGETS);
    let shipped: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities/agent_explore.yaml"),
        )
        .expect("read the shipped explorer activity"),
    )
    .expect("parse the shipped explorer activity");
    let profile = shipped["spec"]["fsProfile"]
        .as_str()
        .expect("the explorer declares its filesystem profile");
    super::engine_support::seed_executor(
        &fx.runtime,
        "claude",
        Some(ExecutorSandboxKind::LinuxBwrap),
    );
    let worktree = fx.worktree.canonicalize().expect("canonical worktree");
    let source = worktree.join("src/lib.rs");
    let writes = |profile: Option<&str>| {
        let resolved = fx
            .runtime
            .resolve_executor_sandbox("claude", profile, Some(&worktree))
            .expect("resolve sandbox")
            .expect("a sandboxed executor resolves a profile");
        orbit_exec::linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &source)
            .expect("diagnose write")
            .is_none()
    };
    assert!(
        !writes(Some(profile)),
        "the explorer's `{profile}` profile must deny writes in its caller's worktree: an \
         explorer that can edit would bypass the implementer's review and validation"
    );
    assert!(
        writes(None),
        "the implementer's own profile may write its worktree"
    );
}

// ------------------------------------------------------------------ analytics

fn implement_invocation(runtime: &OrbitRuntime, run_id: &str, activity: &str, ms: u64, usd: f64) {
    runtime
        .insert_invocation_trace_record(&orbit_store::contracts::InvocationInsertParams {
            job_run_id: run_id.to_string(),
            activity_id: activity.to_string(),
            agent: "claude".to_string(),
            provider: None,
            model: Some("claude-opus-4-6".to_string()),
            task_ids: Vec::new(),
            trace: orbit_types::telemetry::InvocationTrace {
                duration_ms: ms,
                provider_cost_usd: Some(usd),
                ..Default::default()
            },
        })
        .expect("record invocation");
}

fn cost_analytics_compare_runs_with_and_without_explorers() {
    let fx = fixture(BUDGETS);
    let task = fx
        .runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Explore analytics fixture".to_string(),
            ..Default::default()
        })
        .expect("create task")
        .id;
    let (plain, _) = live_run(&fx.runtime, &fx.repo, &task);
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();
    let explorer = explore(&fx.runtime, ask(&cwd, &session));

    implement_invocation(
        &fx.runtime,
        &fx.parent.run_id,
        "agent_implement",
        60_000,
        2.0,
    );
    implement_invocation(&fx.runtime, &plain.run_id, "agent_implement", 180_000, 4.0);
    implement_invocation(&fx.runtime, &plain.run_id, "agent_implement", 60_000, 1.0);
    implement_invocation(&fx.runtime, &explorer.run_id, "agent_explore", 5_000, 0.25);

    let comparison = fx.runtime.explore_comparison(None).expect("compare");
    assert_eq!(
        comparison.runs_scanned, 2,
        "explorer invocations are not implement runs"
    );
    let [crew] = comparison.crews.as_slice() else {
        panic!("one implementing crew: {comparison:?}");
    };
    assert_eq!(crew.crew, "claude/claude-opus-4-6");
    let with = &crew.with_explore;
    assert_eq!((with.runs, with.explore_calls), (1, 1));
    assert_eq!(with.mean_duration_ms, Some(60_000));
    assert_eq!(
        (with.mean_cost_usd, with.priced_runs),
        (Some(2.0), 1),
        "implementing cost excludes the explorer's own spend"
    );
    assert_eq!(with.explore_cost_usd, 0.25);
    assert_eq!(
        with.footprint_known_runs, 0,
        "a run whose task history is unreadable is no footprint sample"
    );
    let without = &crew.without_explore;
    assert_eq!((without.runs, without.explore_calls), (1, 0));
    assert_eq!(
        without.mean_duration_ms,
        Some(240_000),
        "a run's invocations add up"
    );
    assert_eq!(without.mean_cost_usd, Some(5.0));
    assert_eq!(
        (without.footprint_known_runs, without.footprint_miss_runs),
        (1, 0),
        "a task whose run never widened its footprint is a known non-miss"
    );
}

crate::scenario_table!(
    agent_explore_admission_scenarios,
    an_implement_run_delegates_to_a_sandboxed_pool_explorer_in_its_worktree,
    an_out_of_pool_crew_and_a_foreign_cwd_are_refused,
    explorer_budgets_refuse_with_the_setting_named,
    the_switch_off_refuses_every_caller,
    other_agents_and_explorers_themselves_are_refused,
    the_operator_path_is_unchanged_by_exploration,
    cost_analytics_compare_runs_with_and_without_explorers
);

#[cfg(target_os = "linux")]
crate::scenario_table!(
    agent_explore_sandbox_scenarios,
    the_explorer_sandbox_denies_writes_the_implementer_may_make
);
