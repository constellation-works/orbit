//! Executable catalog contracts: crew routing, policy, schema, and seeding.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_policy::PolicyEngine;
use orbit_tools::{ToolContext, ToolRegistry};
use orbit_types::policy::{FsProfile, PolicyDef};
use orbit_types::workflow::ActivityV2Spec;
use orbit_types::workflow::activity_job::JobV2StepBody;
use serde_json::json;
use tempfile::tempdir;

use crate::runtime::assets::{DEFAULT_ACTIVITY_FILES, DEFAULT_JOB_FILES};

/// [ORB-12103] Recovery once "repaired" a failed push by adding an `origin`
/// that pointed at the primary checkout a linked worktree shares its
/// `.git/config` with, turning a failed publication green. The boundary is
/// now both stated in the contract and enforced by the only subprocess
/// surface the activity has: `proc.spawn` refuses a `git` invocation that
/// would write persistent repository configuration.
#[test]
fn step_failure_recovery_cannot_write_persistent_git_configuration() {
    let (_, yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "step_failure_recovery")
        .expect("step failure recovery activity is seeded");
    let asset = load_activity_asset(yaml).expect("parse step failure recovery activity");
    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("expected agent_loop activity");
    };
    for stated in [
        "Repository configuration is out of bounds.",
        "Manufacturing a step's precondition is not recovery",
        "report `recovered: false`",
    ] {
        assert!(
            spec.instruction.contains(stated),
            "[ORB-12103] recovery contract must state `{stated}`"
        );
    }
    let programs = spec.proc_disallowed_programs.clone().unwrap_or_default();
    assert!(
        !programs.iter().any(|program| program == "git"),
        "recovery still inspects and delivers with git"
    );

    let repo = tempdir().expect("create tempdir");
    let repo_path = repo.path().to_string_lossy().into_owned();
    run_git(repo.path(), &["init"]).expect("git init");
    let mut ctx = recovery_tool_context(repo.path(), Vec::new());
    ctx.proc_disallowed_programs = Some(programs);
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let denial = registry
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "git",
                "args": ["-C", repo_path, "remote", "add", "origin", repo_path],
            }),
        )
        .expect_err("recovery must not be able to write a git remote");
    assert!(
        matches!(&denial, OrbitError::PolicyDenied(message)
            if message.contains("persistent repository configuration")),
        "expected the configuration boundary to refuse the call, got: {denial}"
    );
    assert_eq!(
        run_git(repo.path(), &["remote"]).expect("list remotes"),
        "",
        "the refused call must leave repository configuration untouched"
    );

    // The refusal is specific to configuration writes, not to git itself:
    // the same context still reaches the boundary for inspection. (The child
    // can fail for host reasons — no Landlock, no git — so only the
    // configuration refusal itself is asserted against.)
    if let Err(error) = registry.execute(
        "proc.spawn",
        &ctx,
        json!({ "program": "git", "args": ["remote", "-v"] }),
    ) {
        assert!(
            !error
                .to_string()
                .contains("persistent repository configuration"),
            "read-only git inspection must not hit the configuration boundary: {error}"
        );
    }
}

/// [ORB-13915] An owner delivery merged a candidate whose only validation
/// evidence was the implementer's own report; it failed `cargo fmt --check`
/// and turned the integration branch red. Each owner delivery job must run
/// `candidate_validate` on the candidate after its last rewrite and before it
/// leaves the worktree, under step recovery so a repairable failure is
/// repaired and retried rather than terminal.
#[test]
fn owner_delivery_jobs_validate_the_candidate_before_it_leaves_the_worktree() {
    for (job, last_rewrite, publication) in [
        ("task_pr_pipeline", "sync_base", "push"),
        ("task_local_pipeline", "commit", "merge"),
    ] {
        let (_, yaml) = DEFAULT_JOB_FILES
            .iter()
            .find(|(name, _)| *name == job)
            .unwrap_or_else(|| panic!("{job} is seeded"));
        let steps = load_job_asset(yaml)
            .unwrap_or_else(|error| panic!("parse {job}: {error}"))
            .spec
            .steps;
        let position = |id: &str| {
            steps
                .iter()
                .position(|step| step.id == id)
                .unwrap_or_else(|| panic!("{job} has a {id} step"))
        };
        let validate = &steps[position("validate")];
        assert!(
            matches!(&validate.body, JobV2StepBody::TargetRef(target)
                if target.target == "activity:candidate_validate"),
            "{job}: validate runs the deterministic candidate_validate activity"
        );
        assert_eq!(
            validate.recovery_activity.as_deref(),
            Some("step_failure_recovery"),
            "{job}: a failed validation is repairable"
        );
        assert!(
            position(last_rewrite) < position("validate")
                && position("validate") < position(publication),
            "{job}: validation runs after {last_rewrite} and before {publication}"
        );
    }
}

/// The tool context `step_failure_recovery` runs with: activity-scoped, its
/// own program allowlist, and workspace-wide filesystem access.
fn recovery_tool_context(workspace_root: &Path, programs: Vec<String>) -> ToolContext {
    let mut fs_profiles = HashMap::new();
    fs_profiles.insert(
        "implementer".to_string(),
        FsProfile {
            read: vec!["./**".to_string()],
            modify: vec!["./**".to_string()],
        },
    );
    let policy = PolicyDef {
        name: "test".to_string(),
        description: None,
        deny_read: Vec::new(),
        deny_modify: Vec::new(),
        fs_profiles,
        created_at: None,
        updated_at: None,
    };
    ToolContext {
        workspace_root: Some(workspace_root.to_path_buf()),
        policy_engine: Some(Arc::new(
            PolicyEngine::from_def(&policy).expect("policy engine"),
        )),
        fs_profile: Some("implementer".to_string()),
        proc_allowed_programs: programs,
        proc_spawn_activity_scoped: true,
        ..Default::default()
    }
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String, OrbitError> {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|error| OrbitError::Execution(format!("git {args:?}: {error}")))?;
    if !output.status.success() {
        return Err(OrbitError::Execution(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
